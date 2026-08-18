import { describe, expect, it } from "vitest";
import { buildDirTree, countDirTreeFiles, flattenDirTree, pruneDirTree, visibleDirTreeRows } from "./dirView";
import type { DirEntry } from "./types";

function entry(path: string, status: DirEntry["status"] = "same", isDir = false): DirEntry {
  return { path, status, isDir, isSymlink: false, symlinkTarget: null, sizeLeft: 1, sizeRight: 1 };
}

describe("buildDirTree", () => {
  it("groups flat entries into a nested tree by path segment", () => {
    const entries = [entry("src", "same", true), entry("src/a.ts", "modified"), entry("src/lib", "same", true), entry("src/lib/b.ts", "same")];
    const root = buildDirTree(entries);
    expect(root.children.map((c) => c.name)).toEqual(["src"]);
    const src = root.children[0];
    expect(src.children.map((c) => c.name)).toEqual(["a.ts", "lib"]);
    const lib = src.children.find((c) => c.name === "lib")!;
    expect(lib.children.map((c) => c.name)).toEqual(["b.ts"]);
  });

  it("synthesizes a folder node for a path with no directory entry of its own", () => {
    // Mirrors the existing flat-list test fixture's "m/b.txt" with no explicit "m" entry.
    const entries = [entry("m/b.txt", "modified")];
    const root = buildDirTree(entries);
    const m = root.children.find((c) => c.name === "m")!;
    expect(m).toBeDefined();
    expect(m.isDir).toBe(true);
    expect(m.entry).toBeNull();
    expect(m.children.map((c) => c.name)).toEqual(["b.txt"]);
  });

  it("sorts children alphabetically by name within each level", () => {
    const entries = [entry("z.txt"), entry("a.txt"), entry("m.txt")];
    const root = buildDirTree(entries);
    expect(root.children.map((c) => c.name)).toEqual(["a.txt", "m.txt", "z.txt"]);
  });
});

describe("pruneDirTree", () => {
  it("drops unmodified leaves when hideIdentical is true", () => {
    const entries = [entry("same.txt", "same"), entry("changed.txt", "modified")];
    const pruned = pruneDirTree(buildDirTree(entries), true);
    expect(pruned.children.map((c) => c.name)).toEqual(["changed.txt"]);
  });

  it("keeps an unmodified ancestor folder when a descendant passes the filter", () => {
    const entries = [entry("src", "same", true), entry("src/unchanged.ts", "same"), entry("src/changed.ts", "modified")];
    const pruned = pruneDirTree(buildDirTree(entries), true);
    const src = pruned.children.find((c) => c.name === "src")!;
    expect(src).toBeDefined();
    expect(src.children.map((c) => c.name)).toEqual(["changed.ts"]);
  });

  it("drops a folder entirely when none of its descendants pass the filter", () => {
    const entries = [entry("src", "same", true), entry("src/unchanged.ts", "same"), entry("other.ts", "modified")];
    const pruned = pruneDirTree(buildDirTree(entries), true);
    expect(pruned.children.map((c) => c.name)).toEqual(["other.ts"]);
  });

  it("keeps everything when hideIdentical is false", () => {
    const entries = [entry("same.txt", "same"), entry("changed.txt", "modified")];
    const pruned = pruneDirTree(buildDirTree(entries), false);
    expect(pruned.children.map((c) => c.name)).toEqual(["changed.txt", "same.txt"]);
  });
});

describe("flattenDirTree", () => {
  it("includes top-level entries by default with no explicitly collapsed paths", () => {
    const entries = [entry("a.txt"), entry("dir", "same", true), entry("dir/b.txt")];
    const rows = flattenDirTree(buildDirTree(entries), new Set());
    expect(rows.map((r) => r.path)).toEqual(["a.txt", "dir", "dir/b.txt"]);
  });

  it("excludes a directory's children when its path is in collapsedPaths", () => {
    const entries = [entry("dir", "same", true), entry("dir/b.txt")];
    const rows = flattenDirTree(buildDirTree(entries), new Set(["dir"]));
    expect(rows.map((r) => r.path)).toEqual(["dir"]);
  });

  it("reports depth, isDir, and hasChildren correctly for each row", () => {
    const entries = [entry("dir", "same", true), entry("dir/b.txt"), entry("leaf.txt")];
    const rows = flattenDirTree(buildDirTree(entries), new Set());
    const dirRow = rows.find((r) => r.path === "dir")!;
    const nestedRow = rows.find((r) => r.path === "dir/b.txt")!;
    const leafRow = rows.find((r) => r.path === "leaf.txt")!;
    expect(dirRow.depth).toBe(0);
    expect(dirRow.isDir).toBe(true);
    expect(dirRow.hasChildren).toBe(true);
    expect(nestedRow.depth).toBe(1);
    expect(leafRow.hasChildren).toBe(false);
  });
});

describe("visibleDirTreeRows", () => {
  it("combines build, prune, and flatten into one call", () => {
    const entries = [entry("src", "same", true), entry("src/unchanged.ts", "same"), entry("src/changed.ts", "modified")];
    const rows = visibleDirTreeRows(entries, true, new Set());
    expect(rows.map((r) => r.path)).toEqual(["src", "src/changed.ts"]);
  });
});

describe("countDirTreeFiles", () => {
  it("counts only files, not folders, at any depth", () => {
    const entries = [entry("a.txt", "modified"), entry("dir", "same", true), entry("dir/b.txt", "modified"), entry("dir/sub", "same", true), entry("dir/sub/c.txt", "modified")];
    expect(countDirTreeFiles(buildDirTree(entries))).toBe(3);
  });

  it("stays accurate regardless of collapse state -- unaffected by flattenDirTree's collapsedPaths", () => {
    // The regression this guards: the sidebar's "CHANGED FILES · N" header must count every
    // changed file even when its parent folder is currently collapsed, not just the rows
    // flattenDirTree happens to currently emit.
    const entries = [entry("dir", "same", true), entry("dir/a.txt", "modified"), entry("dir/b.txt", "modified")];
    const tree = pruneDirTree(buildDirTree(entries), true);
    const collapsedCount = flattenDirTree(tree, new Set(["dir"])).filter((r) => !r.isDir).length;
    expect(collapsedCount).toBe(0); // rows are hidden while collapsed...
    expect(countDirTreeFiles(tree)).toBe(2); // ...but the true count doesn't change.
  });
});
