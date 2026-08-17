import { describe, it, expect } from "vitest";
import { Text } from "@codemirror/state";
import { LINE_HEIGHT_PX, posAfterLine, buildDecorations } from "./diffView";
import type { Hunk } from "./types";

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
