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
    nextHunkIndex,
    prevHunkIndex,
    scrollToLine,
    computeMinimapSegments,
    computeViewportIndicator,
    minimapClickToLine,
    type ChangeHunkLine,
    type MinimapSegment,
    type ViewportIndicator,
  } from "$lib/diffView";
  import type { FileDiffResult, OpenPairResult, Span } from "$lib/types";

  const FIXTURE = "100k-line-pair";
  const SCROLL_BENCH_DELAY_MS = 2000;
  const SCROLL_BENCH_DURATION_MS = 4000;

  let status = $state("loading…");
  let statLine = $state("");
  let isRealFileMode = $state(false);
  let ignoreWhitespace = $state(false);
  let ignoreCase = $state(false);

  // Populated by runRealFiles; read by retoggleDiffOptions and hunk navigation. leftView/
  // rightView/leftText/rightText aren't read in the template, so plain variables suffice;
  // changeLines and currentHunk are, so they need $state for the toolbar to update.
  let leftView: EditorView | undefined;
  let rightView: EditorView | undefined;
  let leftText = "";
  let rightText = "";
  let changeLines: ChangeHunkLine[] = $state([]);
  let currentHunk = $state(-1);
  let minimapSegments: MinimapSegment[] = $state([]);
  let viewportIndicator: ViewportIndicator = $state({ topFrac: 0, heightFrac: 1 });
  let totalLines = 0;

  function applyNewHunks(hunks: FileDiffResult["hunks"]) {
    if (!leftView || !rightView) return;
    updateHunks(leftView, hunks);
    updateHunks(rightView, hunks);
    changeLines = changeHunkLines(hunks);
    currentHunk = -1;
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

  async function retoggleDiffOptions() {
    if (!leftView || !rightView) return;
    status = "re-diffing…";
    const diff = await invoke<FileDiffResult>("diff_texts", {
      left: leftText,
      right: rightText,
      ignoreWhitespace,
      ignoreCase,
    });
    applyNewHunks(diff.hunks);
    statLine = `+${diff.stats.added} -${diff.stats.removed} ${diff.stats.chunks} chunks`;
    status = "ready";
  }

  function goToHunk(direction: 1 | -1) {
    if (!leftView || changeLines.length === 0) return;
    currentHunk = direction === 1 ? nextHunkIndex(changeLines.length, currentHunk) : prevHunkIndex(changeLines.length, currentHunk);
    scrollToLine(leftView, changeLines[currentHunk].left);
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
      if (!e.altKey || !isRealFileMode) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        goToHunk(1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        goToHunk(-1);
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
   * M1's real entry point: `diffgrid FILE1 FILE2`. Unlike `runSpike`, this never runs the
   * synthetic scroll benchmark or the `disablePadding` A/B toggle — those are M0
   * measurement-harness concerns, not part of the real application.
   */
  async function runRealFiles(left: string, right: string) {
    status = "diffing…";
    const [result, leftBuf, rightBuf] = await Promise.all([
      invoke<OpenPairResult>("open_file_pair", { left, right }),
      invoke<ArrayBuffer>("open_file_text", { path: left }),
      invoke<ArrayBuffer>("open_file_text", { path: right }),
    ]);

    leftText = new TextDecoder().decode(leftBuf);
    rightText = new TextDecoder().decode(rightBuf);
    const leftDoc = Text.of(leftText.split("\n"));
    const rightDoc = Text.of(rightText.split("\n"));

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
      { otherDoc: rightDoc, fetchSpans, onFetchError },
      true,
    );
    rightView = createDiffEditor(
      rightEl,
      rightText,
      result.diff.hunks,
      "right",
      false,
      { otherDoc: leftDoc, fetchSpans, onFetchError },
      true,
    );
    syncScroll(leftView, rightView);
    isRealFileMode = true;
    totalLines = leftDoc.lines;
    changeLines = changeHunkLines(result.diff.hunks);
    currentHunk = -1;
    minimapSegments = computeMinimapSegments(result.diff.hunks, totalLines);
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
      <label>
        <input type="checkbox" bind:checked={ignoreWhitespace} onchange={retoggleDiffOptions} />
        Ignore whitespace
      </label>
      <label>
        <input type="checkbox" bind:checked={ignoreCase} onchange={retoggleDiffOptions} />
        Ignore case
      </label>
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
