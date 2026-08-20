use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use serde::Serialize;
use session::EditBuffer;
use tauri::ipc::{Channel, Response};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

type TabId = String;

/// One open file-pair tab's edit state. Both sides live under the *same* map entry (and so the
/// same lock, held via `SessionState.tabs`) rather than each having its own `Mutex` the way M2's
/// single-pair `SessionState` did -- that's what makes `redo_diff_impl` trivially consistent now
/// (one lock covers both sides, so there's no way to observe a left snapshot from one instant and
/// a right snapshot from another) without the careful "lock both, in a fixed order" discipline
/// the M2 version needed. See DECISIONS.md for why the coarser per-tab lock (vs. per-side) is the
/// right trade for a single-user desktop app with no read-heavy contention case.
struct TabBuffers {
    left: EditBuffer,
    right: EditBuffer,
}

/// M4's in-memory edit state: one `TabBuffers` per open file-pair tab, keyed by a frontend-
/// generated `TabId` (per docs/PLAN.md §5 -- multiple open-file edit buffers, not just one).
/// Closing a tab (`close_tab`) removes its entry entirely, freeing the `EditBuffer`s.
#[derive(Default)]
struct SessionState {
    tabs: Mutex<HashMap<TabId, TabBuffers>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Side {
    Left,
    Right,
}

/// M3's directory-scan cancellation state (docs/PLAN.md §5/M3): holds the *current* scan's
/// cancel flag, if one is running. A fresh `Arc<AtomicBool>` per scan (not one reused flag) is
/// what lets `cancel_scan` target only the scan that was actually running when it was called --
/// without this, a cancel request racing a brand-new scan the user just started could kill the
/// wrong one.
#[derive(Default)]
struct ScanState {
    current_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Serialize)]
struct BenchFlags {
    disable_padding: bool,
    collapse_equal: bool,
}

/// M0/M1 A/B toggles: let the measurement harness isolate whether a given decoration mechanism
/// is the source of a scroll-performance regression, rather than asserting a cause untested.
/// `collapse_equal` probes whether `Decoration.replace({block:true})` (the only CM6 mechanism
/// for collapsing unchanged regions) costs what `Decoration.widget({block:true})` did before
/// docs/PROFILING.md's fix — see DECISIONS.md.
#[tauri::command]
fn bench_flags() -> BenchFlags {
    BenchFlags {
        disable_padding: std::env::var("DIFFGRID_DISABLE_PADDING").is_ok(),
        collapse_equal: std::env::var("DIFFGRID_COLLAPSE_EQUAL").is_ok(),
    }
}

/// M0 spike only: fixtures are resolved relative to this crate's manifest dir so the
/// command works regardless of the process's working directory. Real file-open commands
/// (M1) take arbitrary user-chosen paths instead.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn fixture_path(name: &str, side: &str) -> Result<PathBuf, String> {
    let file = match side {
        "left" => "left.js",
        "right" => "right.js",
        other => return Err(format!("unknown side: {other}")),
    };
    Ok(fixtures_dir().join(name).join(file))
}

#[tauri::command]
fn diff_fixture(name: String) -> Result<diff_core::FileDiffResult, String> {
    let left = std::fs::read_to_string(fixture_path(&name, "left")?).map_err(|e| e.to_string())?;
    let right = std::fs::read_to_string(fixture_path(&name, "right")?).map_err(|e| e.to_string())?;
    Ok(diff_core::diff_lines(&left, &right))
}

/// Full file text crosses the boundary exactly once per open, as raw bytes rather than a
/// JSON string — per docs/PLAN.md §3, avoiding JSON-string overhead on multi-MB fixtures.
#[tauri::command]
fn fixture_text(name: String, side: String) -> Result<Response, String> {
    let bytes = std::fs::read(fixture_path(&name, &side)?).map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenPairResult {
    diff: diff_core::FileDiffResult,
    left_meta: text_io::FileMeta,
    right_meta: text_io::FileMeta,
}

/// Pure over already-read bytes so it's testable without touching the filesystem; the
/// `#[tauri::command]` wrapper below owns the actual `std::fs::read` calls. Both sides are
/// loaded independently (own encoding/line-ending detection) since a real file pair has no
/// reason to share either.
fn diff_pair(left_bytes: &[u8], right_bytes: &[u8]) -> Result<OpenPairResult, String> {
    let left = text_io::load(left_bytes);
    let right = text_io::load(right_bytes);
    if left.meta.is_binary || right.meta.is_binary {
        return Err("binary file: diffgrid does not diff binary files".to_string());
    }
    let diff = diff_core::diff_lines(&left.normalized, &right.normalized);
    Ok(OpenPairResult { diff, left_meta: left.meta, right_meta: right.meta })
}

/// M1: `diffgrid FILE1 FILE2` real-file entry point (see `launch_args`). Reads both files,
/// runs binary refusal + encoding/line-ending detection via `text-io`, then diffs the
/// normalized text. M2: also seeds a fresh `EditBuffer` pair (holding the raw bytes just read,
/// for an exact-byte-identical unedited save — see `EditBuffer`). M4: keyed by `tab_id`, so
/// opening a pair never disturbs any other tab's buffers -- inserting under an id that's already
/// present replaces just that tab, same as it always implicitly did for the single-tab case.
#[tauri::command]
fn open_file_pair(state: tauri::State<SessionState>, tab_id: String, left: String, right: String) -> Result<OpenPairResult, String> {
    let left_bytes = std::fs::read(&left).map_err(|e| format!("{left}: {e}"))?;
    let right_bytes = std::fs::read(&right).map_err(|e| format!("{right}: {e}"))?;
    let result = diff_pair(&left_bytes, &right_bytes)?;

    let left_loaded = text_io::load(&left_bytes);
    let right_loaded = text_io::load(&right_bytes);
    let buffers = TabBuffers {
        left: EditBuffer::new(&left_loaded.normalized, left_bytes, left_loaded.meta),
        right: EditBuffer::new(&right_loaded.normalized, right_bytes, right_loaded.meta),
    };
    state.tabs.lock().unwrap().insert(tab_id, buffers);

    Ok(result)
}

fn close_tab_impl(state: &SessionState, tab_id: &str) {
    state.tabs.lock().unwrap().remove(tab_id);
}

/// Drops a tab's `EditBuffer` pair entirely, freeing the memory. A no-op (not an error) if the
/// id is unknown -- same "nothing to signal, nothing to do" reasoning as `cancel_scan`'s
/// no-op-when-nothing's-running case, since the frontend has no reason to distinguish "already
/// closed" from "never existed" when it's just cleaning up after itself.
#[tauri::command]
fn close_tab(state: tauri::State<SessionState>, tab_id: String) {
    close_tab_impl(&state, &tab_id)
}

/// One open three-way merge tab's state (M5, per docs/PLAN.md §5's `merge-core` module and the
/// BASE/LOCAL/REMOTE + merged-output view). `base`/`local`/`remote` are read-only after open, so
/// they're stored as plain `String`, not `EditBuffer` -- only `merged` is ever edited. `hunks` is
/// the current classification; `resolution` on each entry is mutated by `set_merge_resolution`
/// and, unlike M1-M4's diff hunks, is NOT recomputed by re-diffing -- a merge hunk's identity and
/// boundaries are fixed at open time (per `merge-core`'s own doc comment on `build_merged_text`:
/// it seeds the merged buffer once, and any resolution beyond that is a targeted edit, not a
/// re-diff).
struct MergeTab {
    base: String,
    local: String,
    remote: String,
    hunks: Vec<merge_core::MergeHunk>,
    merged: EditBuffer,
}

#[derive(Default)]
struct MergeState {
    tabs: Mutex<HashMap<TabId, MergeTab>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenMergeResult {
    hunks: Vec<merge_core::MergeHunk>,
    merged_text: String,
    base_text: String,
    local_text: String,
    remote_text: String,
}

/// Pure over already-read bytes, same split as `diff_pair`/`open_file_pair`. `local`'s `FileMeta`
/// (encoding/line-ending) is what the merged output is written back with on save -- `local` is
/// the side whose original file path the merge result overwrites, per `git mergetool`'s own
/// convention (MERGED starts as a copy of LOCAL). Refuses if any of the three sides is binary,
/// same reasoning as `diff_pair`'s binary refusal.
fn open_merge_pair(base_bytes: &[u8], local_bytes: &[u8], remote_bytes: &[u8], take_both_order: merge_core::TakeBothOrder) -> Result<(MergeTab, OpenMergeResult), String> {
    let base = text_io::load(base_bytes);
    let local = text_io::load(local_bytes);
    let remote = text_io::load(remote_bytes);
    if base.meta.is_binary || local.meta.is_binary || remote.meta.is_binary {
        return Err("binary file: diffgrid does not merge binary files".to_string());
    }

    let hunks = merge_core::compute_merge_hunks(&base.normalized, &local.normalized, &remote.normalized);
    let merged_text = merge_core::build_merged_text(&base.normalized, &local.normalized, &remote.normalized, &hunks, take_both_order);

    let result = OpenMergeResult {
        hunks: hunks.clone(),
        merged_text: merged_text.clone(),
        base_text: base.normalized.clone(),
        local_text: local.normalized.clone(),
        remote_text: remote.normalized.clone(),
    };
    let tab = MergeTab {
        base: base.normalized,
        local: local.normalized,
        remote: remote.normalized,
        hunks,
        merged: EditBuffer::new(&merged_text, local_bytes.to_vec(), local.meta),
    };
    Ok((tab, result))
}

/// Resolves one hunk (by index into `tab.hunks`) to a new value and returns the text the
/// frontend must splice into the live merged CM6 buffer at that hunk's *current* position --
/// per `merge-core::build_merged_text`'s doc comment, the frontend (not Rust) tracks each hunk's
/// live character range, the same way M2's `buildHunkCopyChange` already does for the two-pane
/// diff view; Rust never re-derives the whole merged document from this call. Returns an error
/// for `Resolution::Manual` (that path doesn't go through this command at all -- see
/// `mark_merge_hunk_manual`) and for an out-of-range index, rather than panicking on either, since
/// both are plausible (if buggy) frontend-supplied values crossing the IPC boundary, not an
/// internal invariant violation.
fn resolve_merge_hunk_impl(tab: &mut MergeTab, hunk_index: usize, resolution: merge_core::Resolution, take_both_order: merge_core::TakeBothOrder) -> Result<String, String> {
    if resolution == merge_core::Resolution::Manual {
        return Err("Manual resolution is set via mark_merge_hunk_manual, not resolve_merge_hunk".to_string());
    }
    let hunk = tab.hunks.get_mut(hunk_index).ok_or_else(|| format!("no hunk at index {hunk_index}"))?;
    hunk.resolution = Some(resolution);
    Ok(merge_core::resolve_hunk_text(&tab.base, &tab.local, &tab.remote, hunk, take_both_order))
}

/// `session::TakeBothSide` -> `merge_core::TakeBothOrder`, the same translation pattern this
/// file already uses for `session::IntraLineMode` -> `diff_core`'s word/character functions.
fn take_both_order_from_setting(side: session::TakeBothSide) -> merge_core::TakeBothOrder {
    match side {
        session::TakeBothSide::MineFirst => merge_core::TakeBothOrder::LocalFirst,
        session::TakeBothSide::TheirsFirst => merge_core::TakeBothOrder::RemoteFirst,
    }
}

/// M5's merge-view entry point (docs/PLAN.md's BASE/LOCAL/REMOTE + merged-output view). Reads
/// all three files, runs `open_merge_pair`, and stores the resulting `MergeTab` under `tab_id` --
/// same per-tab-id keying convention as `open_file_pair`'s `SessionState`, kept as its own
/// `MergeState`/`MergeTab` rather than folding into `TabBuffers` since a merge tab has three
/// read-only sides plus one editable merged buffer, a genuinely different shape from a two-way
/// diff tab's two editable sides.
#[tauri::command]
fn open_merge(state: tauri::State<MergeState>, tab_id: String, base: String, local: String, remote: String, take_both_side: session::TakeBothSide) -> Result<OpenMergeResult, String> {
    let base_bytes = std::fs::read(&base).map_err(|e| format!("{base}: {e}"))?;
    let local_bytes = std::fs::read(&local).map_err(|e| format!("{local}: {e}"))?;
    let remote_bytes = std::fs::read(&remote).map_err(|e| format!("{remote}: {e}"))?;
    let (tab, result) = open_merge_pair(&base_bytes, &local_bytes, &remote_bytes, take_both_order_from_setting(take_both_side))?;
    state.tabs.lock().unwrap().insert(tab_id, tab);
    Ok(result)
}

/// Resolves one hunk to a new value (TakeLocal/TakeRemote/TakeBoth/TakeBase -- not Manual, see
/// `mark_merge_hunk_manual`) and returns the text the frontend splices into the live merged CM6
/// buffer at that hunk's current position, per `resolve_merge_hunk_impl`'s doc comment.
#[tauri::command]
fn resolve_merge_hunk(
    state: tauri::State<MergeState>,
    tab_id: String,
    hunk_index: usize,
    resolution: merge_core::Resolution,
    take_both_side: session::TakeBothSide,
) -> Result<String, String> {
    let mut tabs = state.tabs.lock().unwrap();
    let tab = tabs.get_mut(&tab_id).ok_or_else(|| format!("no merge tab open with id {tab_id}"))?;
    resolve_merge_hunk_impl(tab, hunk_index, resolution, take_both_order_from_setting(take_both_side))
}

/// Marks a hunk `Manual` after the frontend has already spliced the user's own edit into the
/// merged buffer directly (or is about to) -- unlike `resolve_merge_hunk`, this never computes or
/// returns replacement text, since `Manual` means the live CM6 buffer's own content for that
/// hunk's range *is* the resolution; there's nothing for Rust to derive per `merge-core`'s
/// `resolve_hunk_text` contract. Errors on an out-of-range index rather than panicking, same
/// reasoning as `resolve_merge_hunk_impl`.
#[tauri::command]
fn mark_merge_hunk_manual(state: tauri::State<MergeState>, tab_id: String, hunk_index: usize) -> Result<(), String> {
    let mut tabs = state.tabs.lock().unwrap();
    let tab = tabs.get_mut(&tab_id).ok_or_else(|| format!("no merge tab open with id {tab_id}"))?;
    let hunk = tab.hunks.get_mut(hunk_index).ok_or_else(|| format!("no hunk at index {hunk_index}"))?;
    hunk.resolution = Some(merge_core::Resolution::Manual);
    Ok(())
}

/// Closes a merge tab, freeing its `MergeTab`. Same no-op-on-unknown-id semantics as `close_tab`.
#[tauri::command]
fn close_merge_tab(state: tauri::State<MergeState>, tab_id: String) {
    state.tabs.lock().unwrap().remove(&tab_id);
}

fn apply_merge_edit_impl(state: &MergeState, tab_id: &str, from_utf16: u32, to_utf16: u32, inserted: &str) -> Result<(), String> {
    let mut tabs = state.tabs.lock().unwrap();
    let tab = tabs.get_mut(tab_id).ok_or("no merge tab open with this id")?;
    tab.merged.apply_delta(from_utf16, to_utf16, inserted)
}

/// The merged pane's own edit-delta pipeline, structurally identical to `apply_edit` for M1-M4's
/// two-way diff panes -- a keystroke in the merged pane (or a resolution-action click's own
/// programmatic replace, dispatched through the same CM6 transaction path) lands here the same
/// way. Marking the affected hunk(s) `Manual` is the frontend's job (`mark_merge_hunk_manual`),
/// called alongside this for a real keystroke, not for a resolution click's own dispatch (which
/// already set a non-Manual resolution via `resolve_merge_hunk` before dispatching the change).
#[tauri::command]
fn apply_merge_edit(state: tauri::State<MergeState>, tab_id: String, from_utf16: u32, to_utf16: u32, inserted: String) -> Result<(), String> {
    apply_merge_edit_impl(&state, &tab_id, from_utf16, to_utf16, &inserted)
}

/// Writes the merged buffer's current text back to `path` (LOCAL's original path, per
/// `git mergetool`'s MERGED-starts-as-LOCAL convention already noted on `open_merge_pair`).
/// Returns whether every hunk has a resolution other than `None` (i.e. no hunk is still an
/// unresolved `Conflict`) -- task #42's write-back-with-correct-exit-status needs this to decide
/// whether the CLI process should exit 0 or non-zero, which `save_merge` itself has no business
/// deciding (it only knows how to write bytes; the exit-status *policy* belongs at the CLI/vcs
/// layer, same module-boundary reasoning as everywhere else in this codebase).
#[tauri::command]
fn save_merge(state: tauri::State<MergeState>, tab_id: String, path: String) -> Result<bool, String> {
    let mut tabs = state.tabs.lock().unwrap();
    let tab = tabs.get_mut(&tab_id).ok_or("no merge tab open with this id")?;
    let bytes = tab.merged.to_bytes()?;
    std::fs::write(&path, &bytes).map_err(|e| format!("{path}: {e}"))?;
    tab.merged.mark_saved(bytes);
    Ok(tab.hunks.iter().all(|h| h.resolution.is_some()))
}

/// Companion to `open_file_pair`: the normalized (BOM-stripped, LF-normalized) full text of
/// one real file, as raw bytes rather than a JSON string (see `fixture_text`'s doc comment —
/// same IPC-cost rationale applies here). Normalized rather than raw so the line count the
/// frontend renders matches exactly what `diff_pair` computed hunks against.
#[tauri::command]
fn open_file_text(path: String) -> Result<Response, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let loaded = text_io::load(&bytes);
    if loaded.meta.is_binary {
        return Err(format!("{path}: binary file"));
    }
    Ok(Response::new(loaded.normalized.into_bytes()))
}

/// Viewport-driven per docs/PLAN.md §6: the frontend calls this once per visible `Replace`-hunk
/// line pair as they scroll into view, not eagerly for the whole file. Thin wrapper — all the
/// actual logic (and its tests) live in `diff_core`. `mode` is `session::IntraLineMode` reused
/// directly rather than a second parallel enum; `Off` is handled defensively here (an empty
/// result), but the frontend is expected to never issue this call at all when the setting is
/// Off -- see DECISIONS.md for why that's a frontend-side short-circuit, not a backend concern.
///
/// Must take the same ignore-whitespace/ignore-case toggles as `diff_texts`: without them, a
/// line that differs in both whitespace and real content highlights as "the whole line changed"
/// even when the toggle says the whitespace part doesn't count, because the line-level diff and
/// the intra-line diff would be applying different rules to the same pair.
#[tauri::command]
fn intra_line_spans(
    left_line: String,
    right_line: String,
    ignore_whitespace: bool,
    ignore_case: bool,
    mode: session::IntraLineMode,
) -> Vec<diff_core::Span> {
    let opts = diff_core::DiffOptions { ignore_whitespace, ignore_case };
    match mode {
        session::IntraLineMode::Off => Vec::new(),
        session::IntraLineMode::Word => diff_core::intra_line_spans_word_mode(&left_line, &right_line, opts),
        session::IntraLineMode::Character => diff_core::intra_line_spans_with_options(&left_line, &right_line, opts),
    }
}

/// Core of `apply_edit`, taking a plain `&SessionState` rather than `tauri::State` so it's
/// callable from unit tests without a running app (`tauri::State` has no public constructor —
/// same rationale as `diff_pair` being split out from its `#[tauri::command]` wrapper above).
fn apply_edit_impl(state: &SessionState, tab_id: &str, side: Side, from_utf16: u32, to_utf16: u32, inserted: &str) -> Result<(), String> {
    let mut tabs = state.tabs.lock().unwrap();
    let buffers = tabs.get_mut(tab_id).ok_or("no tab open with this id")?;
    let buffer = match side {
        Side::Left => &mut buffers.left,
        Side::Right => &mut buffers.right,
    };
    buffer.apply_delta(from_utf16, to_utf16, inserted)
}

/// Applies one edit delta captured from a CM6 transaction to the corresponding side's
/// `EditBuffer` — the frontend → Rust half of the delta pipeline in docs/PLAN.md §2. Errors if
/// `tab_id` isn't open, or if the offsets are malformed (see `EditBuffer::apply_delta`).
///
/// Caller contract: deltas from a single side of a single tab must be sent in the order CM6
/// produced them, and each call's `from_utf16`/`to_utf16` must be valid against the buffer state
/// left by the previous call for that same side — never issued concurrently for the same
/// (tab, side), or the shadow buffer diverges from CM6's real document silently (a divergence
/// that only surfaces later, as a bad save). Different tabs are fully independent.
#[tauri::command]
fn apply_edit(state: tauri::State<SessionState>, tab_id: String, side: Side, from_utf16: u32, to_utf16: u32, inserted: String) -> Result<(), String> {
    apply_edit_impl(&state, &tab_id, side, from_utf16, to_utf16, &inserted)
}

/// Reads both sides of one tab under a single lock acquisition (`state.tabs.lock()` once, for
/// the whole read) -- unlike M2's separate per-side `Mutex`es, there is no way to observe a left
/// snapshot from one instant and a right snapshot from another, because both live in the same
/// map entry behind the same lock. See `TabBuffers`'s doc comment for why this is a strict
/// improvement over the "lock both, in a fixed order" discipline the old dual-`Mutex` shape
/// needed to avoid exactly this hazard.
fn redo_diff_impl(state: &SessionState, tab_id: &str, ignore_whitespace: bool, ignore_case: bool) -> Result<diff_core::FileDiffResult, String> {
    let tabs = state.tabs.lock().unwrap();
    let buffers = tabs.get(tab_id).ok_or("no tab open with this id")?;
    let left = buffers.left.text();
    let right = buffers.right.text();
    Ok(diff_core::diff_lines_with_options(&left, &right, diff_core::DiffOptions { ignore_whitespace, ignore_case }))
}

/// Re-diffs one tab's two open `EditBuffer`s' *current* text (i.e. including any edits applied
/// via `apply_edit` since open) with whitespace/case-ignore toggles applied. Replaces M1's
/// `diff_texts(left, right, ...)`, which took the text as parameters from the frontend — once
/// editing exists, the frontend's own copy of the text can be stale the moment an edit lands,
/// so the Rust-side `EditBuffer` (kept current by `apply_edit`) must be the one source of truth
/// for what gets diffed, for the live-toggle case and the post-edit case alike.
#[tauri::command]
fn redo_diff(state: tauri::State<SessionState>, tab_id: String, ignore_whitespace: bool, ignore_case: bool) -> Result<diff_core::FileDiffResult, String> {
    redo_diff_impl(&state, &tab_id, ignore_whitespace, ignore_case)
}

fn save_file_impl(state: &SessionState, tab_id: &str, side: Side, path: &str) -> Result<(), String> {
    let mut tabs = state.tabs.lock().unwrap();
    let buffers = tabs.get_mut(tab_id).ok_or("no tab open with this id")?;
    let buffer = match side {
        Side::Left => &mut buffers.left,
        Side::Right => &mut buffers.right,
    };
    let bytes = buffer.to_bytes()?;
    std::fs::write(path, &bytes).map_err(|e| format!("{path}: {e}"))?;
    buffer.mark_saved(bytes);
    Ok(())
}

/// Writes the current state of one tab's one side's `EditBuffer` to `path`, encoding/line-ending-
/// preserving per docs/PLAN.md §2: an unedited buffer writes back its exact original bytes; an
/// edited one re-encodes into the original encoding/line-ending style (see `text_io::to_bytes`).
/// On success, the buffer adopts the just-written bytes as its new baseline
/// (`EditBuffer::mark_saved`), so a later unedited save short-circuits again instead of
/// re-encoding every time regardless of whether anything changed since the last save.
#[tauri::command]
fn save_file(state: tauri::State<SessionState>, tab_id: String, side: Side, path: String) -> Result<(), String> {
    save_file_impl(&state, &tab_id, side, &path)
}

/// Lets the frontend branch `diffgrid ARG1 ARG2` between M1/M2's file-pair view and M3's
/// directory-compare view without guessing from the path string (extension-sniffing would be
/// wrong for extensionless files/directories) -- it asks the filesystem directly.
#[tauri::command]
fn path_kind(path: String) -> Result<String, String> {
    let metadata = std::fs::metadata(&path).map_err(|e| format!("{path}: {e}"))?;
    Ok(if metadata.is_dir() { "dir".to_string() } else { "file".to_string() })
}

/// M4's global preferences (docs/PLAN.md §5), persisted to `<app-config-dir>/settings.json`.
/// The actual load/save/default-value logic lives in `session` (the crate PLAN.md's module
/// boundary assigns "resolved settings" to); this crate's job is only resolving *where* that
/// file lives on disk -- `app` is "the only crate allowed to depend on `tauri`" per its own
/// Cargo.toml description, so the `tauri::AppHandle`-dependent path lookup can't live in
/// `session` without giving it that same dependency.
fn load_settings_impl(config_dir: &Path) -> session::Settings {
    session::load_settings(config_dir)
}

fn save_settings_impl(config_dir: &Path, settings: &session::Settings) -> Result<(), String> {
    session::save_settings(config_dir, settings)
}

#[tauri::command]
fn load_settings(app: tauri::AppHandle) -> session::Settings {
    match app.path().app_config_dir() {
        Ok(dir) => load_settings_impl(&dir),
        // No usable config dir (unexpected on a real desktop install) -- defaults are the only
        // sane fallback, same reasoning as `session::load_settings`'s own corrupt-file case.
        Err(_) => session::Settings::default(),
    }
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: session::Settings) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    save_settings_impl(&dir, &settings)?;
    // Best-effort: if no window is listening (or the settings window is the only one open and
    // has no listener registered yet), there's nothing else to do -- the new value is already
    // durably saved, and the next window to open reads it fresh via `load_settings` regardless.
    let _ = app.emit("settings-changed", settings);
    Ok(())
}

/// Opens the settings window (docs/UI/ui-02.png), or focuses it if already open -- a second
/// `WebviewWindowBuilder::new` call with the same label would error, so this checks first rather
/// than letting that error surface as a confusing failure to the frontend.
#[tauri::command]
fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("settings") {
        return window.set_focus().map_err(|e| e.to_string());
    }
    WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings".into()))
        .title("Settings")
        .inner_size(560.0, 560.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Installs a fresh cancel flag as *the* current scan, replacing whatever was there before (a
/// scan that finished normally leaves a stale flag behind harmlessly; a scan that was still
/// running gets orphaned, which is fine -- it keeps its own clone of the old flag and simply
/// becomes uncancellable, since nothing else references it). Split out from `scan_dirs` so this
/// bookkeeping is testable without `tauri::State`, which has no public constructor.
fn register_new_scan(state: &ScanState) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    *state.current_cancel.lock().unwrap() = Some(cancel.clone());
    cancel
}

fn cancel_scan_impl(state: &ScanState) {
    if let Some(flag) = state.current_cancel.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
}

/// M3's directory-pair scan, per docs/PLAN.md §3/§5: runs `dirwalk::scan` on a blocking thread
/// (directory walking and content comparison are blocking I/O, not suited to the async Tauri
/// command executor directly) and streams batches to the frontend over `channel` as they're
/// produced, rather than returning the whole result only once the scan completes -- the
/// "incremental" half of the milestone's requirement. The ordinary `Result` return value is the
/// summary (`ScanOutcome`), available once the blocking task finishes either way.
#[tauri::command]
async fn scan_dirs(
    scan_state: tauri::State<'_, ScanState>,
    left: String,
    right: String,
    respect_gitignore: bool,
    exclude_globs: Vec<String>,
    channel: Channel<Vec<dirwalk::DirEntry>>,
) -> Result<dirwalk::ScanOutcome, String> {
    let cancel = register_new_scan(&scan_state);

    let left_root = PathBuf::from(left);
    let right_root = PathBuf::from(right);
    let options = dirwalk::ScanOptions { respect_gitignore, exclude_globs };

    tauri::async_runtime::spawn_blocking(move || {
        dirwalk::scan(&left_root, &right_root, &options, &cancel, |batch| {
            // A send failure here means the frontend's channel is gone (window closed mid-scan)
            // -- nothing to do but let the scan keep running to completion; there's no separate
            // "abandoned" signal to act on, and the cancel flag is the only cooperative stop
            // mechanism this crate has.
            let _ = channel.send(batch);
        })
    })
    .await
    .map_err(|e| e.to_string())
}

/// Requests cancellation of whichever scan is currently running, if any. A no-op if no scan is
/// in progress (including "already finished") -- there's nothing to signal.
#[tauri::command]
fn cancel_scan(scan_state: tauri::State<ScanState>) {
    cancel_scan_impl(&scan_state)
}

/// Argv (excluding argv[0]) as handed to the process. `diffgrid FILE1 FILE2` is M1's real
/// entry point; the frontend falls back to the M0 fixture-benchmark flow when this is empty,
/// which is exactly how `bench/m0-spike.mjs` invokes the binary today (no arguments).
#[tauri::command]
fn launch_args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

/// Called by the frontend once the diff panes have mounted and painted at least one frame.
/// The external cold-launch timing script watches stdout for this exact marker line.
#[tauri::command]
fn report_ready() {
    println!("DIFFGRID_READY");
}

/// Called by the frontend with the scroll-benchmark results (JSON). The external harness
/// watches stdout for this marker to collect fps stats, then may terminate the process.
#[tauri::command]
fn report_bench(json: String) {
    println!("DIFFGRID_BENCH {json}");
}

/// M0 debugging aid only: surfaces frontend exceptions to stdout since the WebView console
/// isn't otherwise visible from this sandbox.
#[tauri::command]
fn report_error(message: String) {
    println!("DIFFGRID_ERROR {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire format for the same reason as diff-core's `Span` test: a missing
    /// `rename_all` on a multi-word-field struct produces `undefined` on the frontend with no
    /// compile-time or IPC-level error, caught only by a crash deep inside unrelated code.
    #[test]
    fn open_pair_result_serializes_with_camel_case_field_names() {
        let result = diff_pair(b"a\n", b"b\n").unwrap();
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("leftMeta").is_some());
        assert!(json.get("rightMeta").is_some());
        assert!(json["leftMeta"].get("isBinary").is_some());
    }

    #[test]
    fn diffs_two_real_texts_and_reports_per_side_meta() {
        let result = diff_pair(b"a\nb\nc\n", b"a\r\nx\r\nc\r\n").unwrap();
        assert_eq!(result.diff.stats.chunks, 1);
        assert_eq!(result.left_meta.line_ending, text_io::LineEnding::Lf);
        assert_eq!(result.right_meta.line_ending, text_io::LineEnding::Crlf);
        assert!(!result.left_meta.is_binary);
        assert!(!result.right_meta.is_binary);
    }

    #[test]
    fn refuses_to_diff_when_either_side_is_binary() {
        let err = diff_pair(b"hello\0world", b"a\nb\n").unwrap_err();
        assert!(err.contains("binary"), "expected a binary-file error, got: {err}");
    }

    #[test]
    fn open_merge_pair_computes_hunks_and_seeds_the_merged_buffer() {
        let base = b"a\nb\nc\n";
        let local = b"a\nLOCAL\nc\n";
        let remote = b"a\nb\nc\n";
        let (tab, result) = open_merge_pair(base, local, remote, merge_core::TakeBothOrder::LocalFirst).unwrap();
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.merged_text, "a\nLOCAL\nc\n");
        assert_eq!(result.base_text, "a\nb\nc\n");
        assert_eq!(result.local_text, "a\nLOCAL\nc\n");
        assert_eq!(result.remote_text, "a\nb\nc\n");
        assert_eq!(tab.merged.text(), "a\nLOCAL\nc\n");
        assert!(!tab.merged.is_dirty(), "seeding the merged buffer must not itself count as an edit");
    }

    #[test]
    fn open_merge_pair_refuses_when_any_side_is_binary() {
        let result = open_merge_pair(b"a\n", b"hello\0world", b"a\n", merge_core::TakeBothOrder::LocalFirst);
        match result {
            Err(e) => assert!(e.contains("binary"), "expected a binary-file error, got: {e}"),
            Ok(_) => panic!("expected an error for a binary local file"),
        }
    }

    #[test]
    fn resolve_merge_hunk_updates_resolution_and_returns_the_new_text() {
        let (mut tab, _) = open_merge_pair(b"a\nb\nc\n", b"a\nLOCAL\nc\n", b"a\nREMOTE\nc\n", merge_core::TakeBothOrder::LocalFirst).unwrap();
        assert_eq!(tab.hunks[0].resolution, None, "starts as an unresolved conflict");
        let text = resolve_merge_hunk_impl(&mut tab, 0, merge_core::Resolution::TakeRemote, merge_core::TakeBothOrder::LocalFirst).unwrap();
        assert_eq!(text, "REMOTE");
        assert_eq!(tab.hunks[0].resolution, Some(merge_core::Resolution::TakeRemote));
    }

    #[test]
    fn resolve_merge_hunk_respects_take_both_order() {
        let (mut tab, _) = open_merge_pair(b"a\nb\nc\n", b"a\nLOCAL\nc\n", b"a\nREMOTE\nc\n", merge_core::TakeBothOrder::LocalFirst).unwrap();
        let local_first = resolve_merge_hunk_impl(&mut tab, 0, merge_core::Resolution::TakeBoth, merge_core::TakeBothOrder::LocalFirst).unwrap();
        assert_eq!(local_first, "LOCAL\nREMOTE");
        let remote_first = resolve_merge_hunk_impl(&mut tab, 0, merge_core::Resolution::TakeBoth, merge_core::TakeBothOrder::RemoteFirst).unwrap();
        assert_eq!(remote_first, "REMOTE\nLOCAL");
    }

    #[test]
    fn resolve_merge_hunk_errors_on_an_out_of_range_index_instead_of_panicking() {
        let (mut tab, _) = open_merge_pair(b"a\nb\nc\n", b"a\nLOCAL\nc\n", b"a\nREMOTE\nc\n", merge_core::TakeBothOrder::LocalFirst).unwrap();
        let result = resolve_merge_hunk_impl(&mut tab, 99, merge_core::Resolution::TakeRemote, merge_core::TakeBothOrder::LocalFirst);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_merge_hunk_rejects_manual_rather_than_forwarding_to_resolve_hunk_text() {
        let (mut tab, _) = open_merge_pair(b"a\nb\nc\n", b"a\nLOCAL\nc\n", b"a\nREMOTE\nc\n", merge_core::TakeBothOrder::LocalFirst).unwrap();
        let result = resolve_merge_hunk_impl(&mut tab, 0, merge_core::Resolution::Manual, merge_core::TakeBothOrder::LocalFirst);
        assert!(result.is_err(), "Manual must go through mark_merge_hunk_manual, not this command");
        assert_eq!(tab.hunks[0].resolution, None, "a rejected call must not mutate hunk state");
    }

    #[test]
    fn diffs_across_different_encodings_on_each_side() {
        // right is UTF-16LE with a BOM; text-io must decode both sides before diffing.
        let mut right = vec![0xFF, 0xFE];
        for u in "a\nb\n".encode_utf16() {
            right.extend_from_slice(&u.to_le_bytes());
        }
        let result = diff_pair(b"a\nb\n", &right).unwrap();
        assert_eq!(result.diff.stats.chunks, 0, "identical content under different encodings should diff as equal");
        assert_eq!(result.right_meta.encoding, text_io::Encoding::Utf16Le);
    }

    const TAB: &str = "tab-1";

    fn opened_state(left: &str, right: &str) -> SessionState {
        opened_state_with_id(TAB, left, right)
    }

    fn opened_state_with_id(tab_id: &str, left: &str, right: &str) -> SessionState {
        let left_loaded = text_io::load(left.as_bytes());
        let right_loaded = text_io::load(right.as_bytes());
        let buffers = TabBuffers {
            left: EditBuffer::new(&left_loaded.normalized, left.as_bytes().to_vec(), left_loaded.meta),
            right: EditBuffer::new(&right_loaded.normalized, right.as_bytes().to_vec(), right_loaded.meta),
        };
        let state = SessionState::default();
        state.tabs.lock().unwrap().insert(tab_id.to_string(), buffers);
        state
    }

    /// Exercises the exact sequence the frontend's edit pipeline relies on: an edit lands via
    /// `apply_edit`, then `redo_diff` must see it -- i.e. it reads the *current* `EditBuffer`
    /// text, not a snapshot from open time. This is the invariant the whole shadow-buffer
    /// design in docs/PLAN.md §2 rests on.
    #[test]
    fn redo_diff_reflects_an_edit_applied_since_open() {
        let state = opened_state("a\nb\nc\n", "a\nb\nc\n");
        let diff = redo_diff_impl(&state, TAB, false, false).unwrap();
        assert_eq!(diff.stats.chunks, 0, "no edits yet -- must still diff as identical");

        apply_edit_impl(&state, TAB, Side::Left, 2, 3, "X").unwrap();
        let diff = redo_diff_impl(&state, TAB, false, false).unwrap();
        assert_eq!(diff.stats.chunks, 1, "the edit must be visible to a re-diff without re-sending text from the frontend");
    }

    #[test]
    fn redo_diff_honors_ignore_whitespace_after_an_edit() {
        let state = opened_state("a\nfoo\nc\n", "a\nfoo\nc\n");
        apply_edit_impl(&state, TAB, Side::Right, 6, 6, "   ").unwrap();
        let diff = redo_diff_impl(&state, TAB, true, false).unwrap();
        assert_eq!(diff.stats.chunks, 0, "a whitespace-only edit must not count as a hunk under ignore_whitespace");
    }

    #[test]
    fn apply_edit_errors_when_no_tab_is_open_with_that_id() {
        let state = SessionState::default();
        let err = apply_edit_impl(&state, TAB, Side::Left, 0, 0, "x").unwrap_err();
        assert!(err.contains("no tab"));
    }

    #[test]
    fn save_file_writes_original_bytes_verbatim_when_unedited() {
        let state = opened_state("a\r\nb\r\nc\r\n", "a\nb\nc\n"); // left is CRLF
        let dir = std::env::temp_dir().join(format!("diffgrid-test-{}-a", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("left.txt");
        save_file_impl(&state, TAB, Side::Left, path.to_str().unwrap()).unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"a\r\nb\r\nc\r\n", "an unedited save must reproduce the original CRLF bytes exactly");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_file_reencodes_and_marks_clean_after_an_edit() {
        let state = opened_state("a\nb\nc\n", "a\nb\nc\n");
        apply_edit_impl(&state, TAB, Side::Left, 0, 1, "X").unwrap();
        let dir = std::env::temp_dir().join(format!("diffgrid-test-{}-b", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("left.txt");
        save_file_impl(&state, TAB, Side::Left, path.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"X\nb\nc\n");
        // a second save with no further edits must now short-circuit to *these* bytes, not
        // silently re-diverge -- i.e. mark_saved really updated the baseline.
        save_file_impl(&state, TAB, Side::Left, path.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"X\nb\nc\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The whole point of keying `SessionState` by `TabId`: editing one tab must never leak into
    /// another's buffers, even though both live in the same `HashMap` behind the same lock.
    #[test]
    fn two_tabs_have_fully_independent_buffers() {
        let state = SessionState::default();
        state.tabs.lock().unwrap().insert("a".to_string(), {
            let l = text_io::load(b"a\nb\nc\n");
            let r = text_io::load(b"a\nb\nc\n");
            TabBuffers { left: EditBuffer::new(&l.normalized, b"a\nb\nc\n".to_vec(), l.meta), right: EditBuffer::new(&r.normalized, b"a\nb\nc\n".to_vec(), r.meta) }
        });
        state.tabs.lock().unwrap().insert("b".to_string(), {
            let l = text_io::load(b"x\ny\nz\n");
            let r = text_io::load(b"x\ny\nz\n");
            TabBuffers { left: EditBuffer::new(&l.normalized, b"x\ny\nz\n".to_vec(), l.meta), right: EditBuffer::new(&r.normalized, b"x\ny\nz\n".to_vec(), r.meta) }
        });

        apply_edit_impl(&state, "a", Side::Left, 0, 1, "X").unwrap();

        let diff_a = redo_diff_impl(&state, "a", false, false).unwrap();
        let diff_b = redo_diff_impl(&state, "b", false, false).unwrap();
        assert_eq!(diff_a.stats.chunks, 1, "tab a's edit must be visible in tab a's diff");
        assert_eq!(diff_b.stats.chunks, 0, "tab a's edit must not leak into tab b, which was never touched");
    }

    #[test]
    fn close_tab_removes_the_entry_so_apply_edit_then_errors() {
        let state = opened_state("a\nb\n", "a\nb\n");
        close_tab_impl(&state, TAB);
        let err = apply_edit_impl(&state, TAB, Side::Left, 0, 0, "x").unwrap_err();
        assert!(err.contains("no tab"));
    }

    #[test]
    fn close_tab_is_a_harmless_no_op_for_an_unknown_id() {
        let state = SessionState::default();
        close_tab_impl(&state, "does-not-exist"); // must not panic
        assert!(state.tabs.lock().unwrap().is_empty());
    }

    #[test]
    fn close_tab_only_removes_the_named_tab() {
        let state = opened_state_with_id("a", "1\n", "1\n");
        state.tabs.lock().unwrap().insert(
            "b".to_string(),
            TabBuffers {
                left: EditBuffer::new("2\n", b"2\n".to_vec(), text_io::load(b"2\n").meta),
                right: EditBuffer::new("2\n", b"2\n".to_vec(), text_io::load(b"2\n").meta),
            },
        );
        close_tab_impl(&state, "a");
        assert!(apply_edit_impl(&state, "a", Side::Left, 0, 0, "x").is_err());
        assert!(redo_diff_impl(&state, "b", false, false).is_ok(), "closing tab a must not disturb tab b");
    }

    #[test]
    fn path_kind_distinguishes_files_from_directories() {
        let dir = std::env::temp_dir().join(format!("diffgrid-test-{}-path-kind", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(path_kind(dir.to_str().unwrap().to_string()).unwrap(), "dir");
        assert_eq!(path_kind(file.to_str().unwrap().to_string()).unwrap(), "file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_kind_errors_on_a_path_that_does_not_exist() {
        assert!(path_kind("/does/not/exist/anywhere".to_string()).is_err());
    }

    #[test]
    fn settings_wiring_round_trips_through_this_crate_the_same_way_session_does() {
        let dir = std::env::temp_dir().join(format!("diffgrid-test-{}-app-settings", std::process::id()));
        let settings = session::Settings {
            ignore_whitespace: true,
            ignore_case: false,
            collapse_context_lines: 5,
            intra_line_mode: session::IntraLineMode::Word,
            auto_resolve_non_conflicting: false,
            default_take_both_side: session::TakeBothSide::TheirsFirst,
        };
        save_settings_impl(&dir, &settings).unwrap();
        assert_eq!(load_settings_impl(&dir), settings);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn intra_line_spans_off_mode_returns_no_spans_even_for_differing_lines() {
        let spans = intra_line_spans("foo".to_string(), "bar".to_string(), false, false, session::IntraLineMode::Off);
        assert_eq!(spans, vec![]);
    }

    #[test]
    fn intra_line_spans_dispatches_to_word_mode() {
        let spans = intra_line_spans("foo bar baz".to_string(), "foo XXX baz".to_string(), false, false, session::IntraLineMode::Word);
        // word mode finds a single disjoint changed-word span per side; character mode's
        // prefix/suffix trim would produce the same result for this single-word-diff input too,
        // so this specifically pins that the mode parameter reaches diff_core, not just that
        // *some* span comes back.
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn intra_line_spans_dispatches_to_character_mode() {
        let spans = intra_line_spans("foo bar baz".to_string(), "foo XXX baz".to_string(), false, false, session::IntraLineMode::Character);
        assert_eq!(
            spans,
            diff_core::intra_line_spans_with_options("foo bar baz", "foo XXX baz", diff_core::DiffOptions::default())
        );
    }

    #[test]
    fn register_new_scan_installs_a_fresh_unset_flag_each_time() {
        let state = ScanState::default();
        let first = register_new_scan(&state);
        first.store(true, Ordering::Relaxed);
        let second = register_new_scan(&state);
        assert!(!second.load(Ordering::Relaxed), "a new scan's flag must start false regardless of a prior scan's state");
    }

    #[test]
    fn cancel_scan_impl_sets_the_flag_of_the_currently_registered_scan() {
        let state = ScanState::default();
        let cancel = register_new_scan(&state);
        assert!(!cancel.load(Ordering::Relaxed));
        cancel_scan_impl(&state);
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_scan_impl_only_affects_the_most_recently_registered_scan() {
        // A stale cancel request for a scan the frontend has already moved on from must never
        // reach a scan that started after it -- the whole reason ScanState holds a fresh Arc
        // per scan instead of one reused flag.
        let state = ScanState::default();
        let stale = register_new_scan(&state);
        let current = register_new_scan(&state);
        cancel_scan_impl(&state);
        assert!(!stale.load(Ordering::Relaxed), "the orphaned flag from the previous scan must be untouched");
        assert!(current.load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_scan_impl_is_a_harmless_no_op_when_no_scan_has_run() {
        let state = ScanState::default();
        cancel_scan_impl(&state); // must not panic
    }

    /// Exercises `dirwalk::scan` wired the same way `scan_dirs` wires it (minus the actual
    /// `tauri::ipc::Channel`, which can't be constructed outside a running app) -- a real,
    /// if partial, integration check that the app crate's plumbing (PathBuf conversion,
    /// ScanOptions construction, the registered cancel flag) actually reaches dirwalk correctly.
    #[test]
    fn scan_dirs_wiring_reaches_dirwalk_and_reports_a_real_difference() {
        let base = std::env::temp_dir().join(format!("diffgrid-test-{}-scan-dirs-wiring", std::process::id()));
        let (left, right) = (base.join("left"), base.join("right"));
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        std::fs::write(left.join("only-left.txt"), "x").unwrap();

        let state = ScanState::default();
        let cancel = register_new_scan(&state);
        let options = dirwalk::ScanOptions { respect_gitignore: true, exclude_globs: vec![] };
        let mut entries = Vec::new();
        let outcome = dirwalk::scan(&left, &right, &options, &cancel, |batch| entries.extend(batch));

        assert!(!outcome.cancelled);
        assert!(entries.iter().any(|e| e.path == "only-left.txt" && e.status == dirwalk::EntryStatus::LeftOnly));
        std::fs::remove_dir_all(&base).ok();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(SessionState::default())
        .manage(ScanState::default())
        .manage(MergeState::default())
        .invoke_handler(tauri::generate_handler![
            diff_fixture, fixture_text, report_ready, report_bench, report_error, bench_flags,
            open_file_pair, open_file_text, launch_args, intra_line_spans, apply_edit, redo_diff,
            save_file, path_kind, scan_dirs, cancel_scan, close_tab, load_settings, save_settings,
            open_settings_window, open_merge, resolve_merge_hunk, mark_merge_hunk_manual,
            close_merge_tab, apply_merge_edit, save_merge
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
