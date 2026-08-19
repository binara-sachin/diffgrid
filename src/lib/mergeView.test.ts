import { describe, expect, it } from "vitest";
import { Text } from "@codemirror/state";
import { EditorState } from "@codemirror/state";
import { buildMergeHunkDecorations, hunkIndexAtPos, initialMergedHunkRanges, mergeHunkDecorationsField, resolveHunkText, setMergeHunkDecorations } from "./mergeView";
import { posAfterLine } from "./diffView";
import type { MergeHunk } from "./types";

function hunk(overrides: Partial<MergeHunk>): MergeHunk {
  return { kind: "autoMerged", base: { start: 0, len: 0 }, local: { start: 0, len: 0 }, remote: { start: 0, len: 0 }, resolution: "takeLocal", ...overrides };
}

describe("resolveHunkText", () => {
  it("extracts the local range for takeLocal", () => {
    const h = hunk({ local: { start: 1, len: 1 }, resolution: "takeLocal" });
    expect(resolveHunkText("a\nb\nc", "a\nLOCAL\nc", "a\nb\nc", h, "mineFirst")).toBe("LOCAL");
  });

  it("extracts the remote range for takeRemote", () => {
    const h = hunk({ remote: { start: 1, len: 1 }, resolution: "takeRemote" });
    expect(resolveHunkText("a\nb\nc", "a\nb\nc", "a\nREMOTE\nc", h, "mineFirst")).toBe("REMOTE");
  });

  it("concatenates local then remote for takeBoth with mineFirst", () => {
    const h = hunk({ local: { start: 1, len: 1 }, remote: { start: 1, len: 1 }, resolution: "takeBoth" });
    expect(resolveHunkText("a\nb\nc", "a\nLOCAL\nc", "a\nREMOTE\nc", h, "mineFirst")).toBe("LOCAL\nREMOTE");
  });

  it("concatenates remote then local for takeBoth with theirsFirst", () => {
    const h = hunk({ local: { start: 1, len: 1 }, remote: { start: 1, len: 1 }, resolution: "takeBoth" });
    expect(resolveHunkText("a\nb\nc", "a\nLOCAL\nc", "a\nREMOTE\nc", h, "theirsFirst")).toBe("REMOTE\nLOCAL");
  });

  it("extracts the base range for takeBase", () => {
    const h = hunk({ base: { start: 1, len: 1 }, resolution: "takeBase" });
    expect(resolveHunkText("a\nb\nc", "a\nLOCAL\nc", "a\nREMOTE\nc", h, "mineFirst")).toBe("b");
  });

  it("throws on manual", () => {
    const h = hunk({ resolution: "manual" });
    expect(() => resolveHunkText("a", "a", "a", h, "mineFirst")).toThrow();
  });

  it("throws on an unresolved conflict", () => {
    const h = hunk({ resolution: null });
    expect(() => resolveHunkText("a", "a", "a", h, "mineFirst")).toThrow();
  });
});

describe("initialMergedHunkRanges", () => {
  it("accounts for unchanged base lines between hunks, not just hunk content", () => {
    // Two disjoint single-line changes with an unchanged base line between them (base line 1,
    // "b") -- the regression this test guards: a naive walk that only sums each hunk's own
    // resolved length (skipping the gap) would place the second hunk one line too early.
    const base = "a\nb\nc\nd\n";
    const local = "a\nb\nc\nd\n";
    const remote = "a\nb\nc\nd\n";
    const hunks: MergeHunk[] = [
      hunk({ base: { start: 0, len: 1 }, local: { start: 0, len: 1 }, remote: { start: 0, len: 1 }, resolution: "takeLocal" }),
      hunk({ base: { start: 2, len: 1 }, local: { start: 2, len: 1 }, remote: { start: 2, len: 1 }, resolution: "takeRemote" }),
    ];
    const ranges = initialMergedHunkRanges(base, local, remote, hunks, "mineFirst");
    expect(ranges[0]).toEqual({ start: 0, len: 1 });
    // The gap (base line 1, "b") sits between the two hunks in the merged text, so hunk 1 starts
    // at line 2, not line 1.
    expect(ranges[1]).toEqual({ start: 2, len: 1 });
  });

  it("places a hunk's range correctly when its resolved content is longer than its base range", () => {
    const base = "a\nb\nc\n";
    const local = "a\nLOCAL1\nLOCAL2\nc\n";
    const remote = "a\nb\nc\n";
    const hunks: MergeHunk[] = [hunk({ base: { start: 1, len: 1 }, local: { start: 1, len: 2 }, remote: { start: 1, len: 1 }, resolution: "takeLocal" })];
    const ranges = initialMergedHunkRanges(base, local, remote, hunks, "mineFirst");
    expect(ranges[0]).toEqual({ start: 1, len: 2 });
  });

  it("computed ranges correctly slice into the real merged text produced by the same inputs", () => {
    // Integration check against a mergedText string constructed the same way merge-core's
    // build_merged_text would (rather than trusting internally-consistent numbers alone) --
    // guards against initialMergedHunkRanges silently drifting from what Rust actually sends.
    const base = "a\nb\nc\nd\ne\n";
    const local = "a\nLOCAL1\nLOCAL2\nc\nd\ne\n";
    const remote = "a\nb\nc\nd\nREMOTE\n";
    const hunks: MergeHunk[] = [
      hunk({ base: { start: 1, len: 1 }, local: { start: 1, len: 2 }, remote: { start: 1, len: 1 }, resolution: "takeLocal" }),
      hunk({ base: { start: 4, len: 1 }, local: { start: 5, len: 1 }, remote: { start: 4, len: 1 }, resolution: "takeRemote" }),
    ];
    const mergedText = "a\nLOCAL1\nLOCAL2\nc\nd\nREMOTE\n";
    const ranges = initialMergedHunkRanges(base, local, remote, hunks, "mineFirst");
    const mergedLines = mergedText.split("\n");
    expect(mergedLines.slice(ranges[0].start, ranges[0].start + ranges[0].len).join("\n")).toBe("LOCAL1\nLOCAL2");
    expect(mergedLines.slice(ranges[1].start, ranges[1].start + ranges[1].len).join("\n")).toBe("REMOTE");
  });
});

describe("buildMergeHunkDecorations + hunkIndexAtPos", () => {
  it("marks each hunk's character range and hunkIndexAtPos finds it back by index", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const ranges = [{ start: 1, len: 1 }];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal", kind: "autoMerged" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const found = hunkIndexAtPos(decorations, posAfterLine(doc, 1));
    expect(found).toBe(0);
  });

  it("returns null when the position is outside every hunk's range", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const ranges = [{ start: 1, len: 1 }];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    expect(hunkIndexAtPos(decorations, posAfterLine(doc, 0))).toBeNull();
  });

  it("distinguishes multiple hunks by index", () => {
    const doc = Text.of("a\nLOCAL\nc\nREMOTE\ne\n".split("\n"));
    const ranges = [
      { start: 1, len: 1 },
      { start: 3, len: 1 },
    ];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" }), hunk({ resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    expect(hunkIndexAtPos(decorations, posAfterLine(doc, 1))).toBe(0);
    expect(hunkIndexAtPos(decorations, posAfterLine(doc, 3))).toBe(1);
  });

  it("keeps a later hunk's position correct after an earlier hunk's content is edited (the core RangeSet.map guarantee)", () => {
    // This is the property the whole no-Rust-side-position-tracking design (see DECISIONS.md)
    // depends on: after hunk 0's one-line content grows to three lines via a real CM6
    // transaction, hunk 1's decoration must have shifted down by two lines automatically,
    // with no manual re-derivation of its range.
    const doc = Text.of("a\nLOCAL\nc\nREMOTE\ne\n".split("\n"));
    const ranges = [
      { start: 1, len: 1 },
      { start: 3, len: 1 },
    ];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" }), hunk({ resolution: "takeRemote" })];
    const initial = buildMergeHunkDecorations(doc, ranges, hunks);

    let state = EditorState.create({ doc, extensions: [mergeHunkDecorationsField.init(() => initial)] });
    // Replace "LOCAL" (hunk 0's content) with a three-line block.
    const hunk0From = posAfterLine(doc, 1);
    const hunk0To = posAfterLine(doc, 2);
    const tr = state.update({ changes: { from: hunk0From, to: hunk0To, insert: "GROWN1\nGROWN2\nGROWN3" } });
    state = tr.state;

    const decorationsAfter = state.field(mergeHunkDecorationsField);
    const newDoc = state.doc;
    const hunk1PosAfter = posAfterLine(newDoc, 5); // "REMOTE" is now on line 6 (0-indexed 5), was line 4 (0-indexed 3)
    expect(hunkIndexAtPos(decorationsAfter, hunk1PosAfter)).toBe(1);
  });

  it("setMergeHunkDecorations fully replaces the field's value", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" })];
    const initial = buildMergeHunkDecorations(doc, [{ start: 1, len: 1 }], hunks);
    const state = EditorState.create({ doc, extensions: [mergeHunkDecorationsField.init(() => initial)] });
    const replacement = buildMergeHunkDecorations(doc, [{ start: 1, len: 1 }], [hunk({ resolution: "takeRemote" })]);
    const tr = state.update({ effects: setMergeHunkDecorations.of(replacement) });
    const found = hunkIndexAtPos(tr.state.field(mergeHunkDecorationsField), posAfterLine(doc, 1));
    expect(found).toBe(0);
  });
});
