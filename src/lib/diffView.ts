import { EditorState, RangeSetBuilder, Text } from "@codemirror/state";
import { EditorView, Decoration, type DecorationSet, WidgetType, lineNumbers, keymap } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import type { Hunk } from "./types";

// Approximation only — the M0 spike doesn't need pixel-perfect alignment, just enough
// to exercise real decoration + block-widget + scroll-sync rendering cost.
export const LINE_HEIGHT_PX = 20;

export class PadWidget extends WidgetType {
  constructor(readonly lines: number, readonly kind: string) {
    super();
  }
  eq(other: PadWidget) {
    return other.lines === this.lines && other.kind === this.kind;
  }
  toDOM() {
    const div = document.createElement("div");
    div.className = `diff-pad diff-pad-${this.kind}`;
    div.style.height = `${this.lines * LINE_HEIGHT_PX}px`;
    return div;
  }
  ignoreEvent() {
    return true;
  }
}

export function posAfterLine(doc: Text, n: number): number {
  if (n <= 0) return 0;
  if (n >= doc.lines) return doc.length;
  return doc.line(n + 1).from;
}

/**
 * Builds line-highlight + alignment-padding decorations for one side of the diff.
 * Hunks are assumed contiguous and gapless across the whole file (diff-core guarantees this).
 */
export function buildDecorations(doc: Text, hunks: Hunk[], side: "left" | "right", disablePadding = false): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();

  for (const h of hunks) {
    const range = side === "left" ? h.left : h.right;
    const otherRange = side === "left" ? h.right : h.left;

    if (h.kind !== "equal" && range.len > 0) {
      for (let i = 0; i < range.len; i++) {
        const lineNo = range.start + i + 1;
        if (lineNo > doc.lines) break;
        const line = doc.line(lineNo);
        builder.add(line.from, line.from, Decoration.line({ class: `diff-line diff-line-${h.kind}` }));
      }
    }

    const pad = otherRange.len - range.len;
    if (pad > 0 && !disablePadding) {
      const pos = posAfterLine(doc, range.start + range.len);
      builder.add(pos, pos, Decoration.widget({ widget: new PadWidget(pad, h.kind), block: true, side: 1 }));
    }
  }

  return builder.finish();
}

export function createDiffEditor(
  parent: HTMLElement,
  text: string,
  hunks: Hunk[],
  side: "left" | "right",
  disablePadding = false,
): EditorView {
  const doc = Text.of(text.split("\n"));
  const decorations = buildDecorations(doc, hunks, side, disablePadding);

  const state = EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      keymap.of(defaultKeymap),
      javascript(),
      EditorState.readOnly.of(true),
      EditorView.decorations.of(decorations),
      EditorView.theme({
        "&": { height: "100%", fontSize: "13px" },
        ".cm-scroller": { overflow: "auto", fontFamily: "ui-monospace, Menlo, monospace" },
      }),
    ],
  });

  return new EditorView({ state, parent });
}

export function syncScroll(a: EditorView, b: EditorView): void {
  let syncing = false;
  const link = (from: EditorView, to: EditorView) => {
    from.scrollDOM.addEventListener("scroll", () => {
      if (syncing) return;
      syncing = true;
      to.scrollDOM.scrollTop = from.scrollDOM.scrollTop;
      to.scrollDOM.scrollLeft = from.scrollDOM.scrollLeft;
      syncing = false;
    });
  };
  link(a, b);
  link(b, a);
}

export interface FrameStats {
  frames: number;
  durationMs: number;
  /** Cost of the very first scroll-triggered layout/paint, isolated from steady state. */
  firstScrollFrameMs: number;
  /** Stats over all frames *after* the first scroll mutation — the sustained-scroll signal. */
  steadyMeanFrameMs: number;
  steadyP95FrameMs: number;
  steadyWorstFrameMs: number;
  steadyEstimatedFps: number;
  /** Count of steady-state frames slower than one 60fps frame budget (~16.7ms) and than 33ms (half rate). */
  framesOver16_7ms: number;
  framesOver33ms: number;
}

/**
 * Programmatically scrolls `view` for `durationMs`, recording the wall-clock gap between
 * consecutive requestAnimationFrame callbacks as a proxy for real frame time under load.
 * The pre-scroll setup delta and the first post-mutation frame are reported separately from
 * steady-state, since a one-time layout cost and sustained per-frame cost are different findings.
 */
export function runScrollBenchmark(view: EditorView, durationMs: number): Promise<FrameStats> {
  return new Promise((resolve) => {
    const deltas: number[] = [];
    let last = performance.now();
    let elapsed = 0;
    let dir = 1;
    const step = 15;

    function tick(now: number) {
      const delta = now - last;
      last = now;
      elapsed += delta;
      deltas.push(delta);

      const max = view.scrollDOM.scrollHeight - view.scrollDOM.clientHeight;
      let next = view.scrollDOM.scrollTop + dir * step;
      if (next >= max) {
        next = max;
        dir = -1;
      } else if (next <= 0) {
        next = 0;
        dir = 1;
      }
      view.scrollDOM.scrollTop = next;

      if (elapsed < durationMs) {
        requestAnimationFrame(tick);
      } else {
        // deltas[0] is pre-scroll setup (discarded); deltas[1] spans the first scroll
        // mutation's layout/paint; deltas[2..] is steady-state sustained scrolling.
        const firstScrollFrameMs = deltas[1] ?? 0;
        const steady = deltas.slice(2);
        const sorted = [...steady].sort((x, y) => x - y);
        const mean = steady.reduce((s, v) => s + v, 0) / steady.length;
        const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? mean;
        const worst = sorted[sorted.length - 1] ?? mean;
        resolve({
          frames: deltas.length,
          durationMs: elapsed,
          firstScrollFrameMs,
          steadyMeanFrameMs: mean,
          steadyP95FrameMs: p95,
          steadyWorstFrameMs: worst,
          steadyEstimatedFps: 1000 / mean,
          framesOver16_7ms: steady.filter((d) => d > 16.7).length,
          framesOver33ms: steady.filter((d) => d > 33).length,
        });
      }
    }
    requestAnimationFrame(tick);
  });
}
