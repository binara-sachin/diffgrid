import { describe, it, expect } from "vitest";
import { Text } from "@codemirror/state";
import { PadWidget, LINE_HEIGHT_PX, posAfterLine, buildDecorations } from "./diffView";
import type { Hunk } from "./types";

describe("PadWidget", () => {
  it("renders a DOM node whose height is the padded line count times LINE_HEIGHT_PX", () => {
    const widget = new PadWidget(5, "insert");
    const dom = widget.toDOM();
    expect(dom.style.height).toBe(`${5 * LINE_HEIGHT_PX}px`);
  });

  it("scales rendered height with the padded line count", () => {
    expect(new PadWidget(1, "delete").toDOM().style.height).toBe(`${LINE_HEIGHT_PX}px`);
    expect(new PadWidget(37, "replace").toDOM().style.height).toBe(`${37 * LINE_HEIGHT_PX}px`);
  });
});

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
    const widgets: { pos: number; lines: number }[] = [];
    set.between(0, doc.length, (from, _to, deco) => {
      if (deco.spec.widget) widgets.push({ pos: from, lines: (deco.spec.widget as PadWidget).lines });
      else lines.push(from);
    });
    return { lines, widgets };
  }

  it("highlights every real line in a non-equal hunk on the shorter (left) side", () => {
    const { lines } = collect(leftDoc, "left");
    expect(lines).toEqual([leftDoc.line(4).from, leftDoc.line(5).from]); // L0, L1
  });

  it("highlights every real line in a non-equal hunk on the longer (right) side", () => {
    const { lines } = collect(rightDoc, "right");
    expect(lines).toEqual([rightDoc.line(4).from, rightDoc.line(5).from, rightDoc.line(6).from, rightDoc.line(7).from]);
  });

  it("pads the shorter side to align with the longer side's extra lines", () => {
    const { widgets } = collect(leftDoc, "left");
    expect(widgets).toEqual([{ pos: leftDoc.line(6).from, lines: 2 }]); // right has 2 more lines than left
  });

  it("does not pad the longer side", () => {
    const { widgets } = collect(rightDoc, "right");
    expect(widgets).toEqual([]);
  });

  it("omits padding entirely when disablePadding is set", () => {
    const { widgets } = collect(leftDoc, "left", true);
    expect(widgets).toEqual([]);
  });

  it("never decorates an equal hunk", () => {
    const { lines: leftLines } = collect(leftDoc, "left");
    const { lines: rightLines } = collect(rightDoc, "right");
    // only the 2 left / 4 right replace-hunk lines should be decorated, never the equal runs
    expect(leftLines.length).toBe(2);
    expect(rightLines.length).toBe(4);
  });
});
