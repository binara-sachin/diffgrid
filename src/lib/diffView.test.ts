import { describe, it, expect } from "vitest";
import { Text } from "@codemirror/state";
import { LINE_HEIGHT_PX, posAfterLine, buildDecorations, replaceLinePairsInRange, spansToMarkRanges } from "./diffView";
import type { Hunk, Span } from "./types";

describe("posAfterLine", () => {
  const doc = Text.of(["a", "b", "c", "d", "e"]);

  it("returns the start of the document for zero lines consumed", () => {
    expect(posAfterLine(doc, 0)).toBe(0);
  });

  it("returns the start of the next line partway through the document", () => {
    expect(posAfterLine(doc, 2)).toBe(doc.line(3).from);
  });

  it("returns the document end once all lines are consumed", () => {
    expect(posAfterLine(doc, doc.lines)).toBe(doc.length);
    expect(posAfterLine(doc, doc.lines + 10)).toBe(doc.length);
  });
});

describe("buildDecorations", () => {
  // left: 3 unchanged, 2 replaced-with-4, 3 unchanged = 8 lines
  // right:                  4 replaced lines               = 9 lines
  const hunks: Hunk[] = [
    { kind: "equal", left: { start: 0, len: 3 }, right: { start: 0, len: 3 } },
    { kind: "replace", left: { start: 3, len: 2 }, right: { start: 3, len: 4 } },
    { kind: "equal", left: { start: 5, len: 3 }, right: { start: 7, len: 3 } },
  ];
  const leftDoc = Text.of(["u0", "u1", "u2", "L0", "L1", "u3", "u4", "u5"]);
  const rightDoc = Text.of(["u0", "u1", "u2", "R0", "R1", "R2", "R3", "u3", "u4", "u5"]);

  function collect(doc: Text, side: "left" | "right", disablePadding = false) {
    const set = buildDecorations(doc, hunks, side, disablePadding);
    const lines: number[] = [];
    const margins: { pos: number; style: string }[] = [];
    set.between(0, doc.length, (from, _to, deco) => {
      const style = deco.spec.attributes?.style;
      if (style) margins.push({ pos: from, style });
      else lines.push(from);
    });
    return { lines, margins };
  }

  it("highlights every real line in a non-equal hunk on the shorter (left) side", () => {
    const { lines } = collect(leftDoc, "left");
    expect(lines).toEqual([leftDoc.line(4).from, leftDoc.line(5).from]); // L0, L1
  });

  it("highlights every real line in a non-equal hunk on the longer (right) side", () => {
    const { lines } = collect(rightDoc, "right");
    expect(lines).toEqual([rightDoc.line(4).from, rightDoc.line(5).from, rightDoc.line(6).from, rightDoc.line(7).from]);
  });

  it("pads the shorter side via a padding-top on the line right after the gap, not a block widget", () => {
    const { margins } = collect(leftDoc, "left");
    // right has 2 more lines than left; the gap trails line 6 ("u3"), so line 7 ("u4") gets pushed down
    expect(margins).toEqual([{ pos: leftDoc.line(6).from, style: `padding-top: ${2 * LINE_HEIGHT_PX}px` }]);
  });

  it("pads via padding-bottom on the last line when the gap falls at document end", () => {
    // left: 1 unchanged line, right has 3 extra trailing lines with no left-side line to push down
    const endHunks: Hunk[] = [
      { kind: "equal", left: { start: 0, len: 1 }, right: { start: 0, len: 1 } },
      { kind: "insert", left: { start: 1, len: 0 }, right: { start: 1, len: 3 } },
    ];
    const doc = Text.of(["u0"]);
    const set = buildDecorations(doc, endHunks, "left");
    const found: { pos: number; style: string }[] = [];
    set.between(0, doc.length, (from, _to, deco) => {
      const style = deco.spec.attributes?.style;
      if (style) found.push({ pos: from, style });
    });
    expect(found).toEqual([{ pos: doc.line(1).from, style: `padding-bottom: ${3 * LINE_HEIGHT_PX}px` }]);
  });

  it("does not pad the longer side", () => {
    const { margins } = collect(rightDoc, "right");
    expect(margins).toEqual([]);
  });

  it("omits padding entirely when disablePadding is set", () => {
    const { margins } = collect(leftDoc, "left", true);
    expect(margins).toEqual([]);
  });

  it("never decorates an equal hunk", () => {
    const { lines: leftLines } = collect(leftDoc, "left");
    const { lines: rightLines } = collect(rightDoc, "right");
    // only the 2 left / 4 right replace-hunk lines should be decorated, never the equal runs
    expect(leftLines.length).toBe(2);
    expect(rightLines.length).toBe(4);
  });
});

describe("replaceLinePairsInRange", () => {
  // hunk 1: a 1:1 replace (left line 2 <-> right line 2)
  // hunk 2: an unequal replace (left lines 5-6 <-> right lines 5-7); only min(2,3)=2 lines pair
  const hunks: Hunk[] = [
    { kind: "equal", left: { start: 0, len: 1 }, right: { start: 0, len: 1 } },
    { kind: "replace", left: { start: 1, len: 1 }, right: { start: 1, len: 1 } },
    { kind: "equal", left: { start: 2, len: 2 }, right: { start: 2, len: 2 } },
    { kind: "replace", left: { start: 4, len: 2 }, right: { start: 4, len: 3 } },
  ];

  it("never pairs lines from an equal or pure insert/delete hunk", () => {
    expect(replaceLinePairsInRange(hunks, "left", 1, 100)).not.toContainEqual(
      expect.objectContaining({ leftLine: 1 }),
    );
  });

  it("pairs a simple 1:1 replace by line number", () => {
    expect(replaceLinePairsInRange(hunks, "left", 1, 100)).toContainEqual({ leftLine: 2, rightLine: 2 });
  });

  it("pairs only min(left.len, right.len) lines when a replace hunk's sides are unequal", () => {
    const pairs = replaceLinePairsInRange(hunks, "left", 1, 100).filter((p) => p.leftLine >= 5);
    expect(pairs).toEqual([
      { leftLine: 5, rightLine: 5 },
      { leftLine: 6, rightLine: 6 },
    ]);
    // right line 7 has no left counterpart in this hunk and must never appear
    expect(pairs.some((p) => p.rightLine === 7)).toBe(false);
  });

  it("filters by the requested side's own line range, not the other side's", () => {
    // right-side viewport [2,2] should still find the pair anchored at left line 2
    expect(replaceLinePairsInRange(hunks, "right", 2, 2)).toEqual([{ leftLine: 2, rightLine: 2 }]);
    expect(replaceLinePairsInRange(hunks, "right", 3, 3)).toEqual([]);
  });

  it("returns nothing when the range excludes every replace hunk", () => {
    expect(replaceLinePairsInRange(hunks, "left", 3, 3)).toEqual([]);
  });
});

describe("spansToMarkRanges", () => {
  const doc = Text.of(["one", "hello world", "three"]);

  it("converts a span's UTF-16 offset within a line into absolute document positions", () => {
    const spans: Span[] = [{ side: "left", startUtf16: 6, lenUtf16: 5 }]; // "world" in line 2
    const ranges = spansToMarkRanges(spans, doc, 2, "left");
    const line = doc.line(2);
    expect(ranges).toEqual([{ from: line.from + 6, to: line.from + 11 }]);
  });

  it("only includes spans belonging to the requested side", () => {
    const spans: Span[] = [
      { side: "left", startUtf16: 0, lenUtf16: 3 },
      { side: "right", startUtf16: 0, lenUtf16: 3 },
    ];
    expect(spansToMarkRanges(spans, doc, 1, "right")).toEqual([{ from: doc.line(1).from, to: doc.line(1).from + 3 }]);
  });

  it("drops a span that would run past the end of the target line rather than corrupt the range", () => {
    // defensive: a stale cache entry from before a (hypothetical future) edit should never
    // produce an out-of-bounds CM6 range
    const spans: Span[] = [{ side: "left", startUtf16: 0, lenUtf16: 999 }];
    expect(spansToMarkRanges(spans, doc, 1, "left")).toEqual([]);
  });

  it("returns nothing for an out-of-range line number", () => {
    const spans: Span[] = [{ side: "left", startUtf16: 0, lenUtf16: 1 }];
    expect(spansToMarkRanges(spans, doc, 999, "left")).toEqual([]);
  });
});
