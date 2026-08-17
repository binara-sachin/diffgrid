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
}
