<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { Text } from "@codemirror/state";
  import type { EditorView } from "@codemirror/view";
  import {
    createDiffEditor,
    syncScroll,
    updateHunks,
    runScrollBenchmark,
    changeHunkLines,
    changeHunks,
    nextHunkIndex,
    prevHunkIndex,
    scrollToLine,
    computeMinimapSegments,
    computeViewportIndicator,
    minimapClickToLine,
    buildHunkCopyChange,
    type ChangeHunkLine,
    type MinimapSegment,
    type ViewportIndicator,
    type EditDelta,
  } from "$lib/diffView";
  import type { FileDiffResult, Hunk, OpenPairResult, Span } from "$lib/types";

  const FIXTURE = "100k-line-pair";
  const SCROLL_BENCH_DELAY_MS = 2000;
  const SCROLL_BENCH_DURATION_MS = 4000;
  // M2: how long to wait after the last keystroke on either pane before re-diffing. Per
  // docs/PLAN.md §2 this must be debounced, not per-keystroke -- a live re-diff on every
  // character would mean a Rust round-trip (plus a full histogram diff) on every keystroke,
  // which is exactly the per-keystroke cost the delta pipeline exists to avoid elsewhere.
  const EDIT_REDIFF_DEBOUNCE_MS = 300;

  let status = $state("loading…");
  let statLine = $state("");
  let isRealFileMode = $state(false);
  let ignoreWhitespace = $state(false);
  let ignoreCase = $state(false);
  let dirtyLeft = $state(false);
  let dirtyRight = $state(false);
  let savingLeft = $state(false);
  let savingRight = $state(false);
  // The paths runRealFiles opened -- save_file needs these to know where to write back to,
  // since EditBuffer only holds the bytes/text, not the path it came from (that's the
  // frontend's concern, same as it already is for open_file_pair/open_file_text).
  let leftPath = "";
  let rightPath = "";

  // Populated by runRealFiles; read by retoggleDiffOptions and hunk navigation. leftView/
  // rightView aren't read in the template, so plain variables suffice; changeLines and
  // currentHunk are, so they need $state for the toolbar to update.
  let leftView: EditorView | undefined;
  let rightView: EditorView | undefined;
  let changeLines: ChangeHunkLine[] = $state([]);
  // Parallel to changeLines (same filter, same order -- see changeHunks), kept separately
  // because copyCurrentHunk needs the full Hunk (kind + both LineRanges), which
  // ChangeHunkLine's two bare line numbers don't carry.
  let currentChangeHunks: Hunk[] = [];
  let currentHunk = $state(-1);
  let minimapSegments: MinimapSegment[] = $state([]);
  let viewportIndicator: ViewportIndicator = $state({ topFrac: 0, heightFrac: 1 });
  let totalLines = 0;

  // M2's edit pipeline: apply_edit calls for a given side must land at Rust in the exact order
  // CM6 produced them (each one's offsets are only valid against the buffer state the previous
  // one left behind) even though `invoke` is async and its IPC round-trip could otherwise let
  // calls resolve out of order. Chaining each call onto a per-side promise means the next one
  // is only *issued* once the previous one's response has come back, without blocking typing
  // itself (nothing here awaits these chains except the debounced re-diff, below).
  let editQueueLeft: Promise<void> = Promise.resolve();
  let editQueueRight: Promise<void> = Promise.resolve();
  let redoDiffTimer: ReturnType<typeof setTimeout> | undefined;

  function applyNewHunks(hunks: FileDiffResult["hunks"]) {
    if (!leftView || !rightView) return;
    updateHunks(leftView, hunks);
    updateHunks(rightView, hunks);
    changeLines = changeHunkLines(hunks);
    currentChangeHunks = changeHunks(hunks);
    // Deliberately reset rather than trying to re-point at "the same" hunk post-copy: a copy
    // changes the hunk list (the copied hunk usually disappears, and every hunk after it can
    // shift), the same as any other hunks-invalidating event (a toggle, another edit) already
    // does. Consistent behavior across all of those beats trying to preserve a selection that
    // may no longer refer to anything meaningful.
    currentHunk = -1;
    // Refreshed here, not just at open: an edit can add or remove lines, so a re-diff's hunk
    // list may no longer match the line count the minimap was last computed against.
    totalLines = leftView.state.doc.lines;
    minimapSegments = computeMinimapSegments(hunks, totalLines);
  }

  function updateViewportIndicator() {
    if (!leftView) return;
    const { scrollTop, scrollHeight, clientHeight } = leftView.scrollDOM;
    viewportIndicator = computeViewportIndicator(scrollTop, scrollHeight, clientHeight);
  }

  function onMinimapClick(e: MouseEvent) {
    if (!leftView || totalLines === 0) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    scrollToLine(leftView, minimapClickToLine((e.clientY - rect.top) / rect.height, totalLines));
  }

  /**
   * Re-diffs against the Rust-side `EditBuffer`s' *current* text (which reflects any edits
   * applied via `apply_edit` since open, not just the text as it was when the file was opened)
   * with the current whitespace/case-ignore toggles. Replaces M1's `diff_texts`, which sent
   * `leftText`/`rightText` from the frontend on every call -- once edits exist, holding a
   * frontend copy of "the text" at all just invites sending a stale one, so the toggle path and
   * the edit-re-diff path now share one mechanism with one source of truth.
   *
   * Awaits both edit queues first so a re-diff (whether from a toggle click or the debounced
   * timer below) never races an `apply_edit` call that's still in flight for the same edit.
   */
  async function flushAndRedoDiff(): Promise<FileDiffResult> {
    await Promise.all([editQueueLeft, editQueueRight]);
    const diff = await invoke<FileDiffResult>("redo_diff", { ignoreWhitespace, ignoreCase });
    applyNewHunks(diff.hunks);
    statLine = `+${diff.stats.added} -${diff.stats.removed} ${diff.stats.chunks} chunks`;
    return diff;
  }

  async function retoggleDiffOptions() {
    if (!leftView || !rightView) return;
    status = "re-diffing…";
    await flushAndRedoDiff();
    status = "ready";
  }

  function scheduleDebouncedRedoDiff() {
    if (redoDiffTimer !== undefined) clearTimeout(redoDiffTimer);
    redoDiffTimer = setTimeout(() => {
      redoDiffTimer = undefined;
      flushAndRedoDiff().catch((err) => invoke("report_error", { message: `redo_diff: ${err}` }));
    }, EDIT_REDIFF_DEBOUNCE_MS);
  }

  /**
   * The frontend -> Rust half of docs/PLAN.md §2's delta pipeline: forwards each delta CM6
   * captured to the matching `EditBuffer`, then (debounced) triggers a re-diff. `side` is fixed
   * per call site (see `runRealFiles`), not derived from the delta, since `EditDelta` itself
   * carries no side information -- it's purely a CM6-document-relative offset pair.
   */
  function onEdit(side: "left" | "right", deltas: EditDelta[]) {
    if (side === "left") dirtyLeft = true;
    else dirtyRight = true;
    for (const delta of deltas) {
      const send = (): Promise<void> =>
        invoke<void>("apply_edit", { side, fromUtf16: delta.fromUtf16, toUtf16: delta.toUtf16, inserted: delta.inserted }).catch(
          (err) => {
            invoke("report_error", { message: `apply_edit(${side}): ${err}` });
          },
        );
      if (side === "left") editQueueLeft = editQueueLeft.then(send);
      else editQueueRight = editQueueRight.then(send);
    }
    scheduleDebouncedRedoDiff();
  }

  /**
   * Flushes that side's pending edit queue first (so a save can never race an `apply_edit`
   * still in flight for the same side and write a half-updated buffer), then asks Rust to write
   * `EditBuffer::to_bytes()` back to the original path -- encoding/line-ending-preserving per
   * docs/PLAN.md §2. Errors (e.g. a Latin-1 buffer containing a character that encoding can't
   * represent, see `text_io::to_bytes`) are surfaced via `status` rather than silently dropped,
   * since a failed save leaving `dirty*` set is exactly the correct outcome -- the file on disk
   * genuinely doesn't match the buffer yet.
   */
  async function saveSide(side: "left" | "right") {
    const path = side === "left" ? leftPath : rightPath;
    if (!path) return;
    if (side === "left") savingLeft = true;
    else savingRight = true;
    try {
      await (side === "left" ? editQueueLeft : editQueueRight);
      await invoke("save_file", { side, path });
      if (side === "left") dirtyLeft = false;
      else dirtyRight = false;
    } catch (err) {
      status = `save failed (${side}): ${err}`;
      await invoke("report_error", { message: `save_file(${side}): ${err}` });
    } finally {
      if (side === "left") savingLeft = false;
      else savingRight = false;
    }
  }

  function goToHunk(direction: 1 | -1) {
    if (!leftView || changeLines.length === 0) return;
    currentHunk = direction === 1 ? nextHunkIndex(changeLines.length, currentHunk) : prevHunkIndex(changeLines.length, currentHunk);
    scrollToLine(leftView, changeLines[currentHunk].left);
  }

  /**
   * "Apply/revert individual hunks left↔right" (docs/PLAN.md M2), scoped to the
   * currently-navigated hunk via the existing Prev/Next diff controls rather than per-hunk
   * inline gutter buttons -- see DECISIONS.md. Dispatches the copy as a normal CM6 transaction
   * on the destination view, so it flows through the exact same onEdit → apply_edit →
   * debounced redo_diff pipeline a keystroke would, with no separate backend command.
   */
  function copyCurrentHunk(direction: "leftToRight" | "rightToLeft") {
    if (!leftView || !rightView || currentHunk === -1) return;
    const hunk = currentChangeHunks[currentHunk];
    const change = buildHunkCopyChange(hunk, direction, leftView.state.doc, rightView.state.doc);
    if (!change) return;
    const destView = change.destSide === "left" ? leftView : rightView;
    destView.dispatch({ changes: { from: change.from, to: change.to, insert: change.insert } });
  }

  function doubleRaf(): Promise<void> {
    return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  }

  onMount(async () => {
    window.addEventListener("error", (e) => {
      invoke("report_error", { message: `window.onerror: ${e.message}` });
    });
    window.addEventListener("unhandledrejection", (e) => {
      invoke("report_error", { message: `unhandledrejection: ${String(e.reason)}` });
    });
    window.addEventListener("keydown", (e) => {
      if (!isRealFileMode) return;
      if (e.altKey) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          goToHunk(1);
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          goToHunk(-1);
        }
        return;
      }
      // Cmd+S on macOS, Ctrl+S elsewhere -- saves whichever pane currently has focus.
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        if (leftView?.hasFocus) saveSide("left");
        else if (rightView?.hasFocus) saveSide("right");
      }
    });
    try {
      const args = await invoke<string[]>("launch_args");
      if (args.length === 2) {
        await runRealFiles(args[0], args[1]);
      } else {
        await runSpike();
      }
    } catch (e) {
      const msg = e instanceof Error ? `${e.message}\n${e.stack}` : String(e);
      status = `error: ${msg}`;
      await invoke("report_error", { message: msg });
    }
  });

  /**
   * M1/M2's real entry point: `diffgrid FILE1 FILE2`. Unlike `runSpike`, this never runs the
   * synthetic scroll benchmark or the `disablePadding` A/B toggle — those are M0
   * measurement-harness concerns, not part of the real application. M2 adds: both panes
   * editable, edits forwarded to Rust's `EditBuffer`s, debounced re-diff.
   */
  async function runRealFiles(left: string, right: string) {
    leftPath = left;
    rightPath = right;
    status = "diffing…";
    const [result, leftBuf, rightBuf] = await Promise.all([
      invoke<OpenPairResult>("open_file_pair", { left, right }),
      invoke<ArrayBuffer>("open_file_text", { path: left }),
      invoke<ArrayBuffer>("open_file_text", { path: right }),
    ]);

    const leftText = new TextDecoder().decode(leftBuf);
    const rightText = new TextDecoder().decode(rightBuf);
    // Only a fallback for the brief window during construction below where the *other* side's
    // EditorView doesn't exist yet (its own intra-line highlighter can synchronously schedule
    // its first fetch from inside `new EditorView(...)`, before this function has assigned
    // `rightView`/`leftView`) -- not used once both views exist, since `getOtherDoc` below reads
    // the live view's current document past that point, including edits.
    const leftDocAtOpen = Text.of(leftText.split("\n"));
    const rightDocAtOpen = Text.of(rightText.split("\n"));

    const fetchSpans = (leftLine: string, rightLine: string) =>
      invoke<Span[]>("intra_line_spans", { leftLine, rightLine, ignoreWhitespace, ignoreCase });
    const onFetchError = (message: string) => invoke("report_error", { message: `intra-line: ${message}` });

    status = "mounting editors…";
    const leftEl = document.getElementById("left-pane")!;
    const rightEl = document.getElementById("right-pane")!;
    leftView = createDiffEditor(
      leftEl,
      leftText,
      result.diff.hunks,
      "left",
      false,
      { getOtherDoc: () => (rightView ? rightView.state.doc : rightDocAtOpen), fetchSpans, onFetchError },
      true,
      true,
      (deltas) => onEdit("left", deltas),
    );
    rightView = createDiffEditor(
      rightEl,
      rightText,
      result.diff.hunks,
      "right",
      false,
      { getOtherDoc: () => (leftView ? leftView.state.doc : leftDocAtOpen), fetchSpans, onFetchError },
      true,
      true,
      (deltas) => onEdit("right", deltas),
    );
    syncScroll(leftView, rightView);
    isRealFileMode = true;
    // Sets changeLines/currentChangeHunks/currentHunk/minimapSegments/totalLines consistently
    // with every later re-diff, rather than duplicating that logic here -- a prior version of
    // this function set changeLines directly but never set currentChangeHunks, which stayed
    // stale ([]) until the first toggle/edit, making copyCurrentHunk crash on the very first
    // hunk-copy click before any other re-diff had run. Caught by manual testing under Xvfb,
    // not by any unit test (none of them exercise runRealFiles's real invoke() calls).
    applyNewHunks(result.diff.hunks);
    leftView.scrollDOM.addEventListener("scroll", updateViewportIndicator);
    updateViewportIndicator();

    statLine = `+${result.diff.stats.added} -${result.diff.stats.removed} ${result.diff.stats.chunks} chunks`;
    status = "ready";
    await invoke("report_ready");
  }

  async function runSpike() {
    const t0 = performance.now();

    const [flags, diff, leftBuf, rightBuf] = await Promise.all([
      invoke<{ disable_padding: boolean; collapse_equal: boolean }>("bench_flags"),
      invoke<FileDiffResult>("diff_fixture", { name: FIXTURE }),
      invoke<ArrayBuffer>("fixture_text", { name: FIXTURE, side: "left" }),
      invoke<ArrayBuffer>("fixture_text", { name: FIXTURE, side: "right" }),
    ]);

    const leftText = new TextDecoder().decode(leftBuf);
    const rightText = new TextDecoder().decode(rightBuf);

    status = "mounting editors…";
    const leftEl = document.getElementById("left-pane")!;
    const rightEl = document.getElementById("right-pane")!;
    const leftView = createDiffEditor(leftEl, leftText, diff.hunks, "left", flags.disable_padding, undefined, flags.collapse_equal);
    const rightView = createDiffEditor(rightEl, rightText, diff.hunks, "right", flags.disable_padding, undefined, flags.collapse_equal);
    syncScroll(leftView, rightView);

    await doubleRaf();
    const paintMs = performance.now() - t0;
    statLine = `+${diff.stats.added} -${diff.stats.removed} ${diff.stats.chunks} chunks · open-to-first-paint ${paintMs.toFixed(1)}ms`;
    status = "ready";
    await invoke("report_ready");

    setTimeout(async () => {
      status = "running scroll benchmark…";
      const stats = await runScrollBenchmark(leftView, SCROLL_BENCH_DURATION_MS);
      status = `scroll bench: ${stats.steadyEstimatedFps.toFixed(1)} fps (steady mean), first-scroll-frame ${stats.firstScrollFrameMs.toFixed(1)}ms`;
      await invoke("report_bench", {
        json: JSON.stringify({ paintMs, disablePadding: flags.disable_padding, collapseEqual: flags.collapse_equal, ...stats }),
      });
    }, SCROLL_BENCH_DELAY_MS);
  }
</script>

<main>
  <div class="status">{status}</div>
  <div class="stat">{statLine}</div>
  {#if isRealFileMode}
    <div class="toolbar">
      <button onclick={() => goToHunk(-1)} disabled={changeLines.length === 0}>&uarr; Prev diff</button>
      <button onclick={() => goToHunk(1)} disabled={changeLines.length === 0}>&darr; Next diff</button>
      <span class="hunk-count">{changeLines.length === 0 ? "no changes" : `${currentHunk + 1} / ${changeLines.length}`}</span>
      <button onclick={() => copyCurrentHunk("rightToLeft")} disabled={currentHunk === -1} title="Copy the current hunk's right-side version to the left">
        &larr; Copy to left
      </button>
      <button onclick={() => copyCurrentHunk("leftToRight")} disabled={currentHunk === -1} title="Copy the current hunk's left-side version to the right">
        Copy to right &rarr;
      </button>
      <label>
        <input type="checkbox" bind:checked={ignoreWhitespace} onchange={retoggleDiffOptions} />
        Ignore whitespace
      </label>
      <label>
        <input type="checkbox" bind:checked={ignoreCase} onchange={retoggleDiffOptions} />
        Ignore case
      </label>
      <button onclick={() => saveSide("left")} disabled={!dirtyLeft || savingLeft} title="Save left (Cmd/Ctrl+S while focused)">
        {dirtyLeft ? "● " : ""}Save left
      </button>
      <button onclick={() => saveSide("right")} disabled={!dirtyRight || savingRight} title="Save right (Cmd/Ctrl+S while focused)">
        {dirtyRight ? "● " : ""}Save right
      </button>
    </div>
  {/if}
  <div class="panes">
    <div id="left-pane" class="pane"></div>
    <div id="right-pane" class="pane"></div>
    {#if isRealFileMode}
      <!-- Supplementary pointing-device shortcut to the same navigation the Prev/Next diff
           buttons and Alt+Up/Down already provide with full keyboard access. A click here
           means "jump to the line at this Y position," which has no meaningful keyboard
           equivalent (unlike a real interactive control) -- deliberately not keyboard-operable
           itself, since the same destinations are already reachable by keyboard elsewhere. -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="minimap" onclick={onMinimapClick} title="Click to jump to a position in the file">
        {#each minimapSegments as seg}
          <div class="minimap-segment minimap-{seg.kind}" style="top: {seg.startFrac * 100}%; height: {seg.lenFrac * 100}%;"></div>
        {/each}
        <div class="minimap-viewport" style="top: {viewportIndicator.topFrac * 100}%; height: {viewportIndicator.heightFrac * 100}%;"></div>
      </div>
    {/if}
  </div>
</main>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    height: 100%;
  }
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    font-family: ui-monospace, Menlo, monospace;
  }
  .status,
  .stat {
    flex: 0 0 auto;
    padding: 4px 8px;
    font-size: 12px;
    background: #222;
    color: #ddd;
  }
  .toolbar {
    flex: 0 0 auto;
    display: flex;
    gap: 16px;
    padding: 4px 8px;
    font-size: 12px;
    background: #333;
    color: #ddd;
  }
  .toolbar label {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .toolbar button {
    font-size: 12px;
    background: #444;
    color: #ddd;
    border: 1px solid #555;
    border-radius: 3px;
    padding: 2px 6px;
    cursor: pointer;
  }
  .toolbar button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .hunk-count {
    color: #999;
  }
  .panes {
    flex: 1 1 auto;
    display: flex;
    min-height: 0;
  }
  .minimap {
    flex: 0 0 14px;
    position: relative;
    background: #1a1a1a;
    cursor: pointer;
  }
  .minimap-segment {
    position: absolute;
    left: 3px;
    right: 3px;
    border-radius: 1px;
    min-height: 2px;
    pointer-events: none;
  }
  .minimap-insert {
    background: #3fb950;
  }
  .minimap-delete {
    background: #f85149;
  }
  .minimap-replace {
    background: #d29922;
  }
  .minimap-viewport {
    position: absolute;
    left: 0;
    right: 0;
    border: 1px solid #7aa2f7;
    border-radius: 2px;
    box-sizing: border-box;
    pointer-events: none;
  }
  .pane {
    flex: 1 1 50%;
    min-width: 0;
    overflow: hidden;
  }

  :global(.diff-line-insert) {
    background-color: rgba(46, 160, 67, 0.25);
  }
  :global(.diff-line-delete) {
    background-color: rgba(248, 81, 73, 0.25);
  }
  :global(.diff-line-replace) {
    background-color: rgba(210, 153, 34, 0.25);
  }
  :global(.diff-intra) {
    background-color: rgba(210, 153, 34, 0.55);
    border-radius: 2px;
  }
  :global(.diff-collapse) {
    padding: 2px 8px;
    background: #eee;
    color: #666;
    font-style: italic;
    font-size: 12px;
    border-top: 1px solid #ddd;
    border-bottom: 1px solid #ddd;
  }
  :global(.diff-pad) {
    background: repeating-linear-gradient(
      45deg,
      rgba(128, 128, 128, 0.08),
      rgba(128, 128, 128, 0.08) 6px,
      transparent 6px,
      transparent 12px
    );
  }
</style>
