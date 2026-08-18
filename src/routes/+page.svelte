<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { Text } from "@codemirror/state";
  import { createDiffEditor, syncScroll, runScrollBenchmark } from "$lib/diffView";
  import type { FileDiffResult, OpenPairResult, Span } from "$lib/types";

  const FIXTURE = "100k-line-pair";
  const SCROLL_BENCH_DELAY_MS = 2000;
  const SCROLL_BENCH_DURATION_MS = 4000;

  let status = $state("loading…");
  let statLine = $state("");

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

    const leftText = new TextDecoder().decode(leftBuf);
    const rightText = new TextDecoder().decode(rightBuf);
    const leftDoc = Text.of(leftText.split("\n"));
    const rightDoc = Text.of(rightText.split("\n"));

    const fetchSpans = (leftLine: string, rightLine: string) =>
      invoke<Span[]>("intra_line_spans", { leftLine, rightLine });
    const onFetchError = (message: string) => invoke("report_error", { message: `intra-line: ${message}` });

    status = "mounting editors…";
    const leftEl = document.getElementById("left-pane")!;
    const rightEl = document.getElementById("right-pane")!;
    const leftView = createDiffEditor(
      leftEl,
      leftText,
      result.diff.hunks,
      "left",
      false,
      { otherDoc: rightDoc, fetchSpans, onFetchError },
      true,
    );
    const rightView = createDiffEditor(
      rightEl,
      rightText,
      result.diff.hunks,
      "right",
      false,
      { otherDoc: leftDoc, fetchSpans, onFetchError },
      true,
    );
    syncScroll(leftView, rightView);

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
  <div class="panes">
    <div id="left-pane" class="pane"></div>
    <div id="right-pane" class="pane"></div>
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
  .panes {
    flex: 1 1 auto;
    display: flex;
    min-height: 0;
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
