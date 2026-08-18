<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke, Channel } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
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
  import type { DiffStats, DirEntry, FileDiffResult, Hunk, OpenPairResult, ScanOutcome, Settings, Span } from "$lib/types";
  import { visibleDirEntries as visibleDirEntriesFn } from "$lib/dirView";
  import { createTabId, tabLabel } from "$lib/tabs";

  const FIXTURE = "100k-line-pair";
  const SCROLL_BENCH_DELAY_MS = 2000;
  const SCROLL_BENCH_DURATION_MS = 4000;
  // M2: how long to wait after the last keystroke on either pane before re-diffing. Per
  // docs/PLAN.md §2 this must be debounced, not per-keystroke -- a live re-diff on every
  // character would mean a Rust round-trip (plus a full histogram diff) on every keystroke,
  // which is exactly the per-keystroke cost the delta pipeline exists to avoid elsewhere.
  const EDIT_REDIFF_DEBOUNCE_MS = 300;

  // "loading": nothing decided yet. "spike": M0 benchmark flow (no launch args). "session":
  // M1-M3 unified per M4 (docs/PLAN.md M4) -- one or more open file-pair tabs, plus (when the
  // root pair is a directory comparison) a sidebar of changed files to open tabs from.
  let mode = $state<"loading" | "spike" | "session">("loading");
  let status = $state("loading…");

  // M4's global preferences (docs/PLAN.md §5), loaded once at startup from the Rust-side
  // persisted file (see load_settings/save_settings in src-tauri). ignoreWhitespace/ignoreCase
  // here are only ever *defaults* for a newly-opened tab (see newFileTab) -- each tab's own
  // toolbar checkboxes are the per-tab override PLAN.md describes, and never write back here.
  // collapseContextLines/intraLineMode have no per-tab override; they apply uniformly.
  let settings: Settings = $state({ ignoreWhitespace: false, ignoreCase: false, collapseContextLines: 3, intraLineMode: "character" });

  // Which kind of root this session was opened on -- determines whether the sidebar shows.
  // `null` until launch args are resolved in onMount.
  let sessionKind: "file" | "dir" | null = $state(null);

  // M3: populated once a directory scan has run. Root paths + the already-fetched entry list are
  // session-lifetime state, independent of which (if any) file tabs are currently open.
  let dirLeftRoot = $state("");
  let dirRightRoot = $state("");
  let dirEntries: DirEntry[] = $state([]);
  let dirScanning = $state(false);
  let dirScanOutcome: ScanOutcome | null = $state(null);
  let hideIdentical = $state(true);
  // Entries arrive in scan order (and the left-only tail is HashMap iteration order, so it's not
  // even deterministic run to run) -- visibleDirEntries (tested in dirView.test.ts) sorts by path
  // so the table is actually usable for finding a specific file, matching the
  // flat-table-is-a-complete-capability scope call in DECISIONS.md.
  const visibleDirEntries = $derived(visibleDirEntriesFn(dirEntries, hideIdentical));

  /**
   * M4's per-tab reactive UI state -- one `FileTab` per open file-pair tab. Deliberately holds
   * only plain, Svelte-reactive data (dirty flags, hunk lists, minimap geometry); the live
   * `EditorView` instances and edit-queue promises live in `tabRuntimes` below, *not* here, per
   * docs/PLAN.md §1's "diff panes are managed imperatively... mixing two reactive systems over
   * the same hot-path DOM is asking for dropped frames" -- a CM6 `EditorView` has no business
   * being deep-proxied by Svelte's `$state`.
   */
  interface FileTab {
    id: string;
    leftPath: string;
    rightPath: string;
    label: string;
    dirtyLeft: boolean;
    dirtyRight: boolean;
    savingLeft: boolean;
    savingRight: boolean;
    changeLines: ChangeHunkLine[];
    // Parallel to changeLines (same filter, same order -- see changeHunks), kept separately
    // because copyCurrentHunk needs the full Hunk (kind + both LineRanges), which
    // ChangeHunkLine's two bare line numbers don't carry.
    currentChangeHunks: Hunk[];
    currentHunk: number;
    minimapSegments: MinimapSegment[];
    viewportIndicator: ViewportIndicator;
    totalLines: number;
    ignoreWhitespace: boolean;
    ignoreCase: boolean;
    diffStats: DiffStats | null;
  }

  /** The live, non-reactive half of a tab's state -- kept out of `$state` on purpose (see
   * `FileTab`'s doc comment). `editQueueLeft`/`editQueueRight` are M2's per-side promise chains
   * (see `onEdit`): each `apply_edit` call is only *issued* once the previous one for that same
   * (tab, side) has resolved, so deltas always land at Rust in the order CM6 produced them, even
   * though `invoke` is async and could otherwise let calls resolve out of order. */
  interface TabRuntime {
    leftView: EditorView;
    rightView: EditorView;
    editQueueLeft: Promise<void>;
    editQueueRight: Promise<void>;
    redoDiffTimer: ReturnType<typeof setTimeout> | undefined;
  }

  let tabs: FileTab[] = $state([]);
  let activeTabId: string | null = $state(null);
  const activeTab = $derived(tabs.find((t) => t.id === activeTabId) ?? null);
  const tabRuntimes = new Map<string, TabRuntime>();

  function getTab(id: string): FileTab | undefined {
    return tabs.find((t) => t.id === id);
  }

  function newFileTab(id: string, leftPath: string, rightPath: string): FileTab {
    return {
      id,
      leftPath,
      rightPath,
      label: tabLabel(leftPath, rightPath),
      dirtyLeft: false,
      dirtyRight: false,
      savingLeft: false,
      savingRight: false,
      changeLines: [],
      currentChangeHunks: [],
      currentHunk: -1,
      minimapSegments: [],
      viewportIndicator: { topFrac: 0, heightFrac: 1 },
      totalLines: 0,
      ignoreWhitespace: settings.ignoreWhitespace,
      ignoreCase: settings.ignoreCase,
      diffStats: null,
    };
  }

  function applyNewHunksFor(id: string, hunks: Hunk[]) {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt) return;
    updateHunks(rt.leftView, hunks);
    updateHunks(rt.rightView, hunks);
    tab.changeLines = changeHunkLines(hunks);
    tab.currentChangeHunks = changeHunks(hunks);
    // Deliberately reset rather than trying to re-point at "the same" hunk post-copy: a copy
    // changes the hunk list (the copied hunk usually disappears, and every hunk after it can
    // shift), the same as any other hunks-invalidating event (a toggle, another edit) already
    // does. Consistent behavior across all of those beats trying to preserve a selection that
    // may no longer refer to anything meaningful.
    tab.currentHunk = -1;
    // Refreshed here, not just at open: an edit can add or remove lines, so a re-diff's hunk
    // list may no longer match the line count the minimap was last computed against.
    tab.totalLines = rt.leftView.state.doc.lines;
    tab.minimapSegments = computeMinimapSegments(hunks, tab.totalLines);
  }

  function updateViewportIndicatorFor(id: string) {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt) return;
    const { scrollTop, scrollHeight, clientHeight } = rt.leftView.scrollDOM;
    tab.viewportIndicator = computeViewportIndicator(scrollTop, scrollHeight, clientHeight);
  }

  function onMinimapClick(id: string, e: MouseEvent) {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt || tab.totalLines === 0) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    scrollToLine(rt.leftView, minimapClickToLine((e.clientY - rect.top) / rect.height, tab.totalLines));
  }

  /**
   * Re-diffs one tab against the Rust-side `EditBuffer`s' *current* text (which reflects any
   * edits applied via `apply_edit` since open, not just the text as it was when the file was
   * opened) with that tab's whitespace/case-ignore toggles. Replaces M1's `diff_texts`, which
   * sent `leftText`/`rightText` from the frontend on every call -- once edits exist, holding a
   * frontend copy of "the text" at all just invites sending a stale one, so the toggle path and
   * the edit-re-diff path now share one mechanism with one source of truth.
   *
   * Awaits both edit queues first so a re-diff (whether from a toggle click or the debounced
   * timer below) never races an `apply_edit` call that's still in flight for the same tab.
   */
  async function flushAndRedoDiffFor(id: string): Promise<FileDiffResult | undefined> {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt) return undefined;
    await Promise.all([rt.editQueueLeft, rt.editQueueRight]);
    const diff = await invoke<FileDiffResult>("redo_diff", { tabId: id, ignoreWhitespace: tab.ignoreWhitespace, ignoreCase: tab.ignoreCase });
    applyNewHunksFor(id, diff.hunks);
    tab.diffStats = diff.stats;
    return diff;
  }

  async function retoggleDiffOptions(id: string) {
    if (!tabRuntimes.has(id)) return;
    status = "re-diffing…";
    await flushAndRedoDiffFor(id);
    status = "ready";
  }

  function scheduleDebouncedRedoDiff(id: string) {
    const rt = tabRuntimes.get(id);
    if (!rt) return;
    if (rt.redoDiffTimer !== undefined) clearTimeout(rt.redoDiffTimer);
    rt.redoDiffTimer = setTimeout(() => {
      rt.redoDiffTimer = undefined;
      flushAndRedoDiffFor(id).catch((err) => invoke("report_error", { message: `redo_diff(${id}): ${err}` }));
    }, EDIT_REDIFF_DEBOUNCE_MS);
  }

  /**
   * The frontend -> Rust half of docs/PLAN.md §2's delta pipeline: forwards each delta CM6
   * captured to the matching tab's `EditBuffer`, then (debounced) triggers a re-diff. `side` is
   * fixed per call site (see `mountFileTab`), not derived from the delta, since `EditDelta`
   * itself carries no side information -- it's purely a CM6-document-relative offset pair.
   */
  function onEdit(id: string, side: "left" | "right", deltas: EditDelta[]) {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt) return;
    if (side === "left") tab.dirtyLeft = true;
    else tab.dirtyRight = true;
    for (const delta of deltas) {
      const send = (): Promise<void> =>
        invoke<void>("apply_edit", { tabId: id, side, fromUtf16: delta.fromUtf16, toUtf16: delta.toUtf16, inserted: delta.inserted }).catch(
          (err) => {
            invoke("report_error", { message: `apply_edit(${id}/${side}): ${err}` });
          },
        );
      if (side === "left") rt.editQueueLeft = rt.editQueueLeft.then(send);
      else rt.editQueueRight = rt.editQueueRight.then(send);
    }
    scheduleDebouncedRedoDiff(id);
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
  async function saveSide(id: string, side: "left" | "right") {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt) return;
    const path = side === "left" ? tab.leftPath : tab.rightPath;
    if (!path) return;
    if (side === "left") tab.savingLeft = true;
    else tab.savingRight = true;
    try {
      await (side === "left" ? rt.editQueueLeft : rt.editQueueRight);
      await invoke("save_file", { tabId: id, side, path });
      if (side === "left") tab.dirtyLeft = false;
      else tab.dirtyRight = false;
    } catch (err) {
      status = `save failed (${side}): ${err}`;
      await invoke("report_error", { message: `save_file(${id}/${side}): ${err}` });
    } finally {
      if (side === "left") tab.savingLeft = false;
      else tab.savingRight = false;
    }
  }

  function goToHunk(id: string, direction: 1 | -1) {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt || tab.changeLines.length === 0) return;
    tab.currentHunk = direction === 1 ? nextHunkIndex(tab.changeLines.length, tab.currentHunk) : prevHunkIndex(tab.changeLines.length, tab.currentHunk);
    scrollToLine(rt.leftView, tab.changeLines[tab.currentHunk].left);
  }

  /**
   * "Apply/revert individual hunks left↔right" (docs/PLAN.md M2), scoped to the
   * currently-navigated hunk via the existing Prev/Next diff controls rather than per-hunk
   * inline gutter buttons -- see DECISIONS.md. Dispatches the copy as a normal CM6 transaction
   * on the destination view, so it flows through the exact same onEdit → apply_edit →
   * debounced redo_diff pipeline a keystroke would, with no separate backend command.
   */
  function copyCurrentHunk(id: string, direction: "leftToRight" | "rightToLeft") {
    const tab = getTab(id);
    const rt = tabRuntimes.get(id);
    if (!tab || !rt || tab.currentHunk === -1) return;
    const hunk = tab.currentChangeHunks[tab.currentHunk];
    const change = buildHunkCopyChange(hunk, direction, rt.leftView.state.doc, rt.rightView.state.doc);
    if (!change) return;
    const destView = change.destSide === "left" ? rt.leftView : rt.rightView;
    destView.dispatch({ changes: { from: change.from, to: change.to, insert: change.insert } });
  }

  function doubleRaf(): Promise<void> {
    return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  }

  /**
   * Mounts the CM6 editors for a tab that's already been pushed into `tabs` (and whose panes
   * therefore already exist in the DOM, per the `await tick()` in `openFileTab`). Split out from
   * `openFileTab` so the id-generation/array-push/DOM-wait steps stay in one place and this part
   * -- the actual `invoke` calls and editor construction -- doesn't have to be duplicated for a
   * future second entry point.
   */
  async function mountFileTab(id: string, left: string, right: string) {
    status = "diffing…";
    const [result, leftBuf, rightBuf] = await Promise.all([
      invoke<OpenPairResult>("open_file_pair", { tabId: id, left, right }),
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

    const tab = getTab(id);
    if (!tab) return; // the tab was closed while open_file_pair/open_file_text were in flight

    // "Off" is a frontend short-circuit, not a backend mode: when the setting says not to
    // highlight intra-line differences at all, `createDiffEditor` never even gets an `intraLine`
    // option, so the highlighter extension isn't wired in and no `intra_line_spans` IPC round
    // trip happens per visible Replace line -- not "wired in but always returns empty," which
    // would still pay that cost for nothing. See DECISIONS.md.
    const intraLineMode = settings.intraLineMode;
    const fetchSpans = (leftLine: string, rightLine: string) =>
      invoke<Span[]>("intra_line_spans", {
        leftLine,
        rightLine,
        ignoreWhitespace: tab.ignoreWhitespace,
        ignoreCase: tab.ignoreCase,
        mode: intraLineMode,
      });
    const onFetchError = (message: string) => invoke("report_error", { message: `intra-line(${id}): ${message}` });

    status = "mounting editors…";
    const leftEl = document.getElementById(`left-pane-${id}`)!;
    const rightEl = document.getElementById(`right-pane-${id}`)!;
    let leftView!: EditorView;
    let rightView!: EditorView;
    leftView = createDiffEditor(
      leftEl,
      leftText,
      result.diff.hunks,
      "left",
      false,
      intraLineMode === "off" ? undefined : { getOtherDoc: () => (rightView ? rightView.state.doc : rightDocAtOpen), fetchSpans, onFetchError },
      true,
      true,
      (deltas) => onEdit(id, "left", deltas),
      settings.collapseContextLines,
    );
    rightView = createDiffEditor(
      rightEl,
      rightText,
      result.diff.hunks,
      "right",
      false,
      intraLineMode === "off" ? undefined : { getOtherDoc: () => (leftView ? leftView.state.doc : leftDocAtOpen), fetchSpans, onFetchError },
      true,
      true,
      (deltas) => onEdit(id, "right", deltas),
      settings.collapseContextLines,
    );
    syncScroll(leftView, rightView);
    tabRuntimes.set(id, { leftView, rightView, editQueueLeft: Promise.resolve(), editQueueRight: Promise.resolve(), redoDiffTimer: undefined });

    // Sets changeLines/currentChangeHunks/currentHunk/minimapSegments/totalLines consistently
    // with every later re-diff, rather than duplicating that logic here -- an earlier version of
    // this function set changeLines directly but never set currentChangeHunks, which stayed
    // stale ([]) until the first toggle/edit, making copyCurrentHunk crash on the very first
    // hunk-copy click before any other re-diff had run. Caught by manual testing under Xvfb,
    // not by any unit test (none of them exercise this function's real invoke() calls).
    applyNewHunksFor(id, result.diff.hunks);
    tab.diffStats = result.diff.stats;
    leftView.scrollDOM.addEventListener("scroll", () => updateViewportIndicatorFor(id));
    updateViewportIndicatorFor(id);

    status = "ready";
    await invoke("report_ready");
  }

  /**
   * Opens a new tab for a file pair: allocates a fresh id, pushes a `FileTab` into `tabs`, waits
   * a tick so its (id-suffixed) pane elements actually exist in the DOM -- `{#each tabs}` doesn't
   * repaint synchronously on assignment -- then mounts the real editors. Making a tab the active
   * one immediately (before its editors exist) is deliberate: it un-hides the right `.panes` div
   * via `class:hidden`, which is what makes `document.getElementById` inside `mountFileTab` find
   * a laid-out, visible element rather than a `display:none` one CM6 would mis-measure.
   */
  async function openFileTab(left: string, right: string) {
    const id = createTabId();
    tabs = [...tabs, newFileTab(id, left, right)];
    activeTabId = id;
    await tick();
    await mountFileTab(id, left, right);
  }

  /**
   * Closes a tab: guards on that tab's own unsaved changes (not any other tab's), tears down its
   * `EditorView`s and debounce timer, drops its `tabRuntimes` entry, tells Rust to free its
   * `EditBuffer`s, and picks a new active tab if the closed one was active. `skipDirtyGuard` is
   * for callers that already confirmed discard themselves (there are none yet, but mirrors the
   * `confirmDiscardIfDirty`-then-act split M3 used, in case a future caller needs it).
   */
  async function closeTab(id: string, opts: { skipDirtyGuard?: boolean } = {}) {
    const tab = getTab(id);
    if (!tab) return;
    if (!opts.skipDirtyGuard && (tab.dirtyLeft || tab.dirtyRight)) {
      if (!window.confirm("You have unsaved changes. Discard them and close this tab?")) return;
    }
    const rt = tabRuntimes.get(id);
    rt?.leftView.destroy();
    rt?.rightView.destroy();
    if (rt?.redoDiffTimer !== undefined) clearTimeout(rt.redoDiffTimer);
    tabRuntimes.delete(id);
    const closedIndex = tabs.findIndex((t) => t.id === id);
    tabs = tabs.filter((t) => t.id !== id);
    invoke("close_tab", { tabId: id });
    if (activeTabId === id) {
      activeTabId = tabs.length === 0 ? null : (tabs[Math.min(closedIndex, tabs.length - 1)]?.id ?? tabs[0].id);
    }
  }

  onMount(async () => {
    // The settings window emits this after every successful save (src-tauri's save_settings)
    // so the main window's already-loaded `settings` (and any already-open tab's *global-only*
    // fields, since those have no per-tab override to protect) stay in sync without polling.
    listen<Settings>("settings-changed", (event) => {
      settings = event.payload;
    });
    window.addEventListener("error", (e) => {
      invoke("report_error", { message: `window.onerror: ${e.message}` });
    });
    window.addEventListener("unhandledrejection", (e) => {
      invoke("report_error", { message: `unhandledrejection: ${String(e.reason)}` });
    });
    window.addEventListener("keydown", (e) => {
      if (!activeTabId) return;
      const rt = tabRuntimes.get(activeTabId);
      if (!rt) return;
      if (e.altKey) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          goToHunk(activeTabId, 1);
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          goToHunk(activeTabId, -1);
        }
        return;
      }
      // Cmd+S on macOS, Ctrl+S elsewhere -- saves whichever pane currently has focus.
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        if (rt.leftView.hasFocus) saveSide(activeTabId, "left");
        else if (rt.rightView.hasFocus) saveSide(activeTabId, "right");
      }
    });
    try {
      settings = await invoke<Settings>("load_settings");
      const args = await invoke<string[]>("launch_args");
      if (args.length === 2) {
        const [leftKind, rightKind] = await Promise.all([
          invoke<string>("path_kind", { path: args[0] }),
          invoke<string>("path_kind", { path: args[1] }),
        ]);
        if (leftKind !== rightKind) {
          throw new Error(`cannot compare a ${leftKind} with a ${rightKind}: ${args[0]} vs ${args[1]}`);
        }
        if (leftKind === "dir") {
          await runDirCompare(args[0], args[1]);
        } else {
          await runRealFiles(args[0], args[1]);
        }
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
   * M1/M2's real entry point: `diffgrid FILE1 FILE2`. M4: opens the pair as this session's one
   * tab (no sidebar, since there's no directory root to populate one from) rather than the
   * module-level singleton view M1-M3 used -- see DECISIONS.md for why a bare file-pair
   * invocation still ends up going through the same tab machinery a directory session's rows do,
   * rather than keeping a separate no-tabs code path for this case.
   */
  async function runRealFiles(left: string, right: string) {
    sessionKind = "file";
    mode = "session";
    await openFileTab(left, right);
  }

  async function runSpike() {
    mode = "spike";
    // The spike panes are `display:none` until `mode === "spike"` (see the template) -- without
    // waiting a tick here, `document.getElementById` below would race Svelte's own DOM update,
    // the same trap `openFileTab` guards against explicitly. Relying on the `await Promise.all`
    // below to *happen* to give Svelte a chance to flush first is exactly the kind of "probably
    // works" timing dependency that trap exists to avoid.
    await tick();
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
    const leftEl = document.getElementById("spike-left-pane")!;
    const rightEl = document.getElementById("spike-right-pane")!;
    const leftView = createDiffEditor(leftEl, leftText, diff.hunks, "left", flags.disable_padding, undefined, flags.collapse_equal);
    const rightView = createDiffEditor(rightEl, rightText, diff.hunks, "right", flags.disable_padding, undefined, flags.collapse_equal);
    syncScroll(leftView, rightView);

    await doubleRaf();
    const paintMs = performance.now() - t0;
    status = `ready — +${diff.stats.added} -${diff.stats.removed} ${diff.stats.chunks} chunks · open-to-first-paint ${paintMs.toFixed(1)}ms`;
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

  /**
   * M3's real entry point for `diffgrid DIR1 DIR2`. Streams `DirEntry` batches from `scan_dirs`
   * over a `Channel` into `dirEntries` as they arrive -- the scan itself is what's incremental
   * (docs/PLAN.md §3); this just appends whatever shows up, in whatever order it arrives. M4:
   * no longer switches to a full-page table -- the sidebar and any open file tabs coexist in one
   * layout, per the session-shell mockup (docs/UI/ui-01.png).
   */
  async function runDirCompare(left: string, right: string) {
    sessionKind = "dir";
    dirLeftRoot = left;
    dirRightRoot = right;
    dirEntries = [];
    dirScanOutcome = null;
    dirScanning = true;
    mode = "session";
    status = "scanning…";

    // A large tree streams thousands of small batches. Applying each straight to `dirEntries`
    // forces a full re-sort + keyed DOM reorder of an unvirtualized table on every single one --
    // real, unbounded redundant work as entry count grows. Coalescing into at most one flush per
    // animation frame bounds that to the display refresh rate regardless of how fast batches
    // arrive. NOT verified to be sufficient by itself to keep Cancel reliably clickable on very
    // large trees (see DECISIONS.md) -- that investigation's own A/B tests came back too
    // confounded by this scan's two-phase design (phase 1 streams zero rows, so a click's landing
    // odds depend heavily on which phase it happens to land in) to isolate cause and effect. This
    // is kept because it's a strict improvement with no downside, not because it was proven to
    // fix the large-scale case on its own.
    let pendingEntries: DirEntry[] = [];
    let flushHandle: number | null = null;
    const flush = () => {
      flushHandle = null;
      if (pendingEntries.length === 0) return;
      dirEntries = [...dirEntries, ...pendingEntries];
      pendingEntries = [];
    };

    const channel = new Channel<DirEntry[]>();
    channel.onmessage = (batch) => {
      pendingEntries.push(...batch);
      if (flushHandle === null) {
        flushHandle = requestAnimationFrame(flush);
      }
    };

    try {
      const outcome = await invoke<ScanOutcome>("scan_dirs", {
        left,
        right,
        respectGitignore: true,
        excludeGlobs: [],
        channel,
      });
      if (flushHandle !== null) cancelAnimationFrame(flushHandle);
      flush();
      dirScanOutcome = outcome;
      status = outcome.cancelled ? `scan cancelled — ${dirEntries.length} entries found before stopping` : "ready";
    } catch (e) {
      status = `scan failed: ${e}`;
      await invoke("report_error", { message: `scan_dirs: ${e}` });
    } finally {
      dirScanning = false;
    }
    await invoke("report_ready");
  }

  function cancelDirScan() {
    invoke("cancel_scan");
  }

  /**
   * A row is only meaningful to open as a two-way text diff when both sides are actual regular
   * files with real content to compare -- a directory (its "Same"/"Modified" status already
   * says nothing about its children, which have their own rows), a symlink (diffing two link
   * *targets* as text is a different, unimplemented feature), or a LeftOnly/RightOnly/
   * TypeConflict entry (no meaningful "other side" to diff against) are all excluded.
   */
  function isOpenable(entry: DirEntry): boolean {
    return !entry.isDir && !entry.isSymlink && (entry.status === "same" || entry.status === "modified");
  }

  /**
   * A single-character prefix conveying status alongside the row's color, per the sidebar's
   * compact-list scope decision in DECISIONS.md -- color alone isn't accessible/colorblind-safe,
   * so this is the non-color signal, matching the sigil convention real tools (git status, VS
   * Code's Source Control view) already use rather than a wordy status column this sidebar's
   * width can't accommodate alongside the path.
   */
  function statusSigil(status: DirEntry["status"]): string {
    switch (status) {
      case "modified":
        return "~";
      case "leftOnly":
        return "-";
      case "rightOnly":
        return "+";
      case "typeConflict":
        return "!";
      default:
        return " ";
    }
  }

  /**
   * Opens a row from the sidebar as a tab -- or, if that exact pair is already open, just
   * switches to its existing tab rather than opening a duplicate. Real IDEs and editors all do
   * this; without it, clicking the same row twice would silently accumulate duplicate tabs for
   * the same pair, which is never what a user wants from a tabbed UI.
   */
  async function openRowAsFilePair(entry: DirEntry) {
    if (!isOpenable(entry)) return;
    const left = `${dirLeftRoot}/${entry.path}`;
    const right = `${dirRightRoot}/${entry.path}`;
    const existing = tabs.find((t) => t.leftPath === left && t.rightPath === right);
    if (existing) {
      activeTabId = existing.id;
      return;
    }
    await openFileTab(left, right);
  }
</script>

<main>
  <div class="status">
    <span class="status-text">{status}</span>
    <button class="settings-button" onclick={() => invoke("open_settings_window")} title="Settings">⚙</button>
  </div>
  <div class="body">
    {#if sessionKind === "dir"}
      <aside class="sidebar">
        <div class="sidebar-header">SESSION</div>
        <div class="sidebar-roots">
          <div class="sidebar-root">{dirLeftRoot}</div>
          <div class="sidebar-root">{dirRightRoot}</div>
        </div>
        <div class="sidebar-toolbar">
          <button onclick={cancelDirScan} disabled={!dirScanning}>Cancel scan</button>
          <label>
            <input type="checkbox" bind:checked={hideIdentical} />
            Hide identical
          </label>
        </div>
        <div class="sidebar-header">
          CHANGED FILES · {visibleDirEntries.length}
          {dirScanOutcome?.cancelled ? " (cancelled)" : ""}
        </div>
        <div class="dir-table-wrap">
          <table class="dir-table">
            <tbody>
              {#each visibleDirEntries as entry (entry.path)}
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <tr class="status-{entry.status}" class:openable={isOpenable(entry)} onclick={() => openRowAsFilePair(entry)}>
                  <td class="sigil">{statusSigil(entry.status)}</td>
                  <td>{entry.path}{entry.isDir ? "/" : ""}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </aside>
    {/if}
    <div class="main-area">
      {#if tabs.length > 0}
        <div class="tab-bar">
          {#each tabs as tab (tab.id)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="tab" class:active={tab.id === activeTabId} onclick={() => (activeTabId = tab.id)}>
              <span class="tab-label">{tab.dirtyLeft || tab.dirtyRight ? "● " : ""}{tab.label}</span>
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <span class="tab-close" onclick={(e) => { e.stopPropagation(); closeTab(tab.id); }} title="Close tab">&times;</span>
            </div>
          {/each}
        </div>
      {/if}
      {#if activeTab}
        <div class="toolbar">
          <button onclick={() => goToHunk(activeTab.id, -1)} disabled={activeTab.changeLines.length === 0}>&uarr; Prev diff</button>
          <button onclick={() => goToHunk(activeTab.id, 1)} disabled={activeTab.changeLines.length === 0}>&darr; Next diff</button>
          <span class="hunk-count">
            {activeTab.changeLines.length === 0 ? "no changes" : `${activeTab.currentHunk + 1} / ${activeTab.changeLines.length}`}
          </span>
          <button
            onclick={() => copyCurrentHunk(activeTab.id, "rightToLeft")}
            disabled={activeTab.currentHunk === -1}
            title="Copy the current hunk's right-side version to the left"
          >
            &larr; Copy to left
          </button>
          <button
            onclick={() => copyCurrentHunk(activeTab.id, "leftToRight")}
            disabled={activeTab.currentHunk === -1}
            title="Copy the current hunk's left-side version to the right"
          >
            Copy to right &rarr;
          </button>
          <label>
            <input type="checkbox" bind:checked={activeTab.ignoreWhitespace} onchange={() => retoggleDiffOptions(activeTab.id)} />
            Ignore whitespace
          </label>
          <label>
            <input type="checkbox" bind:checked={activeTab.ignoreCase} onchange={() => retoggleDiffOptions(activeTab.id)} />
            Ignore case
          </label>
          <button
            onclick={() => saveSide(activeTab.id, "left")}
            disabled={!activeTab.dirtyLeft || activeTab.savingLeft}
            title="Save left (Cmd/Ctrl+S while focused)"
          >
            {activeTab.dirtyLeft ? "● " : ""}Save left
          </button>
          <button
            onclick={() => saveSide(activeTab.id, "right")}
            disabled={!activeTab.dirtyRight || activeTab.savingRight}
            title="Save right (Cmd/Ctrl+S while focused)"
          >
            {activeTab.dirtyRight ? "● " : ""}Save right
          </button>
          {#if activeTab.diffStats}
            <span class="stat">+{activeTab.diffStats.added} -{activeTab.diffStats.removed} {activeTab.diffStats.chunks} chunks</span>
          {/if}
        </div>
      {/if}
      <!-- Every open tab's panes stay mounted (hidden via CSS when inactive), not just the active
           one's: `mountFileTab` looks its pane elements up by id synchronously, and a tab that's
           never been the active one yet would otherwise never have laid-out (non-display:none)
           elements for CM6 to measure. This also means switching tabs is a pure visibility
           toggle -- scroll position, undo history, and everything else CM6 tracks survive it. -->
      {#each tabs as tab (tab.id)}
        <div class="panes" class:hidden={tab.id !== activeTabId}>
          <div id="left-pane-{tab.id}" class="pane"></div>
          <div id="right-pane-{tab.id}" class="pane"></div>
          <!-- Supplementary pointing-device shortcut to the same navigation the Prev/Next diff
               buttons and Alt+Up/Down already provide with full keyboard access. A click here
               means "jump to the line at this Y position," which has no meaningful keyboard
               equivalent (unlike a real interactive control) -- deliberately not keyboard-operable
               itself, since the same destinations are already reachable by keyboard elsewhere. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="minimap" onclick={(e) => onMinimapClick(tab.id, e)} title="Click to jump to a position in the file">
            {#each tab.minimapSegments as seg}
              <div class="minimap-segment minimap-{seg.kind}" style="top: {seg.startFrac * 100}%; height: {seg.lenFrac * 100}%;"></div>
            {/each}
            <div class="minimap-viewport" style="top: {tab.viewportIndicator.topFrac * 100}%; height: {tab.viewportIndicator.heightFrac * 100}%;"></div>
          </div>
        </div>
      {/each}
      {#if sessionKind === "dir" && tabs.length === 0}
        <div class="empty-state">Select a file from the sidebar to compare it.</div>
      {/if}
      <!-- M0's benchmark flow only -- always mounted (same DOM-query-timing reasoning as the
           per-tab panes above), hidden via CSS outside "spike" mode. Uses its own element ids,
           entirely separate from the per-tab ones, so the two flows can never collide. -->
      <div class="panes" class:hidden={mode !== "spike"}>
        <div id="spike-left-pane" class="pane"></div>
        <div id="spike-right-pane" class="pane"></div>
      </div>
    </div>
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
  .status {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
    font-size: 12px;
    background: #222;
    color: #ddd;
  }
  .settings-button {
    background: none;
    border: none;
    color: #ddd;
    font-size: 13px;
    cursor: pointer;
    padding: 0 4px;
  }
  .settings-button:hover {
    color: #fff;
  }
  .body {
    flex: 1 1 auto;
    display: flex;
    min-height: 0;
  }
  .sidebar {
    flex: 0 0 260px;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: #fafafa;
    border-right: 1px solid #ddd;
    font-size: 12px;
  }
  .sidebar-header {
    padding: 6px 8px;
    font-weight: bold;
    color: #666;
    background: #f0f0f0;
  }
  .sidebar-roots {
    padding: 4px 8px;
  }
  .sidebar-root {
    color: #333;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sidebar-toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
  }
  .sidebar-toolbar label {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .main-area {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .tab-bar {
    flex: 0 0 auto;
    display: flex;
    background: #222;
    overflow-x: auto;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    font-size: 12px;
    color: #aaa;
    border-right: 1px solid #333;
    cursor: pointer;
    white-space: nowrap;
  }
  .tab.active {
    color: #fff;
    background: #333;
  }
  .tab-close {
    color: #888;
    padding: 0 2px;
  }
  .tab-close:hover {
    color: #fff;
  }
  .toolbar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
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
  .toolbar button,
  .sidebar-toolbar button {
    font-size: 12px;
    background: #444;
    color: #ddd;
    border: 1px solid #555;
    border-radius: 3px;
    padding: 2px 6px;
    cursor: pointer;
  }
  .sidebar-toolbar button {
    background: #eee;
    color: #333;
    border-color: #ccc;
  }
  .toolbar button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .hunk-count,
  .toolbar .stat {
    color: #999;
  }
  .empty-state {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #999;
    font-size: 13px;
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
  .panes.hidden {
    display: none;
  }
  .dir-table-wrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    background: #fafafa;
  }
  .dir-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .dir-table td {
    text-align: left;
    padding: 3px 8px;
    border-bottom: 1px solid #eee;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dir-table td.sigil {
    width: 1em;
    padding-right: 0;
    font-weight: bold;
  }
  .dir-table tr.openable {
    cursor: pointer;
  }
  .dir-table tr.openable:hover {
    background: #eef4ff;
  }
  .dir-table tr.status-modified {
    color: #9a6700;
  }
  .dir-table tr.status-leftOnly {
    color: #cf222e;
  }
  .dir-table tr.status-rightOnly {
    color: #1a7f37;
  }
  .dir-table tr.status-typeConflict {
    color: #cf222e;
    font-weight: bold;
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
