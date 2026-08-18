import type { DirEntry } from "./types";

export function visibleDirEntries(entries: DirEntry[], hideIdentical: boolean): DirEntry[] {
  const filtered = hideIdentical ? entries.filter((e) => e.status !== "same") : entries;
  return filtered.slice().sort((a, b) => a.path.localeCompare(b.path));
}
