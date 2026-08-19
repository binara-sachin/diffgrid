use diff_core::{diff_lines, HunkKind, LineRange};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MergeHunkKind {
    AutoMerged,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    TakeLocal,
    TakeRemote,
    TakeBoth,
    TakeBase,
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeHunk {
    pub kind: MergeHunkKind,
    pub base: LineRange,
    pub local: LineRange,
    pub remote: LineRange,
    pub resolution: Option<Resolution>,
}

#[derive(Clone, Copy)]
enum SideTag {
    Local,
    Remote,
}

struct Tagged {
    side: SideTag,
    base: LineRange,
}

/// A change hunk's base range, extended by one when the range is a bare insertion (zero-length),
/// so an insert still registers as touching the single base position it's anchored at instead of
/// vanishing from every overlap test. This does NOT make adjacent hunks (one ending exactly where
/// the next starts) touch each other -- `overlaps` below uses strict `<`, so hunks at base
/// positions [1,2) and [2,3) are adjacent but disjoint, and correctly auto-merge as two separate
/// hunks rather than clustering (see the "adjacent...auto_merge_as_two_hunks" test).
fn effective_end(r: LineRange) -> u32 {
    r.start + r.len.max(1)
}

fn overlaps(a: LineRange, b: LineRange) -> bool {
    a.start < effective_end(b) && b.start < effective_end(a)
}

/// Maps a base-line position to the corresponding position on the other side of `hunks` (a
/// `diff_lines(base, other)` result). `hunks` partitions base-line space contiguously (by
/// `diff_core::diff_lines`'s own construction: every `Equal` gap plus every change hunk together
/// cover `[0, total_left)` with no gaps), so a linear scan finds the containing hunk and offsets
/// into its `right` range. A position exactly at a hunk's end (used for range-end mapping) falls
/// through to the next hunk's start, which is exactly the half-open-interval behavior wanted.
fn map_base_pos(hunks: &[diff_core::Hunk], pos: u32) -> u32 {
    for h in hunks {
        let base_end = h.left.start + h.left.len;
        if pos < base_end || (pos == h.left.start && h.left.len == 0) {
            return h.right.start + pos.saturating_sub(h.left.start);
        }
    }
    hunks.last().map(|h| h.right.start + h.right.len).unwrap_or(pos)
}

fn map_base_range(hunks: &[diff_core::Hunk], base: LineRange) -> LineRange {
    let start = map_base_pos(hunks, base.start);
    let end = map_base_pos(hunks, base.start + base.len);
    LineRange { start, len: end.saturating_sub(start) }
}

fn extract_lines(text: &str, range: LineRange) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let end = (range.start + range.len).min(lines.len() as u32);
    let start = range.start.min(lines.len() as u32);
    lines[start as usize..end as usize].join("\n")
}

/// Diffs base->local and base->remote (reusing `diff_core::diff_lines`, the same histogram
/// algorithm M1-M4 already use), then clusters change hunks from both sides that touch
/// overlapping base-line regions into one `MergeHunk` -- a change on only one side auto-merges to
/// that side; changes on both sides that resolve to identical text auto-merge (no real
/// disagreement); changes on both sides with different text conflict.
pub fn compute_merge_hunks(base: &str, local: &str, remote: &str) -> Vec<MergeHunk> {
    let local_diff = diff_lines(base, local);
    let remote_diff = diff_lines(base, remote);

    let mut tagged: Vec<Tagged> = local_diff
        .hunks
        .iter()
        .filter(|h| h.kind != HunkKind::Equal)
        .map(|h| Tagged { side: SideTag::Local, base: h.left })
        .chain(remote_diff.hunks.iter().filter(|h| h.kind != HunkKind::Equal).map(|h| Tagged { side: SideTag::Remote, base: h.left }))
        .collect();
    tagged.sort_by_key(|t| t.base.start);

    let mut merge_hunks = Vec::new();
    let mut i = 0;
    while i < tagged.len() {
        let mut cluster_base = tagged[i].base;
        let mut has_local = matches!(tagged[i].side, SideTag::Local);
        let mut has_remote = matches!(tagged[i].side, SideTag::Remote);
        let mut j = i + 1;
        while j < tagged.len() && overlaps(tagged[j].base, cluster_base) {
            let jb = tagged[j].base;
            let new_end = effective_end(cluster_base).max(effective_end(jb));
            cluster_base = LineRange { start: cluster_base.start, len: new_end - cluster_base.start };
            match tagged[j].side {
                SideTag::Local => has_local = true,
                SideTag::Remote => has_remote = true,
            }
            j += 1;
        }

        let local_range = map_base_range(&local_diff.hunks, cluster_base);
        let remote_range = map_base_range(&remote_diff.hunks, cluster_base);

        let (kind, resolution) = if has_local && has_remote {
            if extract_lines(local, local_range) == extract_lines(remote, remote_range) {
                (MergeHunkKind::AutoMerged, Some(Resolution::TakeLocal))
            } else {
                (MergeHunkKind::Conflict, None)
            }
        } else if has_local {
            (MergeHunkKind::AutoMerged, Some(Resolution::TakeLocal))
        } else {
            (MergeHunkKind::AutoMerged, Some(Resolution::TakeRemote))
        };

        merge_hunks.push(MergeHunk { kind, base: cluster_base, local: local_range, remote: remote_range, resolution });
        i = j;
    }

    merge_hunks
}

/// Which side's content comes first when a hunk resolves to `Resolution::TakeBoth` -- the
/// merge-core equivalent of `session::TakeBothSide` (docs/UI/ui-02.png's "Default side when
/// taking both"). Kept as its own type here rather than depending on the `session` crate: no
/// other crate in this workspace depends on `session`, and this crate's own dependency (only
/// `diff-core`) should stay that way -- the `app` crate is what translates `session::TakeBothSide`
/// into this at the call site, the same pattern it already uses for `session::IntraLineMode` ->
/// `diff_core`'s word/character mode functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeBothOrder {
    LocalFirst,
    RemoteFirst,
}

/// Builds the *initial* merged output text by walking `merge_hunks` in base order, taking the
/// untouched base content between hunks verbatim and, for each hunk, the content dictated by its
/// `resolution` (an unresolved `Conflict` hunk -- `resolution: None` -- takes the base's own
/// content for that region as a placeholder; the frontend is responsible for surfacing that it's
/// still unresolved before write-back, per docs/PLAN.md's write-back-with-correct-exit-status
/// requirement). `merge_hunks` must be the exact list `compute_merge_hunks` returned for this
/// same `(base, local, remote)` triple -- the `local`/`remote`/`base` LineRanges only make sense
/// against those specific texts.
///
/// This seeds the merged-pane CM6 buffer once, at merge-view open (same role `open_file_pair`
/// plays for M1/M2's two-pane view) -- it is NOT re-invoked on every resolution change. Once the
/// merged pane exists, CM6's document is authoritative for it (same principle as M2's `EditBuffer`
/// for a two-way diff): the frontend edits the merged text directly for a given hunk (marking its
/// `resolution` as `Manual`) or replaces just that hunk's range in the live buffer for a
/// TakeLocal/TakeRemote/TakeBoth/TakeBase click, rather than re-deriving the whole document from
/// scratch. Calling this function again after any hunk has gone `Manual` is therefore a caller
/// bug, not a supported "recompute" path -- see the `Manual` match arm's panic below.
/// Resolves one hunk to the text `resolution` dictates -- the single source of truth both
/// `build_merged_text`'s initial seed and a later resolution-change command use, so the two paths
/// can never compute a hunk's content two different ways. Panics on `Some(Resolution::Manual)`
/// or `None` for a `Conflict` hunk -- see `build_merged_text`'s doc comment for why: neither has
/// text derivable from `base`/`local`/`remote` `LineRange`s alone, and a caller reaching this
/// path is a bug (an unresolved conflict must be surfaced to the user, never silently resolved to
/// something), not a case to paper over.
pub fn resolve_hunk_text(base: &str, local: &str, remote: &str, hunk: &MergeHunk, take_both_order: TakeBothOrder) -> String {
    match hunk.resolution {
        Some(Resolution::TakeLocal) => extract_lines(local, hunk.local),
        Some(Resolution::TakeRemote) => extract_lines(remote, hunk.remote),
        Some(Resolution::TakeBoth) => {
            let local_text = extract_lines(local, hunk.local);
            let remote_text = extract_lines(remote, hunk.remote);
            let (first, second) = match take_both_order {
                TakeBothOrder::LocalFirst => (local_text, remote_text),
                TakeBothOrder::RemoteFirst => (remote_text, local_text),
            };
            if first.is_empty() { second } else if second.is_empty() { first } else { format!("{first}\n{second}") }
        }
        Some(Resolution::TakeBase) => extract_lines(base, hunk.base),
        Some(Resolution::Manual) => panic!(
            "Manual resolution has no derivable text from base/local/remote LineRanges -- \
             the merged-pane CM6 buffer is authoritative once a hunk is Manual, so \
             resolve_hunk_text must not be called again for it"
        ),
        None => panic!(
            "an unresolved Conflict hunk has no text to resolve to -- the caller must surface \
             this to the user rather than reaching resolve_hunk_text for it"
        ),
    }
}

/// Builds the *initial* merged output text by walking `merge_hunks` in base order, taking the
/// untouched base content between hunks verbatim and, for each hunk, `resolve_hunk_text`'s
/// result -- except an unresolved `Conflict` hunk (`resolution: None`), which takes the base's
/// own content for that region as a placeholder here specifically (this is the one caller allowed
/// to treat `None` as "not yet decided, show something" rather than a bug, since it's building a
/// first-paint document that must render *something* for every hunk); the frontend is responsible
/// for surfacing that it's still unresolved before write-back, per docs/PLAN.md's
/// write-back-with-correct-exit-status requirement. `merge_hunks` must be the exact list
/// `compute_merge_hunks` returned for this same `(base, local, remote)` triple -- the
/// `local`/`remote`/`base` LineRanges only make sense against those specific texts.
///
/// This seeds the merged-pane CM6 buffer once, at merge-view open (same role `open_file_pair`
/// plays for M1/M2's two-pane view) -- it is NOT re-invoked on every resolution change. Once the
/// merged pane exists, CM6's document is authoritative for it (same principle as M2's `EditBuffer`
/// for a two-way diff): a resolution click computes just that hunk's new text (via
/// `resolve_hunk_text`) and the frontend dispatches a normal CM6 transaction replacing that
/// hunk's *current* character range in the live document -- the same mechanism M2's
/// `buildHunkCopyChange` already uses for copying a hunk between the two-pane view's panes, and
/// the same reason neither `MergeHunk` nor this crate tracks a hunk's position within the merged
/// document itself: CM6's own `ChangeSet`/`RangeSet` position-mapping already keeps every other
/// hunk's boundaries correct through an edit, for free, and duplicating that in Rust would be
/// exactly the "shadow state that can drift" bug class this project has already hit once (see
/// DECISIONS.md's M0 alignment-mapping revert). Calling `build_merged_text` again after any hunk
/// has gone `Manual` is therefore a caller bug, not a supported "recompute" path.
pub fn build_merged_text(base: &str, local: &str, remote: &str, merge_hunks: &[MergeHunk], take_both_order: TakeBothOrder) -> String {
    let base_lines: Vec<&str> = base.split('\n').collect();
    let mut out_lines: Vec<String> = Vec::new();
    let mut base_cursor = 0u32;

    for h in merge_hunks {
        if h.base.start > base_cursor {
            out_lines.extend(base_lines[base_cursor as usize..h.base.start as usize].iter().map(|s| s.to_string()));
        }
        let resolved = if h.resolution.is_none() { extract_lines(base, h.base) } else { resolve_hunk_text(base, local, remote, h, take_both_order) };
        if !resolved.is_empty() {
            out_lines.extend(resolved.split('\n').map(|s| s.to_string()));
        }
        base_cursor = h.base.start + h.base.len;
    }
    if (base_cursor as usize) < base_lines.len() {
        out_lines.extend(base_lines[base_cursor as usize..].iter().map(|s| s.to_string()));
    }

    out_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_local_and_remote_produce_no_merge_hunks() {
        let base = "a\nb\nc\n";
        let hunks = compute_merge_hunks(base, base, base);
        assert_eq!(hunks.len(), 0);
    }

    #[test]
    fn a_change_on_local_only_auto_merges_as_one_hunk_resolved_to_local() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let hunks = compute_merge_hunks(base, local, base);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::AutoMerged);
        assert_eq!(hunks[0].resolution, Some(Resolution::TakeLocal));
    }

    #[test]
    fn a_change_on_remote_only_auto_merges_as_one_hunk_resolved_to_remote() {
        let base = "a\nb\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let hunks = compute_merge_hunks(base, base, remote);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::AutoMerged);
        assert_eq!(hunks[0].resolution, Some(Resolution::TakeRemote));
    }

    #[test]
    fn disjoint_changes_on_both_sides_produce_two_separate_auto_merged_hunks() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let local = "1\nLOCAL\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let remote = "1\n2\n3\n4\n5\n6\n7\n8\nREMOTE\n10\n";
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks.len(), 2, "{hunks:?}");
        assert!(hunks.iter().all(|h| h.kind == MergeHunkKind::AutoMerged), "{hunks:?}");
        let one_local = hunks.iter().any(|h| h.resolution == Some(Resolution::TakeLocal));
        let one_remote = hunks.iter().any(|h| h.resolution == Some(Resolution::TakeRemote));
        assert!(one_local && one_remote, "{hunks:?}");
    }

    #[test]
    fn identical_changes_on_the_same_base_lines_merge_cleanly_as_one_hunk_not_a_conflict() {
        let base = "a\nb\nc\n";
        let local = "a\nX\nc\n";
        let remote = "a\nX\nc\n";
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::AutoMerged, "{hunks:?}");
    }

    #[test]
    fn different_changes_on_the_same_base_line_conflict() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::Conflict, "{hunks:?}");
        assert_eq!(hunks[0].resolution, None);
    }

    #[test]
    fn adjacent_but_not_overlapping_changes_on_consecutive_base_lines_auto_merge_as_two_hunks() {
        // Regression guard for the off-by-one effective_end/overlaps must get right: local
        // touches base line 2 (0-indexed 1), remote touches base line 3 (0-indexed 2) --
        // consecutive lines, but genuinely disjoint single-line edits with no real disagreement
        // about either line's content. This must NOT false-conflict.
        let base = "a\nb\nc\nd\n";
        let local = "a\nLOCAL\nc\nd\n";
        let remote = "a\nb\nREMOTE\nd\n";
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks.len(), 2, "{hunks:?}");
        assert!(hunks.iter().all(|h| h.kind == MergeHunkKind::AutoMerged), "{hunks:?}");
    }

    #[test]
    fn a_change_touching_the_very_last_base_line_maps_correctly() {
        // map_base_pos's "fell off the end of the hunks list" branch is only exercised when a
        // change reaches the final base line with no trailing Equal hunk after it -- this is the
        // regression guard for that specific path.
        let base = "a\nb\nc\n";
        let local = "a\nb\nLAST\n";
        let hunks = compute_merge_hunks(base, local, base);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::AutoMerged);
        assert_eq!(hunks[0].local, LineRange { start: 2, len: 1 }, "{hunks:?}");
    }

    #[test]
    fn a_pure_deletion_on_local_overlapping_a_replace_on_remote_conflicts() {
        let base = "a\nb\nc\nd\n";
        let local = "a\nd\n"; // deletes b and c
        let remote = "a\nb\nX\nd\n"; // replaces c with X
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks.len(), 1, "{hunks:?}");
        assert_eq!(hunks[0].kind, MergeHunkKind::Conflict, "{hunks:?}");
    }

    #[test]
    fn build_merged_text_with_no_hunks_returns_base_unchanged() {
        let base = "a\nb\nc\n";
        let hunks = compute_merge_hunks(base, base, base);
        let merged = build_merged_text(base, base, base, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, base);
    }

    #[test]
    fn build_merged_text_applies_an_auto_merged_local_only_change() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let hunks = compute_merge_hunks(base, local, base);
        let merged = build_merged_text(base, local, base, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, "a\nLOCAL\nc\n");
    }

    #[test]
    fn build_merged_text_applies_disjoint_changes_from_both_sides_together() {
        let base = "1\n2\n3\n4\n5\n";
        let local = "1\nLOCAL\n3\n4\n5\n";
        let remote = "1\n2\n3\n4\nREMOTE\n";
        let hunks = compute_merge_hunks(base, local, remote);
        let merged = build_merged_text(base, local, remote, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, "1\nLOCAL\n3\n4\nREMOTE\n");
    }

    #[test]
    fn build_merged_text_respects_an_explicit_conflict_resolution() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let mut hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks[0].kind, MergeHunkKind::Conflict);
        hunks[0].resolution = Some(Resolution::TakeRemote);
        let merged = build_merged_text(base, local, remote, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, "a\nREMOTE\nc\n");
    }

    #[test]
    fn build_merged_text_take_both_concatenates_local_then_remote() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let mut hunks = compute_merge_hunks(base, local, remote);
        hunks[0].resolution = Some(Resolution::TakeBoth);
        let merged = build_merged_text(base, local, remote, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, "a\nLOCAL\nREMOTE\nc\n");
    }

    #[test]
    fn build_merged_text_take_both_respects_remote_first_order() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let mut hunks = compute_merge_hunks(base, local, remote);
        hunks[0].resolution = Some(Resolution::TakeBoth);
        let merged = build_merged_text(base, local, remote, &hunks, TakeBothOrder::RemoteFirst);
        assert_eq!(merged, "a\nREMOTE\nLOCAL\nc\n");
    }

    #[test]
    #[should_panic(expected = "Manual")]
    fn build_merged_text_panics_on_a_manual_resolution() {
        // Manual means the user has directly edited that hunk's content in the merged-pane CM6
        // buffer, which is authoritative from that point on (same pattern as M2's EditBuffer) --
        // there is no derivable text for this hunk from base/local/remote LineRanges alone, so
        // calling build_merged_text again after a Manual resolution is a caller bug, not a case
        // to silently paper over with base content.
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let mut hunks = compute_merge_hunks(base, local, remote);
        hunks[0].resolution = Some(Resolution::Manual);
        build_merged_text(base, local, remote, &hunks, TakeBothOrder::LocalFirst);
    }

    #[test]
    fn resolve_hunk_text_returns_just_that_hunks_content_for_take_local() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let hunks = compute_merge_hunks(base, local, remote);
        let mut hunk = hunks[0].clone();
        hunk.resolution = Some(Resolution::TakeLocal);
        assert_eq!(resolve_hunk_text(base, local, remote, &hunk, TakeBothOrder::LocalFirst), "LOCAL");
    }

    #[test]
    #[should_panic(expected = "unresolved Conflict")]
    fn resolve_hunk_text_panics_on_an_unresolved_conflict() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let hunks = compute_merge_hunks(base, local, remote);
        assert_eq!(hunks[0].resolution, None);
        resolve_hunk_text(base, local, remote, &hunks[0], TakeBothOrder::LocalFirst);
    }

    #[test]
    fn build_merged_text_take_base_restores_the_original_content() {
        let base = "a\nb\nc\n";
        let local = "a\nLOCAL\nc\n";
        let remote = "a\nREMOTE\nc\n";
        let mut hunks = compute_merge_hunks(base, local, remote);
        hunks[0].resolution = Some(Resolution::TakeBase);
        let merged = build_merged_text(base, local, remote, &hunks, TakeBothOrder::LocalFirst);
        assert_eq!(merged, base);
    }
}
