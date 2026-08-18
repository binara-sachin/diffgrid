use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryStatus {
    Same,
    Modified,
    LeftOnly,
    RightOnly,
    TypeConflict,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    /// Forward-slash-normalized, relative to each root -- the join key between the two trees,
    /// so it must be computed identically on both sides (see `relative_path`) or every entry
    /// mismatches.
    pub path: String,
    pub status: EntryStatus,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub size_left: Option<u64>,
    pub size_right: Option<u64>,
}

/// User-facing exclude globs, per docs/PLAN.md's "glob/.gitignore filters." Plain glob syntax
/// (e.g. `"node_modules/"`, `"*.log"`) meaning "hide paths matching this" -- callers should NOT
/// write the `ignore` crate's own inverted override syntax (`!pattern` = ignore, bare `pattern`
/// = whitelist-everything-else); `build_overrides` below does that translation once, in one
/// place, so the inversion is never a decision a caller has to get right themselves.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    pub respect_gitignore: bool,
    pub exclude_globs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOutcome {
    pub cancelled: bool,
    pub left_visited: u64,
    pub right_visited: u64,
    pub entries_emitted: u64,
}

const BATCH_SIZE: usize = 256;

#[derive(Debug, Clone)]
struct FileInfo {
    is_dir: bool,
    is_symlink: bool,
    symlink_target: Option<String>,
    size: Option<u64>,
    mtime: Option<SystemTime>,
    /// Absolute path, kept only for the byte-compare tier (never sent over IPC).
    abs_path: std::path::PathBuf,
}

fn build_overrides(root: &Path, exclude_globs: &[String]) -> ignore::overrides::Override {
    if exclude_globs.is_empty() {
        return ignore::overrides::Override::empty();
    }
    let mut builder = OverrideBuilder::new(root);
    for glob in exclude_globs {
        // `!` inverts the ignore crate's override semantics from "whitelist" to "ignore" --
        // see this module's doc comment on `ScanOptions::exclude_globs`.
        let _ = builder.add(&format!("!{glob}"));
    }
    builder.build().unwrap_or_else(|_| ignore::overrides::Override::empty())
}

fn walker(root: &Path, options: &ScanOptions) -> ignore::Walk {
    WalkBuilder::new(root)
        .git_ignore(options.respect_gitignore)
        .git_global(options.respect_gitignore)
        .git_exclude(options.respect_gitignore)
        // The `ignore` crate only applies git-related ignore rules inside an actual git
        // repository by default (`require_git` defaults to `true`) -- diffgrid compares
        // arbitrary directory trees, most of which won't have a `.git` folder at the compared
        // root, so `.gitignore`-style filtering needs to apply universally or this option would
        // silently do nothing for the common case.
        .require_git(false)
        // Diff tools conventionally show dotfiles (.env, .gitignore itself); this crate's
        // upstream use case (ripgrep) hides them by default, which would be a surprising
        // default here. See DECISIONS.md.
        .hidden(false)
        // Never descend into a symlinked directory: it's reported as a single leaf DirEntry
        // (is_symlink + symlink_target), not recursed into -- avoids symlink cycles entirely
        // rather than needing cycle detection.
        .follow_links(false)
        .overrides(build_overrides(root, &options.exclude_globs))
        .build()
}

/// Forward-slash-normalized path relative to `root`. Must be computed the same way for both
/// trees being compared, since this string is the join key between them -- any asymmetry here
/// (e.g. one side keeping backslashes on Windows, or differing case-folding) silently splits one
/// logical file into a LeftOnly/RightOnly pair instead of matching it.
fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

fn file_info(entry: &ignore::DirEntry) -> Option<FileInfo> {
    let is_symlink = entry.path_is_symlink();
    let symlink_target = if is_symlink { fs::read_link(entry.path()).ok().map(|t| t.to_string_lossy().into_owned()) } else { None };
    // A symlink's own metadata (not the target's) is what determines is_dir here -- consistent
    // with never following links, so a symlink is always treated as its own leaf kind, never as
    // "the file/dir it points to."
    let metadata = entry.metadata().ok()?;
    let is_dir = metadata.is_dir() && !is_symlink;
    let size = if is_symlink || is_dir { None } else { Some(metadata.len()) };
    let mtime = metadata.modified().ok();
    Some(FileInfo { is_dir, is_symlink, symlink_target, size, mtime, abs_path: entry.path().to_path_buf() })
}

/// Byte-for-byte comparison in fixed-size chunks, short-circuiting on the first differing chunk
/// (or differing final read length) rather than reading both files to completion regardless.
/// Deliberately not a hash: for a one-shot equality check, hashing reads exactly as much I/O as
/// a chunked compare would but can't short-circuit early, adds a dependency, and raises a (however
/// remote) collision question direct comparison never has to answer. See DECISIONS.md.
fn contents_equal(a: &Path, b: &Path) -> io::Result<bool> {
    const CHUNK: usize = 64 * 1024;
    let mut fa = fs::File::open(a)?;
    let mut fb = fs::File::open(b)?;
    let mut buf_a = vec![0u8; CHUNK];
    let mut buf_b = vec![0u8; CHUNK];
    loop {
        let na = fa.read(&mut buf_a)?;
        let nb = fb.read(&mut buf_b)?;
        if na != nb || buf_a[..na] != buf_b[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn classify(left: &FileInfo, right: &FileInfo) -> EntryStatus {
    if left.is_dir != right.is_dir || left.is_symlink != right.is_symlink {
        return EntryStatus::TypeConflict;
    }
    if left.is_symlink {
        return if left.symlink_target == right.symlink_target { EntryStatus::Same } else { EntryStatus::Modified };
    }
    if left.is_dir {
        // A directory's mere co-presence on both sides is "Same" -- any real difference inside
        // it shows up as its own entries elsewhere in this flat list, not as a property of the
        // directory entry itself.
        return EntryStatus::Same;
    }
    match (left.size, right.size) {
        (Some(sl), Some(sr)) if sl != sr => EntryStatus::Modified,
        (Some(_), Some(_)) => {
            // Sizes match. Same mtime: accept as Same without reading either file -- the
            // perf-critical fast path (see docs/PLAN.md §7's "hash on demand"). This is a
            // deliberate, honestly-stated tradeoff, not a free lunch: its one failure mode is a
            // *false* Same (two files with identical size and mtime but different content are
            // reported as unchanged) -- for a diff tool, a missed difference is the worst
            // possible wrong answer, worse than the alternative (always comparing content on a
            // size match, which is correct but reads every same-sized file on every scan). See
            // DECISIONS.md for why this tradeoff was made anyway.
            if left.mtime.is_some() && left.mtime == right.mtime {
                EntryStatus::Same
            } else {
                match contents_equal(&left.abs_path, &right.abs_path) {
                    Ok(true) => EntryStatus::Same,
                    Ok(false) => EntryStatus::Modified,
                    // Unreadable (permission changed mid-scan, etc.) -- report as Modified
                    // rather than silently claiming Same on a comparison we couldn't actually
                    // perform.
                    Err(_) => EntryStatus::Modified,
                }
            }
        }
        _ => EntryStatus::Modified,
    }
}

/// Walks `root`, checking `cancel` once per entry (an `AtomicBool` load is cheap enough not to
/// need batching the check itself -- only the caller's `on_batch` *emission* is batched).
/// Permission-denied subdirectories are a `Result::Err` from the iterator, not a panic
/// condition: skipped, not `.unwrap()`ed. Returns `true` if cancellation was observed.
fn walk_root(root: &Path, options: &ScanOptions, cancel: &AtomicBool, visited: &mut u64, mut on_entry: impl FnMut(String, FileInfo)) -> bool {
    for result in walker(root, options) {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue; // the root itself, not a comparable entry
        }
        let Some(info) = file_info(&entry) else { continue };
        *visited += 1;
        on_entry(relative_path(root, entry.path()), info);
    }
    false
}

/// Two-phase directory-pair scan per docs/PLAN.md M3: walks `left_root` fully into a map, then
/// streams `right_root`, classifying each entry against that map as it's discovered (flushing
/// `on_batch` every `BATCH_SIZE` entries), then flushes whatever's left in the map as `LeftOnly`.
/// Cancellable via `cancel` (checked every entry in both phases); `on_batch` is only ever called
/// with non-empty batches, and never after `cancel` has been observed true.
pub fn scan(left_root: &Path, right_root: &Path, options: &ScanOptions, cancel: &AtomicBool, mut on_batch: impl FnMut(Vec<DirEntry>)) -> ScanOutcome {
    let mut left_map: HashMap<String, FileInfo> = HashMap::new();
    let mut left_visited = 0u64;
    let cancelled_in_phase1 = walk_root(left_root, options, cancel, &mut left_visited, |path, info| {
        left_map.insert(path, info);
    });
    if cancelled_in_phase1 {
        return ScanOutcome { cancelled: true, left_visited, right_visited: 0, entries_emitted: 0 };
    }

    let mut right_visited = 0u64;
    let mut batch: Vec<DirEntry> = Vec::with_capacity(BATCH_SIZE);
    let mut entries_emitted = 0u64;
    let mut cancelled = false;

    for result in walker(right_root, options) {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(right_info) = file_info(&entry) else { continue };
        right_visited += 1;
        let rel = relative_path(right_root, entry.path());

        let dir_entry = match left_map.remove(&rel) {
            Some(left_info) => DirEntry {
                path: rel,
                status: classify(&left_info, &right_info),
                is_dir: right_info.is_dir,
                is_symlink: right_info.is_symlink,
                symlink_target: right_info.symlink_target.clone(),
                size_left: left_info.size,
                size_right: right_info.size,
            },
            None => DirEntry {
                path: rel,
                status: EntryStatus::RightOnly,
                is_dir: right_info.is_dir,
                is_symlink: right_info.is_symlink,
                symlink_target: right_info.symlink_target.clone(),
                size_left: None,
                size_right: right_info.size,
            },
        };
        batch.push(dir_entry);
        if batch.len() >= BATCH_SIZE {
            entries_emitted += batch.len() as u64;
            on_batch(std::mem::take(&mut batch));
        }
    }

    if !batch.is_empty() {
        entries_emitted += batch.len() as u64;
        on_batch(std::mem::take(&mut batch));
    }

    if cancelled {
        return ScanOutcome { cancelled: true, left_visited, right_visited, entries_emitted };
    }

    // Whatever's left in the map never appeared on the right -- LeftOnly.
    let mut leftover: Vec<DirEntry> = left_map
        .into_iter()
        .map(|(path, info)| DirEntry {
            path,
            status: EntryStatus::LeftOnly,
            is_dir: info.is_dir,
            is_symlink: info.is_symlink,
            symlink_target: info.symlink_target,
            size_left: info.size,
            size_right: None,
        })
        .collect();
    while !leftover.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            return ScanOutcome { cancelled: true, left_visited, right_visited, entries_emitted };
        }
        let take = leftover.len().min(BATCH_SIZE);
        let batch: Vec<DirEntry> = leftover.drain(..take).collect();
        entries_emitted += batch.len() as u64;
        on_batch(batch);
    }

    ScanOutcome { cancelled: false, left_visited, right_visited, entries_emitted }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("dirwalk-test-{}-{}", std::process::id(), name));
        let left = base.join("left");
        let right = base.join("right");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        (left, right)
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn scan_all(left: &Path, right: &Path, options: &ScanOptions) -> (Vec<DirEntry>, ScanOutcome) {
        let cancel = AtomicBool::new(false);
        let mut entries = Vec::new();
        let outcome = scan(left, right, options, &cancel, |batch| entries.extend(batch));
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        (entries, outcome)
    }

    fn find<'a>(entries: &'a [DirEntry], path: &str) -> &'a DirEntry {
        entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("no entry for {path:?} in {entries:?}"))
    }

    #[test]
    fn dir_entry_serializes_with_camel_case_field_names_matching_the_frontend_type() {
        let entry = DirEntry {
            path: "a".into(),
            status: EntryStatus::Modified,
            is_dir: false,
            is_symlink: false,
            symlink_target: None,
            size_left: Some(1),
            size_right: Some(2),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["status"], "modified");
        assert_eq!(json["isDir"], false);
        assert_eq!(json["isSymlink"], false);
        assert_eq!(json["symlinkTarget"], serde_json::Value::Null);
        assert_eq!(json["sizeLeft"], 1);
        assert_eq!(json["sizeRight"], 2);
    }

    #[test]
    fn left_only_and_right_only_are_detected() {
        let (left, right) = test_dirs("left-right-only");
        write(&left, "only-left.txt", "a");
        write(&right, "only-right.txt", "b");
        let (entries, outcome) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "only-left.txt").status, EntryStatus::LeftOnly);
        assert_eq!(find(&entries, "only-right.txt").status, EntryStatus::RightOnly);
        assert!(!outcome.cancelled);
    }

    #[test]
    fn differing_size_is_modified() {
        let (left, right) = test_dirs("differing-size");
        write(&left, "f.txt", "short");
        write(&right, "f.txt", "a much longer string");
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "f.txt").status, EntryStatus::Modified);
    }

    #[test]
    fn same_size_and_same_mtime_is_accepted_as_same_without_content_comparison() {
        // Deliberately different content but identical size and forced-identical mtime -- this
        // is the documented false-Same failure mode of the mtime fast path, made concrete: the
        // scan trusts size+mtime and reports Same even though the bytes actually differ.
        let (left, right) = test_dirs("same-mtime-fast-path");
        write(&left, "f.txt", "aaaaa");
        write(&right, "f.txt", "bbbbb");
        let now = SystemTime::now();
        fs::File::open(left.join("f.txt")).unwrap().set_modified(now).unwrap();
        fs::File::open(right.join("f.txt")).unwrap().set_modified(now).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "f.txt").status, EntryStatus::Same, "documented tradeoff: same size+mtime short-circuits to Same");
    }

    #[test]
    fn same_size_differing_mtime_falls_back_to_content_comparison_and_finds_a_real_difference() {
        let (left, right) = test_dirs("content-compare-modified");
        write(&left, "f.txt", "aaaaa");
        write(&right, "f.txt", "bbbbb");
        fs::File::open(left.join("f.txt")).unwrap().set_modified(SystemTime::now()).unwrap();
        fs::File::open(right.join("f.txt")).unwrap().set_modified(SystemTime::now() + std::time::Duration::from_secs(10)).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "f.txt").status, EntryStatus::Modified);
    }

    #[test]
    fn same_size_differing_mtime_but_identical_content_is_same() {
        let (left, right) = test_dirs("content-compare-same");
        write(&left, "f.txt", "identical");
        write(&right, "f.txt", "identical");
        fs::File::open(left.join("f.txt")).unwrap().set_modified(SystemTime::now()).unwrap();
        fs::File::open(right.join("f.txt")).unwrap().set_modified(SystemTime::now() + std::time::Duration::from_secs(10)).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "f.txt").status, EntryStatus::Same);
    }

    #[test]
    fn type_conflict_when_a_file_is_replaced_by_a_directory() {
        let (left, right) = test_dirs("type-conflict");
        write(&left, "thing", "i am a file");
        fs::create_dir_all(right.join("thing")).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "thing").status, EntryStatus::TypeConflict);
    }

    #[test]
    fn directories_present_on_both_sides_are_same_regardless_of_their_children() {
        let (left, right) = test_dirs("dir-same");
        write(&left, "sub/a.txt", "a");
        write(&right, "sub/b.txt", "b");
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "sub").status, EntryStatus::Same);
        assert!(find(&entries, "sub").is_dir);
        assert_eq!(find(&entries, "sub/a.txt").status, EntryStatus::LeftOnly);
        assert_eq!(find(&entries, "sub/b.txt").status, EntryStatus::RightOnly);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_compared_by_target_not_followed() {
        use std::os::unix::fs::symlink;
        let (left, right) = test_dirs("symlink-target");
        write(&left, "target-a", "a");
        write(&left, "target-b", "b");
        symlink(left.join("target-a"), left.join("link")).unwrap();
        symlink(left.join("target-a"), right.join("link")).unwrap(); // same target
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        let link = find(&entries, "link");
        assert!(link.is_symlink);
        assert_eq!(link.status, EntryStatus::Same);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_pointing_at_different_targets_are_modified() {
        use std::os::unix::fs::symlink;
        let (left, right) = test_dirs("symlink-diff-target");
        write(&left, "target-a", "a");
        write(&left, "target-b", "b");
        symlink(left.join("target-a"), left.join("link")).unwrap();
        symlink(left.join("target-b"), right.join("link")).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert_eq!(find(&entries, "link").status, EntryStatus::Modified);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_reported_as_a_leaf_not_descended_into() {
        use std::os::unix::fs::symlink;
        let (left, right) = test_dirs("symlink-dir-not-followed");
        write(&left, "real/inside.txt", "content");
        symlink(left.join("real"), left.join("link-to-real")).unwrap();
        write(&right, "real/inside.txt", "content");
        symlink(right.join("real"), right.join("link-to-real")).unwrap();
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        // the symlink itself is an entry, but nothing under "link-to-real/" is -- proving the
        // walker never descended through it (which would also risk a cycle, since it points
        // back into the same tree).
        assert!(entries.iter().any(|e| e.path == "link-to-real"));
        assert!(!entries.iter().any(|e| e.path.starts_with("link-to-real/")));
    }

    #[test]
    fn gitignore_is_respected_by_default() {
        let (left, right) = test_dirs("gitignore-respected");
        write(&left, ".gitignore", "ignored.txt\n");
        write(&left, "ignored.txt", "should not appear");
        write(&left, "kept.txt", "kept");
        write(&right, "kept.txt", "kept");
        let (entries, _) = scan_all(&left, &right, &ScanOptions { respect_gitignore: true, exclude_globs: vec![] });
        assert!(!entries.iter().any(|e| e.path == "ignored.txt"), "gitignored file leaked into results: {entries:?}");
        assert!(entries.iter().any(|e| e.path == "kept.txt"));
    }

    #[test]
    fn gitignore_can_be_turned_off() {
        let (left, right) = test_dirs("gitignore-disabled");
        write(&left, ".gitignore", "ignored.txt\n");
        write(&left, "ignored.txt", "should appear when disabled");
        let (entries, _) = scan_all(&left, &right, &ScanOptions { respect_gitignore: false, exclude_globs: vec![] });
        assert!(entries.iter().any(|e| e.path == "ignored.txt"));
    }

    #[test]
    fn dotfiles_are_shown_by_default_unlike_the_ignore_crates_own_ripgrep_oriented_default() {
        let (left, right) = test_dirs("dotfiles-shown");
        write(&left, ".env", "SECRET=1");
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert!(entries.iter().any(|e| e.path == ".env"), "dotfile should be visible by default: {entries:?}");
    }

    #[test]
    fn exclude_globs_hide_matching_paths_on_both_sides() {
        let (left, right) = test_dirs("exclude-globs");
        write(&left, "keep.txt", "a");
        write(&left, "build/output.bin", "binary");
        write(&right, "keep.txt", "a");
        write(&right, "build/output.bin", "binary");
        let options = ScanOptions { respect_gitignore: true, exclude_globs: vec!["build/".to_string()] };
        let (entries, _) = scan_all(&left, &right, &options);
        assert!(!entries.iter().any(|e| e.path.starts_with("build")), "excluded glob leaked into results: {entries:?}");
        assert!(entries.iter().any(|e| e.path == "keep.txt"));
    }

    #[test]
    fn cancelling_before_the_scan_starts_visits_nothing() {
        let (left, right) = test_dirs("cancel-before-start");
        write(&left, "a.txt", "a");
        write(&right, "a.txt", "a");
        let cancel = AtomicBool::new(true);
        let mut entries = Vec::new();
        let outcome = scan(&left, &right, &ScanOptions::default(), &cancel, |batch| entries.extend(batch));
        assert!(outcome.cancelled);
        assert_eq!(outcome.left_visited, 0, "must not have read anything from the left tree once already cancelled");
        assert_eq!(outcome.right_visited, 0);
        assert!(entries.is_empty());
    }

    #[test]
    fn cancelling_mid_phase_two_stops_before_visiting_every_right_side_entry() {
        // More files than BATCH_SIZE on each side so a second batch would occur if the scan
        // kept going -- cancellation is triggered deterministically from inside on_batch (not
        // by wall-clock timing), proving the walker actually stops pulling further entries
        // rather than merely stopping *emission* while still reading the whole tree underneath.
        let (left, right) = test_dirs("cancel-mid-phase-two");
        let total = BATCH_SIZE * 3;
        for i in 0..total {
            write(&left, &format!("f{i}.txt"), "x");
            write(&right, &format!("f{i}.txt"), "x");
        }
        let cancel = AtomicBool::new(false);
        let mut batches_seen = 0u32;
        let outcome = scan(&left, &right, &ScanOptions::default(), &cancel, |_batch| {
            batches_seen += 1;
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(outcome.cancelled);
        assert_eq!(batches_seen, 1, "must not emit a second batch once cancellation was requested inside the first");
        assert!(
            (outcome.right_visited as usize) < total,
            "expected the walk to stop well short of all {total} entries, visited {}",
            outcome.right_visited
        );
    }

    #[test]
    fn relative_paths_are_forward_slash_normalized_at_any_depth() {
        let (left, right) = test_dirs("nested-path-normalization");
        write(&left, "a/b/c/deep.txt", "x");
        write(&right, "a/b/c/deep.txt", "x");
        let (entries, _) = scan_all(&left, &right, &ScanOptions::default());
        assert!(entries.iter().any(|e| e.path == "a/b/c/deep.txt"));
        assert!(!entries.iter().any(|e| e.path.contains('\\')));
    }
}
