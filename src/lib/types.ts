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

export type Encoding = "utf8" | "utf16Le" | "utf16Be" | "latin1";
export type LineEnding = "lf" | "crlf" | "mixed";

export interface FileMeta {
  encoding: Encoding;
  lineEnding: LineEnding;
  trailingNewline: boolean;
  isBinary: boolean;
  lineCount: number;
}

export interface OpenPairResult {
  diff: FileDiffResult;
  leftMeta: FileMeta;
  rightMeta: FileMeta;
}

export type SpanSide = "left" | "right";

export interface Span {
  side: SpanSide;
  startUtf16: number;
  lenUtf16: number;
}

export type EntryStatus = "same" | "modified" | "leftOnly" | "rightOnly" | "typeConflict";

export interface DirEntry {
  path: string;
  status: EntryStatus;
  isDir: boolean;
  isSymlink: boolean;
  symlinkTarget: string | null;
  sizeLeft: number | null;
  sizeRight: number | null;
}

export interface ScanOutcome {
  cancelled: boolean;
  leftVisited: number;
  rightVisited: number;
  entriesEmitted: number;
}
