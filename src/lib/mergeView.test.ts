import { describe, expect, it } from "vitest";
import { Text } from "@codemirror/state";
import { EditorState } from "@codemirror/state";
import {
  buildHunkResolutionChange,
  buildMergeHunkDecorations,
  buildSourcePaneDecorations,
  hunkIndexAtPos,
  hunkRangeAtIndex,
  initialMergedHunkRanges,
  mergeHunkDecorationsField,
  replaceHunkDecoration,
  resolveHunkText,
  setMergeHunkDecorations,
} from "./mergeView";
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

describe("buildSourcePaneDecorations", () => {
  it("marks each hunk's range on the given side using that side's own LineRange", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ local: { start: 1, len: 1 }, kind: "autoMerged", resolution: "takeLocal" })];
    const decorations = buildSourcePaneDecorations(doc, hunks, "local");
    const found = hunkIndexAtPos(decorations, posAfterLine(doc, 1));
    expect(found).toBe(0);
  });

  it("uses the base LineRange for the base pane, not local/remote", () => {
    const doc = Text.of("a\nb\nc\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ base: { start: 1, len: 1 }, local: { start: 1, len: 5 }, kind: "conflict", resolution: null })];
    const decorations = buildSourcePaneDecorations(doc, hunks, "base");
    expect(hunkIndexAtPos(decorations, posAfterLine(doc, 1))).toBe(0);
  });

  it("skips a zero-length range for this side (e.g. a pure insertion has no base lines)", () => {
    const doc = Text.of("a\nb\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ base: { start: 1, len: 0 }, local: { start: 1, len: 1 }, resolution: "takeLocal" })];
    const decorations = buildSourcePaneDecorations(doc, hunks, "base");
    expect(hunkIndexAtPos(decorations, posAfterLine(doc, 1))).toBeNull();
  });
});

describe("buildHunkResolutionChange", () => {
  it("replaces the hunk's range including a trailing newline to match, when not at document end", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const ranges = [{ start: 1, len: 1 }];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const change = buildHunkResolutionChange(decorations, doc, 0, "REPLACED")!;
    expect(change).not.toBeNull();
    const newDoc = doc.replace(change.from, change.to, Text.of([change.insert]));
    expect(newDoc.toString()).toBe("a\nREPLACED\nc\n");
  });

  it("returns null for a hunk index with no current decoration", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const decorations = buildMergeHunkDecorations(doc, [{ start: 1, len: 1 }], [hunk({ resolution: "takeLocal" })]);
    expect(buildHunkResolutionChange(decorations, doc, 5, "X")).toBeNull();
  });

  it("does not append a trailing newline when the hunk reaches the end of the document", () => {
    // No content after this hunk means posAfterLine's range ends exactly at doc.length (no
    // newline to replace) -- appending "\n" here would introduce one that was never there.
    const doc = Text.of("a\nLOCAL".split("\n"));
    const ranges = [{ start: 1, len: 1 }];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const change = buildHunkResolutionChange(decorations, doc, 0, "REPLACED")!;
    expect(change.insert).toBe("REPLACED");
    const newDoc = doc.replace(change.from, change.to, Text.of([change.insert]));
    expect(newDoc.toString()).toBe("a\nREPLACED");
  });
});

describe("buildMergeHunkDecorations survives an exact-boundary replace", () => {
  it("keeps hunk 0's decoration when a resolution shrinks its content and another hunk follows", () => {
    // Pins the exact-boundary-replace case for RangeSet.map: a resolution click replaces a
    // hunk's entire current range (both `from` and `to` matching the decoration's own
    // boundaries exactly) with shorter text. Confirmed (via a temporary debug trace under Xvfb,
    // after an initial hand-miscounted manual repro wrongly suggested this dropped the mark) that
    // CM6's default mark decoration DOES survive an exact-boundary replace as long as the
    // replacement is non-empty -- this test locks that behavior in rather than leaving it as an
    // unverified assumption the rest of this module's resolution-splice logic depends on.
    const doc = Text.of("line1\nLOCAL-CHANGE\nline3\nline4\nREMOTE-CHANGE\n".split("\n"));
    const hunks: MergeHunk[] = [
      hunk({ base: { start: 1, len: 1 }, local: { start: 1, len: 1 }, remote: { start: 1, len: 1 }, resolution: "takeLocal" }),
      hunk({ base: { start: 4, len: 1 }, local: { start: 4, len: 1 }, remote: { start: 4, len: 1 }, resolution: "takeRemote" }),
    ];
    const ranges = [
      { start: 1, len: 1 },
      { start: 4, len: 1 },
    ];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const range0 = hunkRangeAtIndex(decorations, 0)!;

    const state = EditorState.create({ doc });
    const changeSet = state.changes({ from: range0.from, to: range0.to, insert: "line2\n" });
    const mapped = decorations.map(changeSet);

    expect(hunkRangeAtIndex(mapped, 0)).not.toBeNull();
    expect(hunkRangeAtIndex(mapped, 1)).not.toBeNull();
  });
});

describe("replaceHunkDecoration", () => {
  it("restyles one hunk's decoration in place, at its current (possibly-shifted) range, leaving other hunks untouched", () => {
    const doc = Text.of("a\nCONFLICT\nc\nREMOTE\ne\n".split("\n"));
    const ranges = [
      { start: 1, len: 1 },
      { start: 3, len: 1 },
    ];
    const hunks: MergeHunk[] = [hunk({ kind: "conflict", resolution: null }), hunk({ kind: "autoMerged", resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const resolvedHunk: MergeHunk = { ...hunks[0], resolution: "takeLocal" };
    const updated = replaceHunkDecoration(decorations, 0, resolvedHunk);

    // Hunk 0's class reflects the new resolution.
    let hunk0Class: string | undefined;
    updated.between(0, doc.length, (from, to, deco) => {
      if ((deco.spec.attributes as Record<string, string>)?.["data-merge-hunk-index"] === "0") hunk0Class = deco.spec.class;
    });
    expect(hunk0Class).toContain("merge-hunk-resolved-takeLocal");

    // Hunk 1 is untouched -- still findable at its original position with its original class.
    expect(hunkIndexAtPos(updated, posAfterLine(doc, 3))).toBe(1);
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

  it("hunkRangeAtIndex finds a hunk's current character range by index", () => {
    const doc = Text.of("a\nLOCAL\nc\nREMOTE\ne\n".split("\n"));
    const ranges = [
      { start: 1, len: 1 },
      { start: 3, len: 1 },
    ];
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" }), hunk({ resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const range = hunkRangeAtIndex(decorations, 1);
    expect(range).not.toBeNull();
    // Trailing "\n" included, same posAfterLine convention buildHunkCopyChange's own tests use
    // (a replace range spans through to the next line's start).
    expect(doc.sliceString(range!.from, range!.to)).toBe("REMOTE\n");
  });

  it("hunkRangeAtIndex returns null for a hunk index with no decoration (e.g. a skipped zero-length hunk)", () => {
    const doc = Text.of("a\nLOCAL\nc\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ resolution: "takeLocal" }), hunk({ base: { start: 0, len: 0 }, local: { start: 0, len: 0 }, resolution: "takeRemote" })];
    const decorations = buildMergeHunkDecorations(doc, [{ start: 1, len: 1 }, { start: 1, len: 0 }], hunks);
    expect(hunkRangeAtIndex(decorations, 1)).toBeNull();
  });

  it("does not skip a zero-length range for an unresolved conflict hunk -- it still needs a trackable position for a later resolution click (e.g. an add/add conflict with an empty BASE)", () => {
    const doc = Text.of("a\nb\n".split("\n"));
    const hunks: MergeHunk[] = [hunk({ base: { start: 1, len: 0 }, local: { start: 1, len: 1 }, remote: { start: 1, len: 1 }, resolution: null, kind: "conflict" })];
    const decorations = buildMergeHunkDecorations(doc, [{ start: 1, len: 0 }], hunks);
    expect(hunkRangeAtIndex(decorations, 0)).not.toBeNull();
  });

  it("buildHunkResolutionChange inserts real text at a zero-length unresolved-conflict hunk's position, instead of silently no-op'ing (regression: Take Local/Remote/Both/Base on an add/add conflict wrote nothing)", () => {
    const doc = Text.of("a\nb\n".split("\n"));
    const ranges = [{ start: 1, len: 0 }];
    const hunks: MergeHunk[] = [hunk({ resolution: null, kind: "conflict" })];
    const decorations = buildMergeHunkDecorations(doc, ranges, hunks);
    const change = buildHunkResolutionChange(decorations, doc, 0, "RESOLVED")!;
    expect(change).not.toBeNull();
    const newDoc = doc.replace(change.from, change.to, Text.of([change.insert]));
    expect(newDoc.toString()).toBe("a\nRESOLVED\nb\n");
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
