use imara_diff::{Algorithm, Diff, InternedInput};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HunkKind {
    Equal,
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineRange {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub kind: HunkKind,
    pub left: LineRange,
    pub right: LineRange,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
    pub chunks: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiffResult {
    pub hunks: Vec<Hunk>,
    pub stats: DiffStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub side: Side,
    pub start_utf16: u32,
    pub len_utf16: u32,
}

/// Character-level diff for one `Replace`-hunk line pair, returning the changed span(s) after
/// trimming the common prefix and suffix. Offsets are UTF-16 code units (not bytes, not Rust
/// `char`s) per docs/PLAN.md §3, since that's the unit CM6 uses for all of its own position
/// arithmetic on the frontend — this must be computed here, not assumed equal to byte length,
/// or every intra-line highlight touching a non-ASCII character misaligns.
///
/// This is prefix/suffix trimming, not a full LCS/Myers character diff: O(n), with no risk of
/// the quadratic blowup a DP-based algorithm would have on a pathologically long single line.
/// The cost is not finding a minimal edit script when a line has two separate edits far apart —
/// they'll span the whole region between them as one "changed" run rather than two small ones.
/// Acceptable for M1 (see DECISIONS.md); revisit only if this visibly produces unhelpfully large
/// spans in practice.
///
/// Deliberately lazy per docs/PLAN.md §6: this is not called for every `Replace` hunk in a file
/// eagerly, only per-line as the frontend's viewport requests it, since computing intra-line
/// spans for every replaced line in a 100k-line file on diff completion is exactly the kind of
/// eager work the M0 profiling pass spent an entire investigation getting rid of elsewhere.
pub fn intra_line_spans(left: &str, right: &str) -> Vec<Span> {
    let l: Vec<u16> = left.encode_utf16().collect();
    let r: Vec<u16> = right.encode_utf16().collect();

    let mut start = 0;
    while start < l.len() && start < r.len() && l[start] == r[start] {
        start += 1;
    }

    let mut end_l = l.len();
    let mut end_r = r.len();
    while end_l > start && end_r > start && l[end_l - 1] == r[end_r - 1] {
        end_l -= 1;
        end_r -= 1;
    }

    let mut spans = Vec::new();
    if end_l > start {
        spans.push(Span { side: Side::Left, start_utf16: start as u32, len_utf16: (end_l - start) as u32 });
    }
    if end_r > start {
        spans.push(Span { side: Side::Right, start_utf16: start as u32, len_utf16: (end_r - start) as u32 });
    }
    spans
}

/// Line-level diff via the histogram algorithm. Intra-line spans are deliberately
/// not computed here — per docs/PLAN.md they are viewport-driven, requested lazily
/// by the frontend for visible Replace hunks only.
pub fn diff_lines(left: &str, right: &str) -> FileDiffResult {
    let input = InternedInput::new(left, right);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let mut hunks = Vec::new();
    let mut stats = DiffStats::default();
    let mut left_cursor = 0u32;
    let mut right_cursor = 0u32;

    for h in diff.hunks() {
        let left_gap = h.before.start - left_cursor;
        let right_gap = h.after.start - right_cursor;
        if left_gap > 0 || right_gap > 0 {
            hunks.push(Hunk {
                kind: HunkKind::Equal,
                left: LineRange { start: left_cursor, len: left_gap },
                right: LineRange { start: right_cursor, len: right_gap },
            });
        }

        let left_len = h.before.end - h.before.start;
        let right_len = h.after.end - h.after.start;
        let kind = match (left_len, right_len) {
            (0, _) => HunkKind::Insert,
            (_, 0) => HunkKind::Delete,
            _ => HunkKind::Replace,
        };
        stats.removed += left_len;
        stats.added += right_len;
        stats.chunks += 1;
        hunks.push(Hunk {
            kind,
            left: LineRange { start: h.before.start, len: left_len },
            right: LineRange { start: h.after.start, len: right_len },
        });

        left_cursor = h.before.end;
        right_cursor = h.after.end;
    }

    let total_left = input.before.len() as u32;
    let total_right = input.after.len() as u32;
    if left_cursor < total_left || right_cursor < total_right {
        hunks.push(Hunk {
            kind: HunkKind::Equal,
            left: LineRange { start: left_cursor, len: total_left - left_cursor },
            right: LineRange { start: right_cursor, len: total_right - right_cursor },
        });
    }

    FileDiffResult { hunks, stats }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_input_is_one_equal_hunk() {
        let result = diff_lines("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.hunks[0].kind, HunkKind::Equal);
        assert_eq!(result.stats.chunks, 0);
    }

    #[test]
    fn pure_insert_is_detected() {
        let result = diff_lines("a\nc\n", "a\nb\nc\n");
        let inserts: Vec<_> = result.hunks.iter().filter(|h| h.kind == HunkKind::Insert).collect();
        assert_eq!(inserts.len(), 1);
        assert_eq!(inserts[0].right.len, 1);
        assert_eq!(result.stats.added, 1);
        assert_eq!(result.stats.removed, 0);
    }

    #[test]
    fn pure_delete_is_detected() {
        let result = diff_lines("a\nb\nc\n", "a\nc\n");
        let deletes: Vec<_> = result.hunks.iter().filter(|h| h.kind == HunkKind::Delete).collect();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].left.len, 1);
        assert_eq!(result.stats.removed, 1);
    }

    #[test]
    fn replace_is_detected() {
        let result = diff_lines("a\nb\nc\n", "a\nx\nc\n");
        let replaces: Vec<_> = result.hunks.iter().filter(|h| h.kind == HunkKind::Replace).collect();
        assert_eq!(replaces.len(), 1);
        assert_eq!(replaces[0].left.len, 1);
        assert_eq!(replaces[0].right.len, 1);
    }

    #[test]
    fn hunks_cover_the_whole_file_contiguously() {
        let result = diff_lines("a\nb\nc\nd\ne\n", "a\nx\nc\nd\ny\n");
        let mut left_pos = 0u32;
        let mut right_pos = 0u32;
        for h in &result.hunks {
            assert_eq!(h.left.start, left_pos);
            assert_eq!(h.right.start, right_pos);
            left_pos += h.left.len;
            right_pos += h.right.len;
        }
        assert_eq!(left_pos, 5);
        assert_eq!(right_pos, 5);
    }

    #[test]
    fn both_empty_produces_no_hunks() {
        let result = diff_lines("", "");
        assert!(result.hunks.is_empty());
        assert_eq!(result.stats.chunks, 0);
    }

    #[test]
    fn empty_left_is_pure_insert_of_everything() {
        let result = diff_lines("", "a\nb\nc\n");
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.hunks[0].kind, HunkKind::Insert);
        assert_eq!(result.hunks[0].left.len, 0);
        assert_eq!(result.hunks[0].right.len, 3);
    }

    #[test]
    fn empty_right_is_pure_delete_of_everything() {
        let result = diff_lines("a\nb\nc\n", "");
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.hunks[0].kind, HunkKind::Delete);
        assert_eq!(result.hunks[0].left.len, 3);
        assert_eq!(result.hunks[0].right.len, 0);
    }

    /// Characterizes a real gap for M1's text-io layer to handle, not a diff-core bug:
    /// imara-diff's line tokenizer treats a final line's presence/absence of a trailing
    /// newline as part of the token itself, so "c" and "c\n" are different tokens even
    /// though nothing a user would call "the content" changed. Left uncorrected here
    /// deliberately — normalizing this is a text-io/encoding-layer concern per
    /// docs/PLAN.md, not diff-core's — but must not be silently undiscovered.
    #[test]
    fn missing_trailing_newline_is_seen_as_a_changed_last_line() {
        let result = diff_lines("a\nb\nc", "a\nb\nc\n");
        let non_equal: Vec<_> = result.hunks.iter().filter(|h| h.kind != HunkKind::Equal).collect();
        assert_eq!(non_equal.len(), 1, "expected exactly the last line to show as changed");
        assert_eq!(non_equal[0].kind, HunkKind::Replace);
        assert_eq!(non_equal[0].left.len, 1);
        assert_eq!(non_equal[0].right.len, 1);
    }

    #[test]
    fn identical_missing_trailing_newline_on_both_sides_is_unaffected() {
        let result = diff_lines("a\nb\nc", "a\nb\nc");
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.hunks[0].kind, HunkKind::Equal);
        assert_eq!(result.stats.chunks, 0);
    }

    #[test]
    fn very_long_single_line_replace_does_not_panic_or_misclassify() {
        let long_a = "x".repeat(100_000);
        let long_b = format!("{}{}", "x".repeat(50_000), "y".repeat(50_000));
        let result = diff_lines(&format!("a\n{long_a}\nc\n"), &format!("a\n{long_b}\nc\n"));
        let replaces: Vec<_> = result.hunks.iter().filter(|h| h.kind == HunkKind::Replace).collect();
        assert_eq!(replaces.len(), 1);
        assert_eq!(replaces[0].left.len, 1);
        assert_eq!(replaces[0].right.len, 1);
    }

    #[test]
    fn large_synthetic_file_hunks_remain_contiguous_and_gapless() {
        // Regression coverage at a scale closer to the fixtures used for benchmarking,
        // without depending on the (gitignored, generated) fixture files existing on disk.
        let mut left = String::new();
        let mut right = String::new();
        for i in 0..2000 {
            left.push_str(&format!("line_{i}\n"));
            if i % 37 == 0 {
                right.push_str(&format!("changed_{i}\n"));
            } else {
                right.push_str(&format!("line_{i}\n"));
            }
        }

        let result = diff_lines(&left, &right);
        let mut left_pos = 0u32;
        let mut right_pos = 0u32;
        for h in &result.hunks {
            assert_eq!(h.left.start, left_pos, "gap or overlap in left ranges");
            assert_eq!(h.right.start, right_pos, "gap or overlap in right ranges");
            left_pos += h.left.len;
            right_pos += h.right.len;
        }
        assert_eq!(left_pos, 2000);
        assert_eq!(right_pos, 2000);
        assert!(result.stats.chunks > 0);
    }

    #[test]
    fn identical_lines_produce_no_intra_line_spans() {
        assert_eq!(intra_line_spans("same line", "same line"), vec![]);
    }

    #[test]
    fn fully_disjoint_lines_span_the_whole_line_on_both_sides() {
        let spans = intra_line_spans("aaa", "bbb");
        assert_eq!(
            spans,
            vec![
                Span { side: Side::Left, start_utf16: 0, len_utf16: 3 },
                Span { side: Side::Right, start_utf16: 0, len_utf16: 3 },
            ]
        );
    }

    #[test]
    fn common_prefix_and_suffix_are_trimmed_leaving_one_span_per_side() {
        // digits vs letters in the middle share no characters, so the trim is unambiguous
        let spans = intra_line_spans("prefix-123-suffix", "prefix-abc-suffix");
        let prefix_len = "prefix-".encode_utf16().count() as u32;
        assert_eq!(
            spans,
            vec![
                Span { side: Side::Left, start_utf16: prefix_len, len_utf16: 3 },
                Span { side: Side::Right, start_utf16: prefix_len, len_utf16: 3 },
            ]
        );
    }

    #[test]
    fn one_side_being_a_prefix_of_the_other_only_spans_the_longer_side() {
        let spans = intra_line_spans("abc", "abcdef");
        assert_eq!(spans, vec![Span { side: Side::Right, start_utf16: 3, len_utf16: 3 }]);
    }

    #[test]
    fn empty_vs_nonempty_line_spans_only_the_nonempty_side() {
        let spans = intra_line_spans("", "new content");
        assert_eq!(spans, vec![Span { side: Side::Right, start_utf16: 0, len_utf16: 11 }]);
    }

    /// Regression test for a real bug: `Span` originally lacked `#[serde(rename_all =
    /// "camelCase")]`, so it serialized as `start_utf16`/`len_utf16` while the frontend's `Span`
    /// TS interface expected `startUtf16`/`lenUtf16`. The mismatch didn't fail to compile or
    /// error at the IPC boundary — it silently produced `undefined` on the frontend, which
    /// arithmetic then turned into `NaN`, which crashed deep inside a CM6 RangeSetBuilder call
    /// with an error message that named none of the real cause. Caught only by end-to-end
    /// visual verification under Xvfb, not by any of the unit tests above. This test pins the
    /// wire format directly so a future field addition can't reintroduce the same class of bug.
    #[test]
    fn span_serializes_with_camel_case_field_names_matching_the_frontend_type() {
        let span = Span { side: Side::Left, start_utf16: 3, len_utf16: 5 };
        let json = serde_json::to_value(&span).unwrap();
        assert_eq!(json["side"], "left");
        assert_eq!(json["startUtf16"], 3);
        assert_eq!(json["lenUtf16"], 5);
    }

    #[test]
    fn offsets_are_counted_in_utf16_code_units_not_chars_or_bytes() {
        // an astral emoji is 1 char, 4 bytes, but 2 UTF-16 code units -- the offset of the
        // change after it must reflect that, or every highlight past an emoji misaligns in CM6.
        let spans = intra_line_spans("a😀b", "a😀c");
        let prefix_units = "a😀".encode_utf16().count() as u32;
        assert_eq!(prefix_units, 3); // 'a' (1) + emoji surrogate pair (2)
        assert_eq!(
            spans,
            vec![
                Span { side: Side::Left, start_utf16: prefix_units, len_utf16: 1 },
                Span { side: Side::Right, start_utf16: prefix_units, len_utf16: 1 },
            ]
        );
    }
}
