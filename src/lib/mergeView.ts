import { RangeSetBuilder, StateEffect, StateField, Text } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView } from "@codemirror/view";
import type { LineRange, MergeHunk, TakeBothSide } from "./types";
import { posAfterLine } from "./diffView";

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
 * A hunk with `len === 0` (a pure insertion resolved to nothing, e.g. an empty `TakeBoth` side)
 * is skipped -- CM6 can't usefully mark a zero-length range for click targeting, and there's
 * nothing to click on since it renders no visible lines.
 */
export function buildMergeHunkDecorations(doc: Text, ranges: LineRange[], hunks: MergeHunk[]): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  const entries = ranges.map((r, i) => ({ r, i })).sort((a, b) => a.r.start - b.r.start);
  for (const { r, i } of entries) {
    if (r.len === 0) continue;
    const from = posAfterLine(doc, r.start);
    const to = posAfterLine(doc, r.start + r.len);
    const hunk = hunks[i];
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
