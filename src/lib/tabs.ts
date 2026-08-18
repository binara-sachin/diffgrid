/** Opaque per-tab id, generated once when a tab opens and used as the join key across the IPC
 * boundary (`open_file_pair`/`apply_edit`/`redo_diff`/`save_file`/`close_tab` all take it) and
 * for the frontend's own `tabRuntimes` map of live `EditorView`s. */
export function createTabId(): string {
  return crypto.randomUUID();
}

function basename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

/**
 * The short name shown in a tab's button, e.g. "compare.ts" rather than the full path -- just
 * the left path's basename (the common case, since both sides of a pair almost always share a
 * name), falling back to the right path's if the left one is empty.
 */
export function tabLabel(leftPath: string, rightPath: string): string {
  return basename(leftPath) || basename(rightPath);
}
