<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import type { EditorView } from "@codemirror/view";
  import {
    createMergeSourceEditor,
    createMergedEditor,
    initialMergedHunkRanges,
    hunkIndexAtPos,
    buildHunkResolutionChange,
    mergeHunkDecorationsField,
    setMergeHunkDecorations,
    replaceHunkDecoration,
  } from "$lib/mergeView";
  import type { EditDelta } from "$lib/diffView";
  import type { MergeHunk, OpenMergeResult, Resolution, Settings } from "$lib/types";

  // M5's three-way merge view (docs/PLAN.md's BASE/LOCAL/REMOTE + merged-output view), launched
  // via `diffgrid --merge BASE LOCAL REMOTE MERGED` (matching git's own mergetool positional
  // convention -- see e.g. /usr/lib/git-core/mergetools/vimdiff's own comment: a configured
  // `mergetool.<name>.cmd` is invoked with $BASE/$LOCAL/$REMOTE/$MERGED substituted in that
  // order) and rendered as this window's whole content -- not a secondary window like
  // settings/, since `git mergetool` invokes one process per conflict and the merge view *is*
  // that process's job, not an auxiliary feature of a session doing something else.
  //
  // MERGED is a genuinely distinct path from LOCAL (per git's convention MERGED often names a
  // temp file, not the real working-tree file) -- Save must write there, not to LOCAL, or the
  // real file git expects updated never gets touched. The 3-arg form (no MERGED) is kept as a
  // convenience for manual testing/demoing outside a real git invocation, defaulting MERGED to
  // LOCAL in that case only.
  let status = $state("loading…");
  let basePath = $state("");
  let localPath = $state("");
  let remotePath = $state("");
  let mergedPath = $state("");
  const tabId = "merge-1";

  // Mirrors Rust's tab.hunks -- kept in sync after every resolution change so the toolbar's
  // enabled/disabled state and the unresolved-count readout stay correct without re-fetching.
  let hunks: MergeHunk[] = $state([]);
  let settings: Settings = {
    ignoreWhitespace: false,
    ignoreCase: false,
    collapseContextLines: 3,
    intraLineMode: "character",
    autoResolveNonConflicting: true,
    defaultTakeBothSide: "mineFirst",
  };

  // Which hunk is currently selected (by index into `hunks`) -- drives which resolution-action
  // buttons are enabled. -1 means nothing selected yet.
  let selectedHunk = $state(-1);
  const unresolvedCount = $derived(hunks.filter((h) => h.resolution === null).length);

  let mergedView: EditorView;

  function statusLine(): string {
    return unresolvedCount > 0 ? `${unresolvedCount} conflict${unresolvedCount === 1 ? "" : "s"} unresolved` : "ready — all hunks resolved";
  }

  function selectHunkFromView(view: EditorView) {
    const decorations = view.state.field(mergeHunkDecorationsField);
    const found = hunkIndexAtPos(decorations, view.state.selection.main.head);
    if (found !== null) selectedHunk = found;
  }

  async function openMerge(base: string, local: string, remote: string, merged: string) {
    basePath = base;
    localPath = local;
    remotePath = remote;
    mergedPath = merged;
    status = "merging…";
    settings = await invoke<Settings>("load_settings");
    const result = await invoke<OpenMergeResult>("open_merge", {
      tabId,
      base,
      local,
      remote,
      takeBothSide: settings.defaultTakeBothSide,
    });
    hunks = result.hunks;
    const initialRanges = initialMergedHunkRanges(result.baseText, result.localText, result.remoteText, hunks, settings.defaultTakeBothSide);

    await tick();
    const baseEl = document.getElementById("merge-base")!;
    const localEl = document.getElementById("merge-local")!;
    const remoteEl = document.getElementById("merge-remote")!;
    const mergedEl = document.getElementById("merge-merged")!;

    const baseView = createMergeSourceEditor(baseEl, result.baseText, hunks, "base");
    const localView = createMergeSourceEditor(localEl, result.localText, hunks, "local");
    const remoteView = createMergeSourceEditor(remoteEl, result.remoteText, hunks, "remote");
    mergedView = createMergedEditor(mergedEl, result.mergedText, initialRanges, hunks, onMergedEdit);

    baseEl.addEventListener("click", () => selectHunkFromView(baseView));
    localEl.addEventListener("click", () => selectHunkFromView(localView));
    remoteEl.addEventListener("click", () => selectHunkFromView(remoteView));
    mergedEl.addEventListener("click", () => selectHunkFromView(mergedView));

    status = statusLine();
  }

  /** Fires for every change to the merged pane -- a real keystroke *and* a resolution-action
   * click's own programmatic replace alike (both go through the same CM6 transaction path).
   * Only a real keystroke needs `mark_merge_hunk_manual`; a resolution click already set a
   * non-Manual resolution via `resolve_merge_hunk` before dispatching its change, so
   * `pendingProgrammaticEdit` suppresses the Manual-marking for that one case. */
  let pendingProgrammaticEdit = false;

  function onMergedEdit(deltas: EditDelta[]) {
    for (const delta of deltas) {
      invoke("apply_merge_edit", { tabId, fromUtf16: delta.fromUtf16, toUtf16: delta.toUtf16, inserted: delta.inserted }).catch((err) => {
        invoke("report_error", { message: `apply_merge_edit: ${err}` });
      });
    }
    if (pendingProgrammaticEdit) return;
    if (selectedHunk >= 0 && hunks[selectedHunk]) {
      invoke("mark_merge_hunk_manual", { tabId, hunkIndex: selectedHunk }).catch((err) => invoke("report_error", { message: `mark_merge_hunk_manual: ${err}` }));
      hunks[selectedHunk] = { ...hunks[selectedHunk], resolution: "manual" };
      status = statusLine();
    }
  }

  async function resolveSelected(resolution: Resolution) {
    if (selectedHunk < 0 || !hunks[selectedHunk]) return;
    const index = selectedHunk;
    const text = await invoke<string>("resolve_merge_hunk", {
      tabId,
      hunkIndex: index,
      resolution,
      takeBothSide: settings.defaultTakeBothSide,
    });
    const updatedHunk = { ...hunks[index], resolution };
    const decorationsBefore = mergedView.state.field(mergeHunkDecorationsField);
    const change = buildHunkResolutionChange(decorationsBefore, mergedView.state.doc, index, text);
    if (change) {
      pendingProgrammaticEdit = true;
      // Dispatched as one transaction: the doc change (which RangeSet.map stretches every
      // hunk's decoration, including this one, to cover) plus an explicit restyle of just this
      // hunk's decoration -- computed against decorationsBefore since setMergeHunkDecorations's
      // effect value is applied by mergeHunkDecorationsField.update in the SAME transaction,
      // replacing whatever value.map(tr.changes) would have produced.
      const mappedDecorations = decorationsBefore.map(mergedView.state.changes({ from: change.from, to: change.to, insert: change.insert }));
      const restyled = replaceHunkDecoration(mappedDecorations, index, updatedHunk);
      mergedView.dispatch({ changes: { from: change.from, to: change.to, insert: change.insert }, effects: setMergeHunkDecorations.of(restyled) });
      pendingProgrammaticEdit = false;
    }
    hunks[index] = updatedHunk;
    status = statusLine();
  }

  /**
   * Writes the merged result to $MERGED and exits -- per `git mergetool`'s own contract (see
   * this file's top-of-script comment), exit 0 only when every hunk actually has a resolution;
   * a save with unresolved conflicts still writes the file (so the placeholder/partial content
   * is visible for a later manual re-run) but exits non-zero, since git only trusts this
   * process's exit code at all when the user has configured
   * `mergetool.diffgrid.trustExitCode = true` -- silently exiting 0 with real conflicts left
   * unresolved would make git record an incorrect "successful" merge for that configuration.
   */
  async function saveMerge() {
    const allResolved = await invoke<boolean>("save_merge", { tabId, path: mergedPath });
    status = allResolved ? "saved — all conflicts resolved" : "saved — unresolved conflicts remain";
    await invoke("exit_process", { exitCode: allResolved ? 0 : 1 });
  }

  /** Quits without saving -- an aborted merge, per `git mergetool`'s convention that a non-zero
   * exit (again, only trusted under `trustExitCode = true`) means the merge did not succeed. */
  async function abortMerge() {
    await invoke("exit_process", { exitCode: 1 });
  }

  onMount(async () => {
    window.addEventListener("error", (e) => invoke("report_error", { message: `merge window.onerror: ${e.message}` }));
    window.addEventListener("unhandledrejection", (e) => invoke("report_error", { message: `merge unhandledrejection: ${String(e.reason)}` }));
    const args = await invoke<string[]>("launch_args");
    const mergeIndex = args.indexOf("--merge");
    if (mergeIndex !== -1 && args.length >= mergeIndex + 3) {
      const base = args[mergeIndex + 1];
      const local = args[mergeIndex + 2];
      const remote = args[mergeIndex + 3];
      // MERGED (a 4th, distinct path per git's own convention) defaults to LOCAL only for the
      // 3-arg manual-testing convenience form -- see this file's top-of-script comment.
      const merged = args.length >= mergeIndex + 5 ? args[mergeIndex + 4] : local;
      await openMerge(base, local, remote, merged);
    } else {
      status = "no --merge BASE LOCAL REMOTE [MERGED] arguments given";
    }
  });
</script>

<main>
  <div class="status">
    <span>{status}</span>
    <div class="status-actions">
      <button onclick={abortMerge}>Abort</button>
      <button onclick={saveMerge}>Save</button>
    </div>
  </div>
  <div class="sources">
    <div class="pane">
      <div class="pane-label">BASE — {basePath}</div>
      <div id="merge-base" class="editor"></div>
    </div>
    <div class="pane">
      <div class="pane-label">LOCAL — {localPath}</div>
      <div id="merge-local" class="editor"></div>
    </div>
    <div class="pane">
      <div class="pane-label">REMOTE — {remotePath}</div>
      <div id="merge-remote" class="editor"></div>
    </div>
  </div>
  <div class="toolbar">
    <button onclick={() => resolveSelected("takeLocal")} disabled={selectedHunk < 0}>Take local</button>
    <button onclick={() => resolveSelected("takeRemote")} disabled={selectedHunk < 0}>Take remote</button>
    <button onclick={() => resolveSelected("takeBoth")} disabled={selectedHunk < 0}>Take both</button>
    <button onclick={() => resolveSelected("takeBase")} disabled={selectedHunk < 0}>Take base</button>
  </div>
  <div class="pane merged">
    <div class="pane-label">MERGED</div>
    <div id="merge-merged" class="editor"></div>
  </div>
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  .status {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px 8px;
    font-size: 12px;
    background: #222;
    color: #eee;
  }
  .status-actions {
    display: flex;
    gap: 6px;
  }
  .sources {
    display: flex;
    flex: 1 1 40%;
    min-height: 0;
  }
  .pane {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    border-right: 1px solid #ddd;
  }
  .pane-label {
    font-size: 11px;
    padding: 2px 6px;
    background: #f0f0f0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .editor {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .toolbar {
    display: flex;
    gap: 8px;
    padding: 4px 8px;
  }
  .merged {
    flex: 1 1 40%;
    min-height: 0;
  }
  :global(.merge-hunk-conflict) {
    background: #ffc9c9;
  }
  :global(.merge-hunk-autoMerged) {
    background: #bfe0ff;
  }
  :global(.merge-hunk-resolved-manual) {
    background: #ffe49c;
  }
</style>
