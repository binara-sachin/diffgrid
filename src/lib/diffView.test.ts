import { describe, it, expect } from "vitest";
import { Text } from "@codemirror/state";
import {
  LINE_HEIGHT_PX,
  posAfterLine,
  buildDecorations,
  replaceLinePairsInRange,
  spansToMarkRanges,
  buildCollapseRanges,
  COLLAPSE_CONTEXT_LINES,
  COLLAPSE_MIN_HUNK_LINES,
  createDiffEditor,
  updateHunks,
  changeHunkLines,
  nextHunkIndex,
  prevHunkIndex,
  computeMinimapSegments,
  computeViewportIndicator,
  minimapClickToLine,
} from "./diffView";
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

describe("buildCollapseRanges", () => {
  function doc(lines: number) {
    return Text.of(Array.from({ length: lines }, (_, i) => `line${i + 1}`));
  }
  const bigLen = COLLAPSE_MIN_HUNK_LINES + 10;

  it("never collapses a hunk at or below the minimum size", () => {
    const hunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: COLLAPSE_MIN_HUNK_LINES }, right: { start: 0, len: COLLAPSE_MIN_HUNK_LINES } }];
    expect(buildCollapseRanges(doc(COLLAPSE_MIN_HUNK_LINES), hunks, "left")).toEqual([]);
  });

  it("collapses a large equal hunk, leaving context lines visible on both edges", () => {
    const hunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: bigLen }, right: { start: 0, len: bigLen } }];
    const ranges = buildCollapseRanges(doc(bigLen), hunks, "left");
    expect(ranges).toEqual([{ fromLine: COLLAPSE_CONTEXT_LINES + 1, toLine: bigLen - COLLAPSE_CONTEXT_LINES }]);
  });

  it("never collapses a non-equal hunk regardless of length", () => {
    const hunks: Hunk[] = [{ kind: "insert", left: { start: 0, len: 0 }, right: { start: 0, len: bigLen } }];
    expect(buildCollapseRanges(doc(bigLen), hunks, "right")).toEqual([]);
  });

  it("produces one independent range per large equal hunk", () => {
    const hunks: Hunk[] = [
      { kind: "equal", left: { start: 0, len: bigLen }, right: { start: 0, len: bigLen } },
      { kind: "replace", left: { start: bigLen, len: 1 }, right: { start: bigLen, len: 1 } },
      { kind: "equal", left: { start: bigLen + 1, len: bigLen }, right: { start: bigLen + 1, len: bigLen } },
    ];
    const totalLines = bigLen * 2 + 1;
    const ranges = buildCollapseRanges(doc(totalLines), hunks, "left");
    expect(ranges).toHaveLength(2);
    expect(ranges[0].fromLine).toBe(COLLAPSE_CONTEXT_LINES + 1);
    expect(ranges[1].fromLine).toBe(bigLen + 1 + COLLAPSE_CONTEXT_LINES + 1);
  });

  it("skips a hunk whose collapse range would run past the end of the document", () => {
    const hunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: bigLen }, right: { start: 0, len: bigLen } }];
    // doc only has 5 real lines even though the hunk metadata claims bigLen -- defensive
    expect(buildCollapseRanges(doc(5), hunks, "left")).toEqual([]);
  });
});

describe("updateHunks", () => {
  it("recomputes line-highlight decorations in place, without recreating the editor", () => {
    const text = "a\nb\nc\n";
    const initialHunks: Hunk[] = [
      { kind: "equal", left: { start: 0, len: 1 }, right: { start: 0, len: 1 } },
      { kind: "replace", left: { start: 1, len: 1 }, right: { start: 1, len: 1 } },
      { kind: "equal", left: { start: 2, len: 1 }, right: { start: 2, len: 1 } },
    ];
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = createDiffEditor(parent, text, initialHunks, "left");

    expect(view.dom.querySelectorAll(".diff-line-replace").length).toBe(1);

    // simulate a whitespace/case-ignore toggle that now finds no differences at all
    const allEqualHunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: 3 }, right: { start: 0, len: 3 } }];
    updateHunks(view, allEqualHunks);

    expect(view.dom.querySelectorAll(".diff-line-replace").length).toBe(0);
    view.destroy();
  });

  it("clears intra-line highlight decorations immediately when hunks change, before any new fetch resolves", async () => {
    const text = "a\nb\nc\n";
    const hunks: Hunk[] = [
      { kind: "equal", left: { start: 0, len: 1 }, right: { start: 0, len: 1 } },
      { kind: "replace", left: { start: 1, len: 1 }, right: { start: 1, len: 1 } },
      { kind: "equal", left: { start: 2, len: 1 }, right: { start: 2, len: 1 } },
    ];
    const otherDoc = Text.of(text.split("\n"));
    const fetchSpans = async (): Promise<Span[]> => [{ side: "left", startUtf16: 0, lenUtf16: 1 }];
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = createDiffEditor(parent, text, hunks, "left", false, { otherDoc, fetchSpans });

    await new Promise((r) => setTimeout(r, 10)); // let the initial intra-line fetch resolve
    expect(view.dom.querySelectorAll(".diff-intra").length).toBeGreaterThan(0);

    updateHunks(view, [{ kind: "equal", left: { start: 0, len: 3 }, right: { start: 0, len: 3 } }]);
    expect(view.dom.querySelectorAll(".diff-intra").length).toBe(0);
    view.destroy();
  });
});

describe("changeHunkLines", () => {
  it("returns the first line of each non-equal hunk, in document order", () => {
    const hunks: Hunk[] = [
      { kind: "equal", left: { start: 0, len: 2 }, right: { start: 0, len: 2 } },
      { kind: "replace", left: { start: 2, len: 1 }, right: { start: 2, len: 1 } },
      { kind: "equal", left: { start: 3, len: 5 }, right: { start: 3, len: 5 } },
      { kind: "insert", left: { start: 8, len: 0 }, right: { start: 8, len: 2 } },
    ];
    expect(changeHunkLines(hunks)).toEqual([
      { left: 3, right: 3 },
      { left: 9, right: 9 },
    ]);
  });

  it("returns an empty array when every hunk is equal", () => {
    const hunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: 5 }, right: { start: 0, len: 5 } }];
    expect(changeHunkLines(hunks)).toEqual([]);
  });
});

describe("nextHunkIndex / prevHunkIndex", () => {
  it("advances from -1 (nothing selected yet) to the first hunk", () => {
    expect(nextHunkIndex(3, -1)).toBe(0);
  });

  it("wraps forward past the last hunk back to the first", () => {
    expect(nextHunkIndex(3, 2)).toBe(0);
  });

  it("wraps backward past the first hunk to the last", () => {
    expect(prevHunkIndex(3, 0)).toBe(2);
  });

  it("moving backward from -1 lands on the last hunk, not an invalid index", () => {
    expect(prevHunkIndex(3, -1)).toBe(2);
  });

  it("returns -1 for both directions when there are no hunks to navigate", () => {
    expect(nextHunkIndex(0, -1)).toBe(-1);
    expect(prevHunkIndex(0, -1)).toBe(-1);
  });
});

describe("computeMinimapSegments", () => {
  it("positions a segment at the hunk's fractional offset and length along the left axis", () => {
    const hunks: Hunk[] = [{ kind: "replace", left: { start: 10, len: 5 }, right: { start: 10, len: 5 } }];
    const segments = computeMinimapSegments(hunks, 100);
    expect(segments).toEqual([{ kind: "replace", startFrac: 0.1, lenFrac: 0.05 }]);
  });

  it("never emits a segment for an equal hunk", () => {
    const hunks: Hunk[] = [{ kind: "equal", left: { start: 0, len: 100 }, right: { start: 0, len: 100 } }];
    expect(computeMinimapSegments(hunks, 100)).toEqual([]);
  });

  it("gives a pure insert a true zero length on the left axis, not an inflated fraction", () => {
    // Visibility for near-zero segments is the rendering layer's job (a fixed-pixel
    // min-height in CSS), not this function's -- see the doc comment on why a fractional
    // floor here would distort real hunk sizes on a large document.
    const hunks: Hunk[] = [{ kind: "insert", left: { start: 50, len: 0 }, right: { start: 50, len: 3 } }];
    const segments = computeMinimapSegments(hunks, 100);
    expect(segments).toEqual([{ kind: "insert", startFrac: 0.5, lenFrac: 0 }]);
  });

  it("does not inflate a small real hunk's length on a large document", () => {
    const hunks: Hunk[] = [{ kind: "replace", left: { start: 50000, len: 10 }, right: { start: 50000, len: 10 } }];
    const segments = computeMinimapSegments(hunks, 100_000);
    expect(segments[0].lenFrac).toBeCloseTo(0.0001, 6);
  });

  it("returns nothing for an empty document", () => {
    expect(computeMinimapSegments([{ kind: "replace", left: { start: 0, len: 1 }, right: { start: 0, len: 1 } }], 0)).toEqual([]);
  });
});

describe("computeViewportIndicator", () => {
  it("computes the visible fraction and top offset from real scroll geometry", () => {
    // 1000px of scrollable content, viewport shows 100px starting 200px down
    expect(computeViewportIndicator(200, 1000, 100)).toEqual({ topFrac: 0.2, heightFrac: 0.1 });
  });

  it("clamps height fraction to 1 when the whole document already fits in the viewport", () => {
    expect(computeViewportIndicator(0, 100, 400)).toEqual({ topFrac: 0, heightFrac: 1 });
  });

  it("is well-defined when there is nothing to scroll (content shorter than the viewport)", () => {
    expect(computeViewportIndicator(0, 0, 400)).toEqual({ topFrac: 0, heightFrac: 1 });
  });
});

describe("minimapClickToLine", () => {
  it("maps a fractional click position to a 1-indexed line number", () => {
    expect(minimapClickToLine(0.5, 100)).toBe(51);
  });

  it("clamps to the first line for a click at or above the top", () => {
    expect(minimapClickToLine(0, 100)).toBe(1);
    expect(minimapClickToLine(-0.1, 100)).toBe(1);
  });

  it("clamps to the last line for a click at or below the bottom", () => {
    expect(minimapClickToLine(1, 100)).toBe(100);
    expect(minimapClickToLine(1.1, 100)).toBe(100);
  });
});
