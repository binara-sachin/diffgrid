import { EditorState, RangeSetBuilder, StateEffect, StateField, Text } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, type ViewUpdate, lineNumbers, keymap } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import type { LineRange, MergeHunk, TakeBothSide } from "./types";
import { LINE_HEIGHT_PX, type EditDelta, editDeltasFromUpdate, posAfterLine } from "./diffView";

function extractLines(text: string, range: LineRange): string {
  const lines = text.split("\n");
  const start = Math.min(range.start, lines.length);
  const end = Math.min(range.start + range.len, lines.length);
  return lines.slice(start, end).join("\n");
}

/**
 * Client-side mirror of `merge_core::resolve_hunk_text` -- the frontend already has
 * base/local/remote text resident (fetched once at merge-view open, per docs/PLAN.md §3's
 * "full text crosses the boundary once" rule), so recomputing a hunk's resolved text here avoids
 * an IPC round trip purely to re-fetch a string this process already holds. Must stay in sync
 * with the Rust implementation's exact behavior for TakeBoth's ordering and empty-side handling --
 * both are covered by `mergeView.test.ts`'s parity tests against fixtures also exercised in
 * `merge-core`'s own Rust tests.
 */
export function resolveHunkText(base: string, local: string, remote: string, hunk: MergeHunk, takeBothSide: TakeBothSide): string {
  switch (hunk.resolution) {
    case "takeLocal":
      return extractLines(local, hunk.local);
    case "takeRemote":
      return extractLines(remote, hunk.remote);
    case "takeBoth": {
      const localText = extractLines(local, hunk.local);
      const remoteText = extractLines(remote, hunk.remote);
      const [first, second] = takeBothSide === "mineFirst" ? [localText, remoteText] : [remoteText, localText];
      if (!first) return second;
      if (!second) return first;
      return `${first}\n${second}`;
    }
    case "takeBase":
      return extractLines(base, hunk.base);
    case "manual":
      throw new Error("resolveHunkText: Manual resolution has no derivable text -- the live CM6 buffer is authoritative for it");
    case null:
      throw new Error("resolveHunkText: an unresolved Conflict hunk has no text to resolve to");
  }
}

/**
 * Each hunk's line range within the *initial* merged text seed (`OpenMergeResult.mergedText`),
 * computed by the same walk `build_merged_text` did in Rust -- an unresolved `Conflict` hunk
 * (`resolution: null`) is treated as base content here too, matching that function's own
 * documented placeholder behavior for first paint. Critically, this must also count the
 * unchanged *base* lines between one hunk's end and the next hunk's start -- `merge_hunks` only
 * lists lines where something actually changed, so a naive walk that sums only hunk content
 * (skipping those gaps) would place every hunk after the first at the wrong line.
 */
export function initialMergedHunkRanges(base: string, local: string, remote: string, hunks: MergeHunk[], takeBothSide: TakeBothSide): LineRange[] {
  const ranges: LineRange[] = [];
  let cursor = 0;
  let baseCursor = 0;
  for (const hunk of hunks) {
    cursor += hunk.base.start - baseCursor; // the unchanged base-line gap before this hunk
    const text = hunk.resolution === null ? extractLines(base, hunk.base) : resolveHunkText(base, local, remote, hunk, takeBothSide);
    const len = text === "" ? 0 : text.split("\n").length;
    ranges.push({ start: cursor, len });
    cursor += len;
    baseCursor = hunk.base.start + hunk.base.len;
  }
  return ranges;
}

const HUNK_INDEX_ATTR = "data-merge-hunk-index";

/**
 * One `Decoration.mark` per merge hunk, keyed to its index via a `data-merge-hunk-index`
 * attribute -- this both renders the conflict/auto-merged highlight AND is the sole position
 * tracker for that hunk's range in the live document. No separate Rust- or JS-side "current
 * position" bookkeeping exists (see DECISIONS.md's M5 entry on why): once this `DecorationSet`
 * is stored in a `StateField`, CM6's own `RangeSet.map()` keeps every hunk's boundaries correct
 * through every edit, the same mechanism the two-way diff view's own decoration fields already
 * rely on for line-highlight ranges.
 *
 * A hunk with `len === 0` that is already resolved (a pure insertion resolved to nothing, e.g.
 * an empty `TakeBoth` side) is skipped -- CM6 can't usefully mark a zero-length range for click
 * targeting, and there's nothing to click on since it renders no visible lines, and no future
 * action will ever need this position again once a hunk is resolved.
 *
 * An *unresolved* `Conflict` hunk (`resolution === null`) whose placeholder happens to be
 * zero-length (e.g. an add/add conflict where BASE is empty, so the first-paint placeholder --
 * base's own content -- is "") is NOT skipped: CM6 supports a zero-length `Decoration.mark`
 * range for tracking/click-targeting purposes even though it renders nothing (confirmed
 * empirically), and this hunk still needs a trackable position for the Take Local/Remote/Both/
 * Base click that will eventually insert real text there. Regression found by testing the M5
 * exit-status contract through a real `git mergetool` add/add conflict: without this, a
 * resolution click silently updated the frontend's own "resolved" bookkeeping without ever
 * inserting text into the merged buffer (since `hunkRangeAtIndex` had nothing to find), so Save
 * exited 0 and staged an empty file as the "resolved" merge result.
 */
export function buildMergeHunkDecorations(doc: Text, ranges: LineRange[], hunks: MergeHunk[]): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  const entries = ranges.map((r, i) => ({ r, i })).sort((a, b) => a.r.start - b.r.start);
  for (const { r, i } of entries) {
    const hunk = hunks[i];
    if (r.len === 0 && hunk.resolution !== null) continue;
    const from = posAfterLine(doc, r.start);
    const to = posAfterLine(doc, r.start + r.len);
    builder.add(
      from,
      to,
      Decoration.mark({
        class: `merge-hunk merge-hunk-${hunk.kind}${hunk.resolution ? ` merge-hunk-resolved-${hunk.resolution}` : ""}`,
        attributes: { [HUNK_INDEX_ATTR]: String(i) },
      }),
    );
  }
  return builder.finish();
}

/**
 * One `Decoration.mark` per hunk on a read-only source pane (base/local/remote), using that
 * hunk's `LineRange` for `side` -- unlike `buildMergeHunkDecorations`, this needs no live
 * position tracking (base/local/remote are read-only and never edited after open, so their line
 * ranges never drift), but shares the exact same `HUNK_INDEX_ATTR` convention and `hunkIndexAtPos`
 * lookup so a click on any of the four panes resolves to a hunk index the same way.
 */
export function buildSourcePaneDecorations(doc: Text, hunks: MergeHunk[], side: "base" | "local" | "remote"): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  const entries = hunks.map((h, i) => ({ range: h[side], hunk: h, i })).sort((a, b) => a.range.start - b.range.start);
  for (const { range, hunk, i } of entries) {
    if (range.len === 0) continue;
    const from = posAfterLine(doc, range.start);
    const to = posAfterLine(doc, range.start + range.len);
    builder.add(
      from,
      to,
      Decoration.mark({
        class: `merge-hunk merge-hunk-${hunk.kind}`,
        attributes: { [HUNK_INDEX_ATTR]: String(i) },
      }),
    );
  }
  return builder.finish();
}

/** Finds which hunk (by index into the original `hunks` array `buildMergeHunkDecorations` was
 * called with) covers `pos` in the live document, or `null` if none does -- e.g. a click in the
 * merged pane lands here to find which hunk's resolution-action buttons should be shown/enabled
 * for that position. */
export function hunkIndexAtPos(decorations: DecorationSet, pos: number): number | null {
  let found: number | null = null;
  decorations.between(pos, pos, (from, to, deco) => {
    if (pos < from || pos > to) return;
    const raw = (deco.spec.attributes as Record<string, string> | undefined)?.[HUNK_INDEX_ATTR];
    if (raw !== undefined) found = Number(raw);
  });
  return found;
}

/** Finds a hunk's current `{from, to}` character range by index (the reverse lookup of
 * `hunkIndexAtPos`) -- what a resolution-action click needs to know exactly which range in the
 * live merged document to replace. Returns `null` if that hunk has no decoration (a zero-length
 * hunk was never added by `buildMergeHunkDecorations`, so there's nothing to replace -- the
 * caller should insert at a computed position instead, not treat this as an error). Iterates
 * the whole set rather than a targeted lookup since `RangeSet` has no direct "get by attribute"
 * API; merge hunk counts are small (a handful to a few dozen per file), so this is not a
 * hot path worth optimizing. */
export function hunkRangeAtIndex(decorations: DecorationSet, index: number): { from: number; to: number } | null {
  let found: { from: number; to: number } | null = null;
  decorations.between(0, Infinity, (from, to, deco) => {
    const raw = (deco.spec.attributes as Record<string, string> | undefined)?.[HUNK_INDEX_ATTR];
    if (raw !== undefined && Number(raw) === index) found = { from, to };
  });
  return found;
}

/**
 * Builds the CM6 `{from, to, insert}` change to replace one hunk's current content with
 * `newText` -- the resolution-action click's counterpart to `buildHunkCopyChange` (M2's
 * copy-between-panes change builder). `hunkRangeAtIndex`'s range follows `posAfterLine`'s
 * existing convention of extending through the line's trailing newline (see
 * `buildHunkCopyChange`'s own tests), so `newText` needs its own trailing `\n` appended to match
 * -- except when the range reaches the end of the document (the last line has no newline after
 * it to replace), where appending one would introduce a newline that was never there. Returns
 * `null` if the hunk has no current decoration (see `hunkRangeAtIndex`).
 */
export function buildHunkResolutionChange(decorations: DecorationSet, doc: Text, hunkIndex: number, newText: string): { from: number; to: number; insert: string } | null {
  const range = hunkRangeAtIndex(decorations, hunkIndex);
  if (!range) return null;
  const atDocEnd = range.to >= doc.length;
  const insert = atDocEnd ? newText : `${newText}\n`;
  return { from: range.from, to: range.to, insert };
}

/**
 * Restyles one hunk's decoration in place after its `resolution`/`kind` changes, at whatever
 * position `RangeSet.map` has already carried it to -- CM6's own position-mapping correctly
 * *stretches* a mark decoration to cover a full-range replacement (confirmed empirically: a
 * transaction replacing an entire decorated range with different-length text yields one mapped
 * decoration spanning the new content, not a zero-length or dropped one), but does NOT update
 * the decoration's own `spec` (class/attributes) -- those are immutable per-decoration data, so a
 * hunk whose resolution just changed needs its *decoration object* swapped for a freshly-styled
 * one at the same range, while every other hunk's decoration is left completely alone. This is
 * cheaper than a full `buildMergeHunkDecorations` rebuild and, more importantly, doesn't need
 * every other hunk's *current* range recomputed (which nothing tracks outside this same
 * `DecorationSet` once the merged pane has been edited even once -- see `initialMergedHunkRanges`'s
 * doc comment on why it's only valid at open).
 */
export function replaceHunkDecoration(decorations: DecorationSet, hunkIndex: number, updatedHunk: MergeHunk): DecorationSet {
  const range = hunkRangeAtIndex(decorations, hunkIndex);
  if (!range) return decorations;
  const filtered = decorations.update({ filter: (_from, _to, deco) => (deco.spec.attributes as Record<string, string> | undefined)?.[HUNK_INDEX_ATTR] !== String(hunkIndex) });
  const restyled = Decoration.mark({
    class: `merge-hunk merge-hunk-${updatedHunk.kind}${updatedHunk.resolution ? ` merge-hunk-resolved-${updatedHunk.resolution}` : ""}`,
    attributes: { [HUNK_INDEX_ATTR]: String(hunkIndex) },
  });
  return filtered.update({ add: [restyled.range(range.from, range.to)] });
}

/** Replaces the whole `DecorationSet` -- used only at merge-view open (the initial seed) and
 * after a resolution change/manual edit changes which hunks exist or how they're classified.
 * Every other transaction (a plain keystroke that doesn't touch a hunk boundary) needs no
 * effect at all: `mergeHunkDecorationsField`'s `update` falls through to `value.map(tr.changes)`,
 * which is what keeps every untouched hunk's range correct for free. */
export const setMergeHunkDecorations = StateEffect.define<DecorationSet>();

export const mergeHunkDecorationsField = StateField.define<DecorationSet>({
  create: () => Decoration.none, // overridden via `.init(...)` at editor construction, same convention as diffView.ts's mainDecorationsField
  update(value, tr) {
    for (const effect of tr.effects) {
      if (effect.is(setMergeHunkDecorations)) return effect.value;
    }
    return value.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

function mergeTheme() {
  return EditorView.theme({
    "&": { height: "100%", fontSize: "13px" },
    ".cm-scroller": { overflow: "auto", fontFamily: "ui-monospace, Menlo, monospace" },
    ".cm-line": { lineHeight: `${LINE_HEIGHT_PX}px` },
  });
}

/** A read-only base/local/remote pane -- no editing, no padding/alignment (each of the three
 * source panes has its own independent line numbering; there's no cross-pane alignment
 * requirement for them the way M1-M4's two-way diff view has). */
export function createMergeSourceEditor(parent: HTMLElement, text: string, hunks: MergeHunk[], side: "base" | "local" | "remote"): EditorView {
  const doc = Text.of(text.split("\n"));
  const state = EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      EditorState.readOnly.of(true),
      mergeHunkDecorationsField.init(() => buildSourcePaneDecorations(doc, hunks, side)),
      mergeTheme(),
    ],
  });
  return new EditorView({ state, parent });
}

/** The editable merged-output pane, seeded from `OpenMergeResult.mergedText`. `initialRanges`
 * (from `initialMergedHunkRanges`) and `hunks` build the initial `DecorationSet` internally, the
 * same convention `createMergeSourceEditor` uses -- callers never construct a CM6 `Text`/
 * `DecorationSet` themselves. `onEdit` fires for every real keystroke or programmatic replace (a
 * resolution-action click's own dispatch) alike -- the caller is responsible for calling
 * `mark_merge_hunk_manual` only for the former, since a resolution click already sets a
 * non-Manual resolution via `resolve_merge_hunk` before dispatching its own change. */
export function createMergedEditor(parent: HTMLElement, mergedText: string, initialRanges: LineRange[], hunks: MergeHunk[], onEdit: (deltas: EditDelta[]) => void): EditorView {
  const doc = Text.of(mergedText.split("\n"));
  const state = EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      keymap.of(defaultKeymap),
      javascript(),
      mergeHunkDecorationsField.init(() => buildMergeHunkDecorations(doc, initialRanges, hunks)),
      EditorView.updateListener.of((update: ViewUpdate) => {
        if (!update.docChanged) return;
        onEdit(editDeltasFromUpdate(update));
      }),
      mergeTheme(),
    ],
  });
  return new EditorView({ state, parent });
}
