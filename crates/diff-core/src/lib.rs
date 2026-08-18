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

fn is_ascii_ws_unit(u: u16) -> bool {
    matches!(u, 0x20 | 0x09 | 0x0a | 0x0d | 0x0c | 0x0b)
}

fn eq_unit(a: u16, b: u16, ignore_case: bool) -> bool {
    if a == b {
        return true;
    }
    if !ignore_case {
        return false;
    }
    // ASCII-only case fold at the UTF-16-unit level -- matches the overwhelming majority of
    // real source code. Unlike `normalize_line`'s full Unicode `to_lowercase()`, folding
    // per-unit while preserving raw offsets can't use a full Unicode case fold (that can change
    // the number of units), so this is a deliberately narrower approximation for this path only.
    let fold = |u: u16| if (b'A' as u16..=b'Z' as u16).contains(&u) { u + 32 } else { u };
    fold(a) == fold(b)
}

/// Common-prefix length in raw UTF-16 units for each side, honoring `opts`. Under
/// `ignore_whitespace`, a maximal whitespace run on one side matches a maximal whitespace run on
/// the other regardless of length (mirroring `normalize_line`'s `split_whitespace().join(" ")`),
/// and leading whitespace is skipped entirely on both sides first (mirroring
/// `split_whitespace()` dropping it) -- but whitespace present on only one side is a real
/// difference, not something to skip past. Returns raw indices, never normalized ones: these
/// feed directly into `Span.start_utf16`, which the frontend adds to the *rendered* line's raw
/// offset.
fn common_prefix_len_raw(left: &[u16], right: &[u16], opts: DiffOptions) -> (usize, usize) {
    let mut li = 0;
    let mut ri = 0;
    if opts.ignore_whitespace {
        while li < left.len() && is_ascii_ws_unit(left[li]) {
            li += 1;
        }
        while ri < right.len() && is_ascii_ws_unit(right[ri]) {
            ri += 1;
        }
    }
    loop {
        if li >= left.len() || ri >= right.len() {
            break;
        }
        let (a, b) = (left[li], right[ri]);
        if opts.ignore_whitespace && is_ascii_ws_unit(a) && is_ascii_ws_unit(b) {
            while li < left.len() && is_ascii_ws_unit(left[li]) {
                li += 1;
            }
            while ri < right.len() && is_ascii_ws_unit(right[ri]) {
                ri += 1;
            }
            continue;
        }
        if opts.ignore_whitespace && (is_ascii_ws_unit(a) || is_ascii_ws_unit(b)) {
            break;
        }
        if !eq_unit(a, b, opts.ignore_case) {
            break;
        }
        li += 1;
        ri += 1;
    }
    (li, ri)
}

/// Symmetric counterpart to `common_prefix_len_raw`, scanning from the end. Bounded by
/// `left_start`/`right_start` so it can never walk back past the already-claimed prefix.
/// Returns suffix lengths (counts of trailing units claimed), not raw indices.
fn common_suffix_len_raw(
    left: &[u16],
    right: &[u16],
    left_start: usize,
    right_start: usize,
    opts: DiffOptions,
) -> (usize, usize) {
    let mut li = left.len();
    let mut ri = right.len();
    if opts.ignore_whitespace {
        while li > left_start && is_ascii_ws_unit(left[li - 1]) {
            li -= 1;
        }
        while ri > right_start && is_ascii_ws_unit(right[ri - 1]) {
            ri -= 1;
        }
    }
    loop {
        if li <= left_start || ri <= right_start {
            break;
        }
        let (a, b) = (left[li - 1], right[ri - 1]);
        if opts.ignore_whitespace && is_ascii_ws_unit(a) && is_ascii_ws_unit(b) {
            while li > left_start && is_ascii_ws_unit(left[li - 1]) {
                li -= 1;
            }
            while ri > right_start && is_ascii_ws_unit(right[ri - 1]) {
                ri -= 1;
            }
            continue;
        }
        if opts.ignore_whitespace && (is_ascii_ws_unit(a) || is_ascii_ws_unit(b)) {
            break;
        }
        if !eq_unit(a, b, opts.ignore_case) {
            break;
        }
        li -= 1;
        ri -= 1;
    }
    (left.len() - li, right.len() - ri)
}

/// `DiffOptions`-aware counterpart to `intra_line_spans`. The plain version is exact-match
/// prefix/suffix trimming, which is correct only when the line-level diff that decided this pair
/// is a `Replace` hunk also used exact matching. Once ignore-whitespace/ignore-case are on, a
/// line can be a `Replace` hunk (differs after normalization) while *also* differing in raw
/// leading/trailing whitespace or casing that the toggle says not to care about -- exact-match
/// trimming then finds no common affix at all and highlights the entire line on both sides,
/// which is wrong: it should still narrow to just the part that differs under the same rules the
/// line-level diff used. Offsets returned are always raw indices (see `common_prefix_len_raw`).
pub fn intra_line_spans_with_options(left: &str, right: &str, opts: DiffOptions) -> Vec<Span> {
    if !opts.ignore_whitespace && !opts.ignore_case {
        return intra_line_spans(left, right);
    }
    let l: Vec<u16> = left.encode_utf16().collect();
    let r: Vec<u16> = right.encode_utf16().collect();

    let (prefix_l, prefix_r) = common_prefix_len_raw(&l, &r, opts);
    let (suffix_l, suffix_r) = common_suffix_len_raw(&l, &r, prefix_l, prefix_r, opts);
    let end_l = l.len() - suffix_l;
    let end_r = r.len() - suffix_r;

    let mut spans = Vec::new();
    if end_l > prefix_l {
        spans.push(Span { side: Side::Left, start_utf16: prefix_l as u32, len_utf16: (end_l - prefix_l) as u32 });
    }
    if end_r > prefix_r {
        spans.push(Span { side: Side::Right, start_utf16: prefix_r as u32, len_utf16: (end_r - prefix_r) as u32 });
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

#[derive(Debug, Clone, Copy, Default)]
pub struct DiffOptions {
    pub ignore_whitespace: bool,
    pub ignore_case: bool,
}

fn normalize_line(line: &str, opts: DiffOptions) -> String {
    let mut s: std::borrow::Cow<str> = line.into();
    if opts.ignore_case {
        s = s.to_lowercase().into();
    }
    if opts.ignore_whitespace {
        s = s.split_whitespace().collect::<Vec<_>>().join(" ").into();
    }
    s.into_owned()
}

/// Splitting and rejoining on `'\n'` reconstructs the exact original newline structure
/// (including a trailing newline, since `"a\n".split('\n')` yields `["a", ""]` and normalizing
/// the empty trailing piece is a no-op) — so this can never change the line count `diff_lines`
/// sees, which is what lets `Hunk` ranges computed against the normalized text stay valid
/// indices into the caller's original, un-normalized text.
fn normalize_for_compare(text: &str, opts: DiffOptions) -> String {
    if !opts.ignore_whitespace && !opts.ignore_case {
        return text.to_string();
    }
    text.split('\n').map(|line| normalize_line(line, opts)).collect::<Vec<_>>().join("\n")
}

/// Whitespace/case-ignore toggle per docs/PLAN.md's M1 feature list. Normalizes each line for
/// *comparison only* — the returned hunks index into the caller's original text unchanged,
/// since normalization never adds or removes a line (see `normalize_for_compare`).
pub fn diff_lines_with_options(left: &str, right: &str, opts: DiffOptions) -> FileDiffResult {
    diff_lines(&normalize_for_compare(left, opts), &normalize_for_compare(right, opts))
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

    #[test]
    fn default_options_match_diff_lines_exactly() {
        let opts = DiffOptions::default();
        let a = diff_lines_with_options("a\nb\nc\n", "a\nx\nc\n", opts);
        let b = diff_lines("a\nb\nc\n", "a\nx\nc\n");
        assert_eq!(a.stats.chunks, b.stats.chunks);
        assert_eq!(a.hunks.len(), b.hunks.len());
    }

    #[test]
    fn ignore_whitespace_treats_a_reindented_line_as_unchanged() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: false };
        let result = diff_lines_with_options("a\n  foo   bar\nc\n", "a\nfoo bar\nc\n", opts);
        assert_eq!(result.stats.chunks, 0, "whitespace-only difference should not be a hunk");
    }

    #[test]
    fn without_the_toggle_the_same_whitespace_difference_is_a_real_hunk() {
        let opts = DiffOptions::default();
        let result = diff_lines_with_options("a\n  foo   bar\nc\n", "a\nfoo bar\nc\n", opts);
        assert_eq!(result.stats.chunks, 1);
    }

    #[test]
    fn ignore_case_treats_a_recased_line_as_unchanged() {
        let opts = DiffOptions { ignore_whitespace: false, ignore_case: true };
        let result = diff_lines_with_options("a\nHello World\nc\n", "a\nhello world\nc\n", opts);
        assert_eq!(result.stats.chunks, 0);
    }

    #[test]
    fn combining_both_toggles_handles_a_line_that_differs_in_both_dimensions() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: true };
        let result = diff_lines_with_options("a\n  Hello   World\nc\n", "a\nhello world\nc\n", opts);
        assert_eq!(result.stats.chunks, 0);
    }

    #[test]
    fn ignoring_whitespace_does_not_hide_a_real_content_difference_elsewhere() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: false };
        let result = diff_lines_with_options("a\n  foo   bar\nc\n", "a\nfoo bar\nCHANGED\n", opts);
        assert_eq!(result.stats.chunks, 1, "the real change on line 3 must still be detected");
        let replace = result.hunks.iter().find(|h| h.kind == HunkKind::Replace).unwrap();
        assert_eq!(replace.left.start, 2); // line 3, zero-indexed -- the whitespace-only line 2 stayed equal
    }

    #[test]
    fn hunk_ranges_still_index_into_the_original_unnormalized_text() {
        // regression guard: a bug here would be a silent off-by-N in every hunk position,
        // not a panic, so pin the exact ranges rather than just stats.chunks.
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: false };
        let left = "a\n  spaced  out  \nc\nd\n";
        let right = "a\nspaced out\nc\nDIFFERENT\n";
        let result = diff_lines_with_options(left, right, opts);
        let replace = result.hunks.iter().find(|h| h.kind == HunkKind::Replace).unwrap();
        assert_eq!(replace.left, LineRange { start: 3, len: 1 });
        assert_eq!(replace.right, LineRange { start: 3, len: 1 });
    }

    #[test]
    fn normalize_for_compare_preserves_trailing_newline_presence() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: true };
        // identical content, one with a trailing newline and one without -- must still be
        // seen as a changed last line, exactly like the plain diff_lines behavior it wraps.
        let result = diff_lines_with_options("a\nb\nc", "a\nb\nc\n", opts);
        let non_equal: Vec<_> = result.hunks.iter().filter(|h| h.kind != HunkKind::Equal).collect();
        assert_eq!(non_equal.len(), 1);
    }

    #[test]
    fn every_equal_hunk_has_matching_line_counts_on_both_sides() {
        // Collapsed-region rendering on the frontend assumes an Equal hunk's left and right
        // line counts always match (so both panes collapse the same number of lines and stay
        // in sync). This is a property of diff_lines' construction (each Equal hunk is exactly
        // the untouched gap between two non-equal hunks, which imara-diff always advances by
        // the same amount on both sides for context it didn't touch), not asserted anywhere --
        // pin it here so a future change to hunk construction can't silently break it.
        let left = "a\nb\nc\nd\ne\nf\ng\n";
        let right = "a\nX\nc\nd\ne\nY\ng\n";
        let result = diff_lines(left, right);
        for h in result.hunks.iter().filter(|h| h.kind == HunkKind::Equal) {
            assert_eq!(h.left.len, h.right.len, "Equal hunk {:?} has mismatched side lengths", h);
        }
    }

    #[test]
    fn options_aware_intra_line_spans_matches_the_plain_version_when_no_toggle_is_set() {
        let opts = DiffOptions::default();
        assert_eq!(
            intra_line_spans_with_options("prefix-123-suffix", "prefix-abc-suffix", opts),
            intra_line_spans("prefix-123-suffix", "prefix-abc-suffix")
        );
    }

    /// Regression test for a real bug caught by review, not by the existing test suite: the
    /// plain `intra_line_spans` ignores `DiffOptions` entirely, so a line that differs in *both*
    /// whitespace amount and real content highlighted the whole line on both sides even with
    /// ignore-whitespace on, because the raw first/last characters never matched. The offsets
    /// returned must still index into the *raw*, un-normalized line -- `spansToMarkRanges` adds
    /// them to the rendered document's line offsets, so normalized-coordinate offsets would
    /// silently misalign every highlight, the same class of bug as the earlier UTF-16-as-binary
    /// mistake.
    #[test]
    fn ignore_whitespace_narrows_the_span_to_the_real_difference_not_the_whole_line() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: false };
        let spans = intra_line_spans_with_options("  foo  bar", "foo baz", opts);
        assert_eq!(
            spans,
            vec![
                Span { side: Side::Left, start_utf16: 9, len_utf16: 1 },
                Span { side: Side::Right, start_utf16: 6, len_utf16: 1 },
            ]
        );
        // sanity-check the offsets really do index into the raw strings as claimed
        let left_units: Vec<u16> = "  foo  bar".encode_utf16().collect();
        let right_units: Vec<u16> = "foo baz".encode_utf16().collect();
        assert_eq!(left_units[9], 'r' as u16);
        assert_eq!(right_units[6], 'z' as u16);
    }

    #[test]
    fn ignore_whitespace_finds_no_span_at_all_when_only_whitespace_amount_differs() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: false };
        assert_eq!(intra_line_spans_with_options("  foo   bar  ", "foo bar", opts), vec![]);
    }

    #[test]
    fn ignore_case_finds_no_span_when_only_casing_differs() {
        let opts = DiffOptions { ignore_whitespace: false, ignore_case: true };
        assert_eq!(intra_line_spans_with_options("FOO bar", "foo BAR", opts), vec![]);
    }

    #[test]
    fn combining_both_toggles_narrows_to_the_real_difference() {
        let opts = DiffOptions { ignore_whitespace: true, ignore_case: true };
        let spans = intra_line_spans_with_options("  Hello   World", "hello world!", opts);
        assert_eq!(
            spans,
            vec![Span { side: Side::Right, start_utf16: 11, len_utf16: 1 }],
            "only the trailing '!' should be flagged as a real difference"
        );
    }

    #[test]
    fn without_toggles_the_options_aware_version_behaves_like_a_plain_diff() {
        let opts = DiffOptions::default();
        let spans = intra_line_spans_with_options("  foo  bar", "foo baz", opts);
        // no ignoring at all: the leading whitespace difference alone breaks the common prefix,
        // so (unlike the ignore-whitespace case above) the whole line is flagged on both sides.
        assert_eq!(
            spans,
            vec![
                Span { side: Side::Left, start_utf16: 0, len_utf16: 10 },
                Span { side: Side::Right, start_utf16: 0, len_utf16: 7 },
            ]
        );
    }
}
