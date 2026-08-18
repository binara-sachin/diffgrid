import { EditorState, RangeSet, RangeSetBuilder, StateEffect, StateField, Text } from "@codemirror/state";
import { EditorView, Decoration, type DecorationSet, ViewPlugin, type ViewUpdate, WidgetType, lineNumbers, keymap } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import type { Hunk, Span } from "./types";

// Must exactly match the line-height set in createDiffEditor's theme below (pinned there,
// not left to the browser default). Previously an unmeasured guess of 20 against a real
// rendered line-height of 18px, so alignment padding was visually wrong by ~10%.
export const LINE_HEIGHT_PX = 18;

export function posAfterLine(doc: Text, n: number): number {
  if (n <= 0) return 0;
  if (n >= doc.lines) return doc.length;
  return doc.line(n + 1).from;
}

/**
 * Builds line-highlight + alignment-padding decorations for one side of the diff.
 * Hunks are assumed contiguous and gapless across the whole file (diff-core guarantees this).
 *
 * Padding is applied as a `padding-top`/`padding-bottom` line attribute, not a block-widget
 * decoration. CM6's height model only switches to its expensive non-uniform-height layout mode
 * when the document contains a block widget or replace decoration (see docs/PROFILING.md) — a
 * plain CSS padding on a line attribute never triggers that switch, since CM6 doesn't inspect
 * arbitrary line CSS when deciding whether every line shares one fixed height. The browser still
 * lays the padding out normally, so panes stay visually aligned without paying the layout-mode
 * cost that made the original block-widget approach not scale (see docs/PROFILING.md's
 * discriminating-probes table for why per-widget cost wasn't the driver).
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
      const line = doc.lineAt(pos);
      const px = pad * LINE_HEIGHT_PX;
      // pos sits at the next line's start unless the gap trails the very last line of the
      // document, in which case there is no "next line" to push down — grow the last line's
      // own box downward instead. padding (not margin) so the .diff-pad hatch background below
      // still paints the filler space; padding is just as invisible to CM6's height model.
      const edge = pos === line.from ? "top" : "bottom";
      builder.add(
        line.from,
        line.from,
        Decoration.line({ attributes: { class: "diff-pad", style: `padding-${edge}: ${px}px` } }),
      );
    }
  }

  return builder.finish();
}

export interface LinePair {
  leftLine: number;
  rightLine: number;
}

/**
 * Which line pairs within a `Replace` hunk need intra-line diffing, restricted to the ones
 * whose *own-side* (`side`) line number falls in `[fromLine, toLine]`. This is the selection
 * half of docs/PLAN.md §6's "viewport-driven, not eager" requirement — call it with the current
 * viewport's line range, not the whole document, or every Replace hunk in a 100k-line file gets
 * diffed on load regardless of visibility.
 *
 * When a Replace hunk's sides have unequal line counts, only the first `min(left.len, right.len)`
 * lines pair up 1:1 by position; the extra lines on the longer side have no intra-line
 * counterpart (they're already visually distinguished by the line-highlight + padding
 * decorations `buildDecorations` applies).
 */
export function replaceLinePairsInRange(hunks: Hunk[], side: "left" | "right", fromLine: number, toLine: number): LinePair[] {
  const pairs: LinePair[] = [];
  for (const h of hunks) {
    if (h.kind !== "replace") continue;
    const n = Math.min(h.left.len, h.right.len);
    for (let i = 0; i < n; i++) {
      const leftLine = h.left.start + i + 1;
      const rightLine = h.right.start + i + 1;
      const ownLine = side === "left" ? leftLine : rightLine;
      if (ownLine >= fromLine && ownLine <= toLine) {
        pairs.push({ leftLine, rightLine });
      }
    }
  }
  return pairs;
}

export interface MarkRange {
  from: number;
  to: number;
}

/**
 * Converts intra-line `Span`s (UTF-16 offsets within one line's text, as returned by the
 * `intra_line_spans` Tauri command) into absolute CM6 document positions for one specific line
 * on one specific side. JS strings are UTF-16 already, so `line.from + startUtf16` needs no
 * further unit conversion.
 *
 * Defensive against a stale/out-of-bounds span (e.g. a future cache entry surviving past an
 * edit that shortened the line) rather than letting CM6 throw on an invalid range.
 */
export function spansToMarkRanges(spans: Span[], doc: Text, lineNo: number, side: "left" | "right"): MarkRange[] {
  if (lineNo < 1 || lineNo > doc.lines) return [];
  const line = doc.line(lineNo);
  const ranges: MarkRange[] = [];
  for (const span of spans) {
    if (span.side !== side) continue;
    const from = line.from + span.startUtf16;
    const to = from + span.lenUtf16;
    if (to > line.to) continue;
    ranges.push({ from, to });
  }
  return ranges;
}

export type SpansFetcher = (leftLine: string, rightLine: string) => Promise<Span[]>;

/**
 * Dispatched whenever the hunk list changes — currently only from a whitespace/case-ignore
 * toggle re-diffing the same two texts. Both the main decoration field and the intra-line
 * highlighter below listen for this on the same view, so one dispatch keeps every
 * hunk-derived visual in sync. See `updateHunks`.
 */
export const setHunks = StateEffect.define<Hunk[]>();

const setIntraLineDecorations = StateEffect.define<DecorationSet>();

/**
 * Recomputes everything `createDiffEditor` derives from the hunk list except intra-line spans
 * (those are fetched lazily and live in their own field, since they depend on data the hunk
 * list alone doesn't carry). Combines two independently-built decoration sets via `RangeSet.join`
 * rather than one shared builder pass, since padding/collapse are conceptually separate steps
 * (and collapse is optional) — merging is O(n) and CM6-provided, not worth folding into one loop.
 */
function computeMainDecorations(doc: Text, hunks: Hunk[], side: "left" | "right", disablePadding: boolean, collapseEqual: boolean): DecorationSet {
  const main = buildDecorations(doc, hunks, side, disablePadding);
  const collapse = collapseEqual ? buildCollapseDecorations(doc, hunks, side) : Decoration.none;
  return RangeSet.join([main, collapse]);
}

function mainDecorationsField(side: "left" | "right", disablePadding: boolean, collapseEqual: boolean) {
  return StateField.define<DecorationSet>({
    create: () => Decoration.none, // overridden per-instance via `.init(...)` in createDiffEditor
    update(value, tr) {
      for (const effect of tr.effects) {
        if (effect.is(setHunks)) value = computeMainDecorations(tr.state.doc, effect.value, side, disablePadding, collapseEqual);
      }
      return value;
    },
    provide: (field) => EditorView.decorations.from(field),
  });
}

/**
 * CM6 extension applying character-level highlight marks to `Replace`-hunk lines, fetched
 * lazily per docs/PLAN.md §6 as they scroll into view rather than eagerly for the whole file.
 * `getOtherDoc` returns the opposite pane's *current* document — needed because a line pair's
 * spans depend on both sides' text, not just this pane's own. It's a live accessor rather than
 * a `Text` snapshot captured once at construction: M2 makes both panes editable, so the other
 * side's document can change after this highlighter is created, and a captured snapshot would
 * silently go stale the moment the other pane is edited (same failure shape as the earlier
 * `Span` serde bug — wrong output with no error at the boundary). The hunk list can also change,
 * via a dispatched `setHunks` effect (a toggle or an edit re-diffing the text) — the cache is
 * keyed by line-number pairs, which point at different content once hunks change, so it's
 * invalidated whenever that happens rather than only tracking viewport changes.
 */
export function intraLineHighlighter(
  initialHunks: Hunk[],
  side: "left" | "right",
  getOtherDoc: () => Text,
  fetchSpans: SpansFetcher,
  onFetchError?: (message: string) => void,
) {
  let hunks = initialHunks;
  const cache = new Map<string, Span[]>();
  const pending = new Set<string>();

  function rebuild(view: EditorView): DecorationSet {
    const builder = new RangeSetBuilder<Decoration>();
    const fromLine = view.state.doc.lineAt(view.viewport.from).number;
    const toLine = view.state.doc.lineAt(view.viewport.to).number;
    const ranges: MarkRange[] = [];
    for (const pair of replaceLinePairsInRange(hunks, side, fromLine, toLine)) {
      const spans = cache.get(`${pair.leftLine}:${pair.rightLine}`);
      if (!spans) continue;
      const ownLine = side === "left" ? pair.leftLine : pair.rightLine;
      ranges.push(...spansToMarkRanges(spans, view.state.doc, ownLine, side));
    }
    ranges.sort((a, b) => a.from - b.from);
    for (const r of ranges) builder.add(r.from, r.to, Decoration.mark({ class: "diff-intra" }));
    return builder.finish();
  }

  const plugin = ViewPlugin.fromClass(
    class {
      constructor(view: EditorView) {
        this.schedule(view);
      }
      update(update: ViewUpdate) {
        let hunksChanged = false;
        for (const tr of update.transactions) {
          for (const effect of tr.effects) {
            if (effect.is(setHunks)) {
              hunks = effect.value;
              hunksChanged = true;
            }
          }
        }
        // The field clears its own decorations synchronously in reaction to the same
        // `setHunks` effect (see below) — no re-dispatch needed here, which matters because
        // dispatching from inside a ViewPlugin's update() while one is already in progress is
        // unsafe. This only clears this plugin's own in-memory cache/pending set and kicks off
        // fresh fetches for the new hunk list.
        if (hunksChanged) {
          cache.clear();
          pending.clear();
        }
        if (update.viewportChanged || hunksChanged) this.schedule(update.view);
      }
      schedule(view: EditorView) {
        const fromLine = view.state.doc.lineAt(view.viewport.from).number;
        const toLine = view.state.doc.lineAt(view.viewport.to).number;
        const otherDoc = getOtherDoc();
        for (const pair of replaceLinePairsInRange(hunks, side, fromLine, toLine)) {
          const key = `${pair.leftLine}:${pair.rightLine}`;
          if (cache.has(key) || pending.has(key)) continue;
          // `hunks` can briefly lag behind either pane's *current* document while an edit on
          // either side is mid-flight (the debounced re-diff hasn't landed and dispatched a
          // fresh `setHunks` yet) -- skip a pair a stale hunk list points past the end of
          // either document instead of throwing, since the upcoming `setHunks` will invalidate
          // this cache entry and reschedule against the corrected hunk list anyway.
          if (pair.leftLine > view.state.doc.lines && side === "left") continue;
          if (pair.rightLine > view.state.doc.lines && side === "right") continue;
          if (pair.leftLine > otherDoc.lines && side === "right") continue;
          if (pair.rightLine > otherDoc.lines && side === "left") continue;
          pending.add(key);
          const leftText = side === "left" ? view.state.doc.line(pair.leftLine).text : otherDoc.line(pair.leftLine).text;
          const rightText = side === "right" ? view.state.doc.line(pair.rightLine).text : otherDoc.line(pair.rightLine).text;
          fetchSpans(leftText, rightText)
            .then((spans) => {
              pending.delete(key);
              cache.set(key, spans);
              view.dispatch({ effects: setIntraLineDecorations.of(rebuild(view)) });
            })
            .catch((err) => {
              pending.delete(key);
              console.error("intra-line fetch failed", key, err);
              onFetchError?.(err instanceof Error ? `${err.message}\n${err.stack}` : String(err));
            });
        }
      }
    },
  );

  const intraLineField = StateField.define<DecorationSet>({
    create: () => Decoration.none,
    update(value, tr) {
      for (const effect of tr.effects) {
        // `setHunks` invalidates in the same transaction, synchronously — a stale span cache
        // keyed by line-number pairs would otherwise point at different content once hunks
        // change, and re-dispatching from inside this plugin's own update() to clear it later
        // would be a reentrant dispatch.
        if (effect.is(setHunks)) value = Decoration.none;
        if (effect.is(setIntraLineDecorations)) value = effect.value;
      }
      return value;
    },
    provide: (field) => EditorView.decorations.from(field),
  });

  return [intraLineField, plugin];
}

/** Re-diffed hunks (e.g. from a whitespace/case-ignore toggle) — updates every hunk-derived
 * decoration on `view` in one dispatch, without recreating the editor or losing scroll position. */
export function updateHunks(view: EditorView, hunks: Hunk[]): void {
  view.dispatch({ effects: setHunks.of(hunks) });
}

export const COLLAPSE_CONTEXT_LINES = 3;
export const COLLAPSE_MIN_HUNK_LINES = 20;

export interface CollapseRange {
  fromLine: number;
  toLine: number;
}

/**
 * Which line ranges within large `Equal` hunks are candidates for collapsing, leaving
 * `COLLAPSE_CONTEXT_LINES` of real content visible at each edge (matching the conventional
 * diff-context idea). Pure and side-agnostic from the caller's hunk-list-and-side inputs so it's
 * unit testable without CM6 involved at all — the CM6 decoration-building step is separate.
 *
 * Never touches a non-`Equal` hunk, and always leaves context lines untouched at both edges of
 * a collapsed run — by construction this never overlaps a `buildDecorations` padding
 * attribute, since padding is only ever anchored at a hunk boundary (the first line of the
 * following hunk, or the document's last line), never inside the middle of a run.
 */
export function buildCollapseRanges(doc: Text, hunks: Hunk[], side: "left" | "right"): CollapseRange[] {
  const ranges: CollapseRange[] = [];
  for (const h of hunks) {
    if (h.kind !== "equal") continue;
    const range = side === "left" ? h.left : h.right;
    if (range.len <= COLLAPSE_MIN_HUNK_LINES) continue;
    const fromLine = range.start + COLLAPSE_CONTEXT_LINES + 1;
    const toLine = range.start + range.len - COLLAPSE_CONTEXT_LINES;
    if (toLine < fromLine || toLine > doc.lines) continue;
    ranges.push({ fromLine, toLine });
  }
  return ranges;
}

export class CollapseWidget extends WidgetType {
  constructor(readonly hiddenLines: number) {
    super();
  }
  eq(other: CollapseWidget) {
    return other.hiddenLines === this.hiddenLines;
  }
  toDOM() {
    const div = document.createElement("div");
    div.className = "diff-collapse";
    div.textContent = `⋯ ${this.hiddenLines} unchanged lines ⋯`;
    return div;
  }
  ignoreEvent() {
    return true;
  }
}

/**
 * Collapses large unchanged (`Equal`) regions to a one-line placeholder, per docs/PLAN.md §6.
 * `Decoration.replace({block: true})` is the only CM6 mechanism for hiding a multi-line range —
 * before shipping this, it was A/B measured (see DECISIONS.md and `bench/m0-spike.mjs
 * --collapse-equal`) against the exact regression the padding-widget investigation
 * (docs/PROFILING.md) found for `Decoration.widget({block: true})`, since both are block-level
 * decorations. Measured result on the 100k-line fixture with 392 collapsed ranges: fps and paint
 * time both stayed within noise of the no-collapse baseline — unlike widgets, a `replace`
 * decoration removes its range from layout instead of requiring CM6 to track a heterogeneous
 * height for content around it, so it never triggers the same non-uniform-height mode.
 *
 * Not yet interactive — there is no click-to-expand. See DECISIONS.md for why that's deferred
 * rather than silently missing.
 */
export function buildCollapseDecorations(doc: Text, hunks: Hunk[], side: "left" | "right"): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  for (const { fromLine, toLine } of buildCollapseRanges(doc, hunks, side)) {
    const from = doc.line(fromLine).from;
    const to = doc.line(toLine).to;
    builder.add(from, to, Decoration.replace({ block: true, widget: new CollapseWidget(toLine - fromLine + 1) }));
  }
  return builder.finish();
}

export interface ChangeHunkLine {
  left: number;
  right: number;
}

/** First line of each non-equal hunk, in document order — the ordered stop list for hunk
 * navigation (`nextHunkIndex`/`prevHunkIndex`). */
export function changeHunkLines(hunks: Hunk[]): ChangeHunkLine[] {
  return hunks.filter((h) => h.kind !== "equal").map((h) => ({ left: h.left.start + 1, right: h.right.start + 1 }));
}

/** Wraps forward through `[0, count)`; `-1` (nothing selected yet) advances to the first hunk.
 * `count === 0` (no changes at all) returns `-1` in both directions — there is nothing to go to. */
export function nextHunkIndex(count: number, current: number): number {
  if (count === 0) return -1;
  return (current + 1) % count;
}

export function prevHunkIndex(count: number, current: number): number {
  if (count === 0) return -1;
  if (current === -1) return count - 1; // nothing selected yet -> start from the end
  return (current - 1 + count) % count;
}

/** Scrolls a hunk's start line to the vertical center of the viewport. Only needs to be called
 * on one pane — `syncScroll` mirrors the resulting scrollTop to the other. */
export function scrollToLine(view: EditorView, lineNo: number): void {
  if (lineNo < 1 || lineNo > view.state.doc.lines) return;
  const pos = view.state.doc.line(lineNo).from;
  view.dispatch({ effects: EditorView.scrollIntoView(pos, { y: "center" }) });
}

export interface MinimapSegment {
  kind: "insert" | "delete" | "replace";
  startFrac: number;
  lenFrac: number;
}

/**
 * Fractional (0-1) positions for a single shared overview strip, per the approved UI model
 * (docs/UI/ui-01.png) — one strip for the whole diff, not one per pane. Positioned along the
 * *left* side's line numbers: a pure Insert hunk has `left.len === 0`, so its fraction is 0 —
 * deliberately a true proportional value, not inflated to an arbitrary minimum here. A fixed
 * *fraction* minimum (e.g. 1% of the document) looks fine on a short file but, on a 100k-line
 * file, 1% is 1000 lines — enough to make every real hunk's true size disappear under an
 * oversized block and merge adjacent hunks into a solid bar (caught by actually rendering this
 * against the 100k fixture, not just testing the math in isolation). Making near-zero segments
 * visible is instead the rendering layer's job (`.minimap-segment { min-height: 2px }` in
 * +page.svelte) — a fixed pixel floor scales correctly at any document size, since it doesn't
 * depend on total line count at all.
 */
export function computeMinimapSegments(hunks: Hunk[], totalLines: number): MinimapSegment[] {
  if (totalLines <= 0) return [];
  const segments: MinimapSegment[] = [];
  for (const h of hunks) {
    if (h.kind === "equal") continue;
    segments.push({ kind: h.kind, startFrac: h.left.start / totalLines, lenFrac: h.left.len / totalLines });
  }
  return segments;
}

export interface ViewportIndicator {
  topFrac: number;
  heightFrac: number;
}

/** Well-defined (not NaN/Infinity) when there's nothing to scroll, so the minimap can always
 * render a viewport rectangle without a special-case check at the call site. */
export function computeViewportIndicator(scrollTop: number, scrollHeight: number, clientHeight: number): ViewportIndicator {
  if (scrollHeight <= 0) return { topFrac: 0, heightFrac: 1 };
  return { topFrac: scrollTop / scrollHeight, heightFrac: Math.min(1, clientHeight / scrollHeight) };
}

/** Inverse of the positioning `computeMinimapSegments` uses: a click at fractional position
 * `clickFrac` down the strip jumps to this 1-indexed line, clamped to the document's bounds. */
export function minimapClickToLine(clickFrac: number, totalLines: number): number {
  const line = Math.round(clickFrac * totalLines) + 1;
  return Math.max(1, Math.min(totalLines, line));
}

/** One CM6 change, in the UTF-16-offset delta shape the IPC boundary uses (docs/PLAN.md §3).
 * `fromUtf16`/`toUtf16` are positions in the document as it stood immediately before this delta
 * — i.e. valid against the Rust-side `EditBuffer` only if every prior delta (from this same
 * batch or an earlier one) has already been applied to it, in order. */
export interface EditDelta {
  fromUtf16: number;
  toUtf16: number;
  inserted: string;
}

/**
 * Decomposes one CM6 update into the ordered list of `EditDelta`s the Rust-side `EditBuffer`
 * needs, per docs/PLAN.md §2's "incremental deltas, never a full-document resend" constraint.
 *
 * `update.changes.iterChanges` yields each change's `fromA`/`toA` relative to the *original*
 * pre-update document — correct for the first change in the batch, but not for the second and
 * later ones once the first has been (conceptually) applied. Since deltas are applied to the
 * Rust rope one at a time and in order, each one after the first needs its position shifted by
 * the net length change every earlier delta *in this same batch* already introduced (its
 * inserted length minus its deleted length) — otherwise the second delta in a multi-change
 * keystroke (e.g. autocomplete replacing a selection, or two simultaneous cursors) would land
 * at the wrong offset against a rope that's already been mutated by the first.
 */
export function editDeltasFromUpdate(update: ViewUpdate): EditDelta[] {
  const deltas: EditDelta[] = [];
  let shift = 0;
  update.changes.iterChanges((fromA, toA, fromB, toB, insertedText) => {
    deltas.push({ fromUtf16: fromA + shift, toUtf16: toA + shift, inserted: insertedText.toString() });
    shift += (toB - fromB) - (toA - fromA);
  });
  return deltas;
}

export function createDiffEditor(
  parent: HTMLElement,
  text: string,
  hunks: Hunk[],
  side: "left" | "right",
  disablePadding = false,
  intraLine?: { getOtherDoc: () => Text; fetchSpans: SpansFetcher; onFetchError?: (message: string) => void },
  collapseEqual = false,
  editable = false,
  onEdit?: (deltas: EditDelta[]) => void,
): EditorView {
  const doc = Text.of(text.split("\n"));
  const field = mainDecorationsField(side, disablePadding, collapseEqual);

  const state = EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      keymap.of(defaultKeymap),
      javascript(),
      EditorState.readOnly.of(!editable),
      field.init(() => computeMainDecorations(doc, hunks, side, disablePadding, collapseEqual)),
      ...(intraLine
        ? intraLineHighlighter(hunks, side, intraLine.getOtherDoc, intraLine.fetchSpans, intraLine.onFetchError)
        : []),
      // Guarded on `docChanged`, which is false for the decoration-only transactions `setHunks`/
      // `setIntraLineDecorations` dispatch (they carry effects, not changes) -- without this
      // guard, applying a hunk-refresh or intra-line-highlight update would be misread as a
      // user edit and echoed back to Rust, corrupting the shadow buffer silently (it wouldn't
      // throw; it would just make a later save write the wrong bytes).
      ...(editable && onEdit
        ? [EditorView.updateListener.of((update: ViewUpdate) => {
            if (!update.docChanged) return;
            onEdit!(editDeltasFromUpdate(update));
          })]
        : []),
      EditorView.theme({
        "&": { height: "100%", fontSize: "13px" },
        ".cm-scroller": { overflow: "auto", fontFamily: "ui-monospace, Menlo, monospace" },
        // Pinned so it exactly matches LINE_HEIGHT_PX rather than trusting the browser
        // default to happen to agree with it.
        ".cm-line": { lineHeight: `${LINE_HEIGHT_PX}px` },
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
