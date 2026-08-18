import { describe, expect, it } from "vitest";
import { visibleDirEntries } from "./dirView";
import type { DirEntry } from "./types";

function entry(path: string, status: DirEntry["status"] = "same"): DirEntry {
  return { path, status, isDir: false, isSymlink: false, symlinkTarget: null, sizeLeft: 1, sizeRight: 1 };
}

describe("visibleDirEntries", () => {
  it("sorts by path regardless of arrival order", () => {
    const entries = [entry("z.txt"), entry("a.txt"), entry("m/b.txt")];
    const result = visibleDirEntries(entries, false);
    expect(result.map((e) => e.path)).toEqual(["a.txt", "m/b.txt", "z.txt"]);
  });

  it("filters out same-status entries when hideIdentical is true", () => {
    const entries = [entry("same.txt", "same"), entry("changed.txt", "modified")];
    const result = visibleDirEntries(entries, true);
    expect(result.map((e) => e.path)).toEqual(["changed.txt"]);
  });

  it("keeps same-status entries when hideIdentical is false", () => {
    const entries = [entry("b.txt", "same"), entry("a.txt", "modified")];
    const result = visibleDirEntries(entries, false);
    expect(result.map((e) => e.path)).toEqual(["a.txt", "b.txt"]);
  });

  it("does not mutate the input array", () => {
    const entries = [entry("z.txt"), entry("a.txt")];
    visibleDirEntries(entries, false);
    expect(entries.map((e) => e.path)).toEqual(["z.txt", "a.txt"]);
  });
});
