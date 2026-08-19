use ropey::Rope;
use text_io::FileMeta;

mod settings;
pub use settings::{load_settings, save_settings, IntraLineMode, Settings, TakeBothSide};

/// One side of an open file pair, per docs/PLAN.md §2 and §5. CM6's document is authoritative
/// for editing; this is the Rust-side shadow buffer, kept in sync via incremental deltas
/// (`apply_delta`) rather than full-document resends. Holds the original bytes captured at open
/// time so an unedited save can write them back verbatim -- see `to_bytes`.
pub struct EditBuffer {
    rope: Rope,
    original_bytes: Vec<u8>,
    meta: FileMeta,
    dirty: bool,
}

impl EditBuffer {
    pub fn new(normalized_text: &str, original_bytes: Vec<u8>, meta: FileMeta) -> Self {
        Self { rope: Rope::from_str(normalized_text), original_bytes, meta, dirty: false }
    }

    /// Applies one edit delta captured from a CM6 transaction. `from_utf16`/`to_utf16` are
    /// UTF-16 code-unit offsets into the document *before* this delta (per docs/PLAN.md §3's
    /// IPC contract) -- the caller must apply deltas from a single transaction, and successive
    /// transactions, strictly in order, since each delta's offsets are only valid against the
    /// buffer state that precedes it. Converts to `ropey`'s char indices via its own built-in
    /// UTF-16 accounting (`utf16_cu_to_char`) rather than a hand-rolled scan, so astral
    /// characters (surrogate pairs: 2 UTF-16 units, 1 `char`) are never miscounted.
    pub fn apply_delta(&mut self, from_utf16: u32, to_utf16: u32, inserted: &str) -> Result<(), String> {
        if from_utf16 > to_utf16 {
            return Err(format!("delta has from ({from_utf16}) after to ({to_utf16})"));
        }
        let from_char = self
            .rope
            .try_utf16_cu_to_char(from_utf16 as usize)
            .map_err(|_| format!("from_utf16 {from_utf16} is out of bounds"))?;
        let to_char = self
            .rope
            .try_utf16_cu_to_char(to_utf16 as usize)
            .map_err(|_| format!("to_utf16 {to_utf16} is out of bounds"))?;
        if from_char < to_char {
            self.rope.remove(from_char..to_char);
        }
        if !inserted.is_empty() {
            self.rope.insert(from_char, inserted);
        }
        self.dirty = true;
        Ok(())
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Bytes to write on save. An unedited buffer returns the *exact* original bytes captured
    /// at open time -- not a re-encode of the normalized text through `text_io::to_bytes`, since
    /// that would lose information `load()` already discarded (a UTF-8 BOM's presence, or which
    /// specific newlines in a `Mixed`-line-ending file were CRLF vs LF) even though nothing was
    /// actually edited. Only a dirty buffer re-encodes for real.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        if !self.dirty {
            return Ok(self.original_bytes.clone());
        }
        text_io::to_bytes(&self.text(), &self.meta)
    }

    /// Call after successfully writing `to_bytes()`'s output to disk: clears the dirty flag and
    /// adopts `written` as the new "original bytes" baseline, so a *subsequent* unedited save
    /// short-circuits correctly again instead of re-encoding (or re-normalizing a Mixed file's
    /// line endings a second time) on every save regardless of whether anything changed since.
    pub fn mark_saved(&mut self, written: Vec<u8>) {
        self.original_bytes = written;
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_io::{Encoding, LineEnding};

    fn meta() -> FileMeta {
        FileMeta { encoding: Encoding::Utf8, line_ending: LineEnding::Lf, trailing_newline: true, is_binary: false, line_count: 3 }
    }

    #[test]
    fn new_buffer_starts_clean() {
        let buf = EditBuffer::new("a\nb\nc\n", b"a\nb\nc\n".to_vec(), meta());
        assert!(!buf.is_dirty());
        assert_eq!(buf.text(), "a\nb\nc\n");
    }

    #[test]
    fn apply_delta_marks_dirty() {
        let mut buf = EditBuffer::new("a\nb\nc\n", b"a\nb\nc\n".to_vec(), meta());
        buf.apply_delta(0, 0, "x").unwrap();
        assert!(buf.is_dirty());
    }

    #[test]
    fn apply_delta_inserts_at_a_position() {
        let mut buf = EditBuffer::new("ac", b"ac".to_vec(), meta());
        buf.apply_delta(1, 1, "b").unwrap();
        assert_eq!(buf.text(), "abc");
    }

    #[test]
    fn apply_delta_deletes_a_range() {
        let mut buf = EditBuffer::new("abc", b"abc".to_vec(), meta());
        buf.apply_delta(1, 2, "").unwrap();
        assert_eq!(buf.text(), "ac");
    }

    #[test]
    fn apply_delta_replaces_a_range() {
        let mut buf = EditBuffer::new("hello world", b"hello world".to_vec(), meta());
        buf.apply_delta(6, 11, "there").unwrap();
        assert_eq!(buf.text(), "hello there");
    }

    #[test]
    fn apply_delta_rejects_a_backwards_range() {
        let mut buf = EditBuffer::new("abc", b"abc".to_vec(), meta());
        assert!(buf.apply_delta(2, 1, "x").is_err());
    }

    #[test]
    fn apply_delta_rejects_an_out_of_bounds_offset() {
        let mut buf = EditBuffer::new("abc", b"abc".to_vec(), meta());
        assert!(buf.apply_delta(0, 100, "x").is_err());
    }

    /// An astral emoji is 1 `char` but 2 UTF-16 code units -- inserting right after it must
    /// land at UTF-16 offset 3 (not 2), or the edit lands one code unit early, inside the
    /// surrogate pair. This is the same class of bug the diff-core UTF-16 offset tests guard
    /// against, now on the write path instead of the read path.
    #[test]
    fn apply_delta_positions_correctly_after_an_astral_character() {
        let mut buf = EditBuffer::new("a😀c", b"a\xf0\x9f\x98\x80c".to_vec(), meta());
        let prefix_units = "a😀".encode_utf16().count() as u32;
        assert_eq!(prefix_units, 3);
        buf.apply_delta(prefix_units, prefix_units, "b").unwrap();
        assert_eq!(buf.text(), "a😀bc");
    }

    /// Simulates several edits arriving one at a time (as separate `apply_delta` calls, each
    /// against the buffer state left by the previous one) rather than a single change --
    /// exactly how sequential keystrokes are captured and forwarded. Each offset is relative to
    /// the buffer *after* the prior delta, not the original text.
    #[test]
    fn sequential_deltas_each_apply_against_the_result_of_the_previous_one() {
        let mut buf = EditBuffer::new("", b"".to_vec(), meta());
        buf.apply_delta(0, 0, "a").unwrap(); // "a"
        buf.apply_delta(1, 1, "b").unwrap(); // "ab"
        buf.apply_delta(1, 2, "x").unwrap(); // "ax" (replaced "b")
        buf.apply_delta(0, 0, "_").unwrap(); // "_ax"
        assert_eq!(buf.text(), "_ax");
    }

    #[test]
    fn to_bytes_short_circuits_to_original_bytes_when_not_dirty() {
        // Deliberately mismatched meta (Latin1, which the current text can't be re-encoded
        // into -- see the Latin1-rejection test in text-io) to prove the short-circuit really
        // skips re-encoding entirely for a clean buffer, not just that it happens to succeed.
        let unrepresentable_meta =
            FileMeta { encoding: Encoding::Latin1, line_ending: LineEnding::Lf, trailing_newline: true, is_binary: false, line_count: 1 };
        let buf = EditBuffer::new("hello 😀\n", "hello 😀\n".as_bytes().to_vec(), unrepresentable_meta);
        assert_eq!(buf.to_bytes().unwrap(), "hello 😀\n".as_bytes());
    }

    #[test]
    fn to_bytes_reencodes_when_dirty() {
        let mut buf = EditBuffer::new("a\nb\nc\n", b"a\nb\nc\n".to_vec(), meta());
        buf.apply_delta(0, 1, "X").unwrap();
        assert_eq!(buf.to_bytes().unwrap(), b"X\nb\nc\n");
    }

    #[test]
    fn to_bytes_propagates_a_reencode_error_when_dirty_and_unrepresentable() {
        let unrepresentable_meta =
            FileMeta { encoding: Encoding::Latin1, line_ending: LineEnding::Lf, trailing_newline: true, is_binary: false, line_count: 1 };
        let mut buf = EditBuffer::new("hello\n", b"hello\n".to_vec(), unrepresentable_meta);
        buf.apply_delta(5, 5, "😀").unwrap();
        assert!(buf.to_bytes().is_err());
    }

    #[test]
    fn mark_saved_clears_dirty_and_adopts_the_written_bytes_as_the_new_baseline() {
        let mut buf = EditBuffer::new("a\nb\nc\n", b"a\nb\nc\n".to_vec(), meta());
        buf.apply_delta(0, 1, "X").unwrap();
        let written = buf.to_bytes().unwrap();
        buf.mark_saved(written.clone());
        assert!(!buf.is_dirty());
        assert_eq!(buf.to_bytes().unwrap(), written);
    }
}
