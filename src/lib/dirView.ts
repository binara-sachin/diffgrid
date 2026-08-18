import type { DirEntry } from "./types";

export interface DirTreeNode {
  /** Last path segment -- "" only for the synthetic root. */
  name: string;
  /** Full relative path, "/" joined -- "" only for the synthetic root. */
  path: string;
  isDir: boolean;
  /** The scanned `DirEntry` this node came from, or null for a folder implied by a nested path
   * (e.g. "src/lib/b.ts" with no separate "src/lib" entry ever scanned) or the synthetic root. */
  entry: DirEntry | null;
  children: DirTreeNode[];
}

function emptyDirNode(name: string, path: string): DirTreeNode {
  return { name, path, isDir: true, entry: null, children: [] };
}

/**
 * Groups `scan_dirs`' flat `DirEntry` list into a real nested tree by splitting each path on
 * "/", per M3's own DECISIONS.md deferral note: this is a pure frontend transform over data the
 * scan already produces correctly, not a scan-logic change. A folder is synthesized (isDir=true,
 * entry=null) wherever a path implies an intermediate directory that was never itself scanned as
 * a `DirEntry` -- e.g. a `.gitignore`-excluded folder whose *contents* are still shown because
 * only some of them are excluded. Every level is sorted alphabetically by name, matching the
 * flat list's existing path-sort convention.
 */
export function buildDirTree(entries: DirEntry[]): DirTreeNode {
  const root = emptyDirNode("", "");
  for (const entry of entries) {
    const segments = entry.path.split("/");
    let node = root;
    for (let i = 0; i < segments.length; i++) {
      const name = segments[i];
      const path = segments.slice(0, i + 1).join("/");
      const isLast = i === segments.length - 1;
      let child = node.children.find((c) => c.name === name);
      if (!child) {
        child = emptyDirNode(name, path);
        node.children.push(child);
      }
      if (isLast) {
        child.isDir = entry.isDir;
        child.entry = entry;
      }
      node = child;
    }
  }
  sortDirTree(root);
  return root;
}

function sortDirTree(node: DirTreeNode): void {
  node.children.sort((a, b) => a.name.localeCompare(b.name));
  for (const child of node.children) sortDirTree(child);
}

/**
 * Filters a tree built by `buildDirTree` for `hideIdentical`, keeping a folder whenever *any*
 * descendant survives the filter even if the folder itself is `same` -- otherwise an unmodified
 * folder containing a modified file would vanish along with its only visible child. Leaves
 * (files) are dropped outright when `same` and `hideIdentical` is true.
 */
export function pruneDirTree(node: DirTreeNode, hideIdentical: boolean): DirTreeNode {
  if (!hideIdentical) return node;
  const children = node.children
    .map((c) => pruneDirTree(c, hideIdentical))
    .filter((c) => c.children.length > 0 || (c.entry !== null && c.entry.status !== "same"));
  return { ...node, children };
}

export interface DirTreeRow {
  path: string;
  name: string;
  isDir: boolean;
  entry: DirEntry | null;
  depth: number;
  hasChildren: boolean;
}

/**
 * Depth-first flattening of a tree into the ordered row list the sidebar renders, skipping a
 * folder's children when its path is in `collapsedPaths` -- the per-folder expand/collapse state
 * PLAN.md's "sidebar tree" calls for. `collapsedPaths` holds folder paths that are collapsed
 * (children hidden); a folder not in the set renders expanded, matching a fresh scan's
 * all-expanded default.
 */
export function flattenDirTree(node: DirTreeNode, collapsedPaths: ReadonlySet<string>, depth = 0): DirTreeRow[] {
  const rows: DirTreeRow[] = [];
  for (const child of node.children) {
    rows.push({ path: child.path, name: child.name, isDir: child.isDir, entry: child.entry, depth, hasChildren: child.children.length > 0 });
    if (child.isDir && child.children.length > 0 && !collapsedPaths.has(child.path)) {
      rows.push(...flattenDirTree(child, collapsedPaths, depth + 1));
    }
  }
  return rows;
}

/** `buildDirTree` + `pruneDirTree` + `flattenDirTree` in one call -- what the sidebar actually
 * needs each time `dirEntries`/`hideIdentical`/`collapsedPaths` change. */
export function visibleDirTreeRows(entries: DirEntry[], hideIdentical: boolean, collapsedPaths: ReadonlySet<string>): DirTreeRow[] {
  return flattenDirTree(pruneDirTree(buildDirTree(entries), hideIdentical), collapsedPaths);
}

/** Total file (non-folder) count across the whole tree, regardless of collapse state -- the
 * "CHANGED FILES · N" header count must reflect every changed file whether or not its parent
 * folder happens to be collapsed right now, so it's computed from the pruned tree directly
 * rather than from `flattenDirTree`'s (collapse-sensitive) row list. */
export function countDirTreeFiles(node: DirTreeNode): number {
  let count = 0;
  for (const child of node.children) {
    if (child.isDir) count += countDirTreeFiles(child);
    else count += 1;
  }
  return count;
}
