export type HunkKind = "equal" | "insert" | "delete" | "replace";

export interface LineRange {
  start: number;
  len: number;
}

export interface Hunk {
  kind: HunkKind;
  left: LineRange;
  right: LineRange;
}

export interface DiffStats {
  added: number;
  removed: number;
  chunks: number;
}

export interface FileDiffResult {
  hunks: Hunk[];
  stats: DiffStats;
}
