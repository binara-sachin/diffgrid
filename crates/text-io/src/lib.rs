use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Latin1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LineEnding {
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    pub trailing_newline: bool,
    pub is_binary: bool,
    pub line_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedText {
    pub meta: FileMeta,
    /// CRLF normalized to LF, BOM stripped. Empty and meaningless when `meta.is_binary`.
    pub normalized: String,
}

const BINARY_SNIFF_LEN: usize = 8000;

/// Git's own heuristic: a NUL byte in the first slice of the file is a strong binary signal.
/// A file can fail UTF-8/be non-ASCII without being binary (see `Encoding::Latin1`), so this
/// is checked independently of, and before, encoding detection.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_LEN)].contains(&0)
}

pub fn detect_encoding(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        Encoding::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Encoding::Utf16Be
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) || std::str::from_utf8(bytes).is_ok() {
        Encoding::Utf8
    } else {
        Encoding::Latin1
    }
}

/// Decodes `bytes` per `encoding`, stripping a leading BOM if the encoding implies one.
/// Malformed UTF-16 code units decode to U+FFFD rather than panicking or truncating.
pub fn decode(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Utf8 => {
            let s = String::from_utf8_lossy(bytes);
            s.strip_prefix('\u{FEFF}').map(str::to_string).unwrap_or_else(|| s.into_owned())
        }
        Encoding::Utf16Le => decode_utf16_bom(bytes, u16::from_le_bytes),
        Encoding::Utf16Be => decode_utf16_bom(bytes, u16::from_be_bytes),
        Encoding::Latin1 => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn decode_utf16_bom(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let body = &bytes[bytes.len().min(2)..];
    let units = body.chunks_exact(2).map(|c| unit([c[0], c[1]]));
    char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

pub fn detect_line_ending(text: &str) -> LineEnding {
    let (mut saw_crlf, mut saw_lf) = (false, false);
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'\n' {
            continue;
        }
        if i > 0 && bytes[i - 1] == b'\r' {
            saw_crlf = true;
        } else {
            saw_lf = true;
        }
    }
    match (saw_crlf, saw_lf) {
        (true, true) => LineEnding::Mixed,
        (true, false) => LineEnding::Crlf,
        _ => LineEnding::Lf,
    }
}

pub fn has_trailing_newline(text: &str) -> bool {
    text.ends_with('\n')
}

pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub fn line_count(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let newlines = text.bytes().filter(|&b| b == b'\n').count();
    let extra = if text.ends_with('\n') { 0 } else { 1 };
    (newlines + extra) as u32
}

/// Full load pipeline for one side of a file pair: binary refusal, encoding detection,
/// decode, and normalization, all from raw bytes. File I/O itself is the caller's
/// responsibility (kept out of this crate so detection logic is testable without a
/// filesystem).
pub fn load(bytes: &[u8]) -> LoadedText {
    let encoding = detect_encoding(bytes);
    // UTF-16 legitimately puts a NUL byte in every other position for ASCII content, so the
    // NUL-sniff binary heuristic only applies once we know we're not looking at recognized
    // UTF-16 (BOM-detected, so this is a real signal, not a guess).
    let is_utf16 = matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be);
    if !is_utf16 && is_binary(bytes) {
        return LoadedText {
            meta: FileMeta {
                encoding: Encoding::Utf8,
                line_ending: LineEnding::Lf,
                trailing_newline: false,
                is_binary: true,
                line_count: 0,
            },
            normalized: String::new(),
        };
    }

    let decoded = decode(bytes, encoding);
    let line_ending = detect_line_ending(&decoded);
    let trailing_newline = has_trailing_newline(&decoded);
    let normalized = normalize_line_endings(&decoded);

    LoadedText {
        meta: FileMeta {
            encoding,
            line_ending,
            trailing_newline,
            is_binary: false,
            line_count: line_count(&normalized),
        },
        normalized,
    }
}

/// Encodes `text` (LF-only, as held in an edited buffer) back to bytes matching `meta`'s
/// encoding and line-ending style, for M2's save path. This is only the *edited*-buffer half of
/// saving: an unedited buffer must write back the exact original bytes captured at open time
/// (byte-identical, including any BOM and the file's original mixed-line-ending pattern) rather
/// than going through this function at all — round-tripping through `load()`'s normalization
/// and then re-deriving bytes here would lose information `load()` already discarded (which
/// exact newlines were CRLF and which were LF in a `Mixed` file; whether a UTF-8 BOM was
/// present). That decision belongs to the caller (`session::EditBuffer`, which holds the
/// original bytes and a dirty flag); this function only handles the case where the buffer's
/// content has actually changed and a real re-encode is unavoidable.
///
/// `LineEnding::Mixed` has no way to be preserved once the content has changed -- the per-line
/// origin of each newline was already discarded at load time, and there is no principled way to
/// decide which (possibly new) lines should get CRLF vs LF. Normalizes to LF uniformly on save,
/// same as `LineEnding::Lf`. This is a deliberate, documented behavior change (see
/// DECISIONS.md), not a silent one -- it only ever applies to files that were already
/// mixed-line-ending *and* have been edited.
///
/// Returns an error rather than lossily substituting a placeholder character if `text` contains
/// a character that cannot be represented in `meta.encoding` (only possible for `Latin1`, where
/// any character above U+00FF has no encoding) -- silently replacing what the user typed with
/// `?` on save would be data loss.
pub fn to_bytes(text: &str, meta: &FileMeta) -> Result<Vec<u8>, String> {
    let line_converted: std::borrow::Cow<str> = match meta.line_ending {
        LineEnding::Lf | LineEnding::Mixed => text.into(),
        LineEnding::Crlf => text.replace('\n', "\r\n").into(),
    };

    match meta.encoding {
        Encoding::Utf8 => Ok(line_converted.into_owned().into_bytes()),
        Encoding::Utf16Le => Ok(encode_utf16_bom(&line_converted, u16::to_le_bytes, [0xFF, 0xFE])),
        Encoding::Utf16Be => Ok(encode_utf16_bom(&line_converted, u16::to_be_bytes, [0xFE, 0xFF])),
        Encoding::Latin1 => {
            let mut bytes = Vec::with_capacity(line_converted.len());
            for c in line_converted.chars() {
                let cp = c as u32;
                if cp > 0xFF {
                    return Err(format!(
                        "character {c:?} (U+{cp:04X}) cannot be represented in Latin-1 -- save refused rather than substituting a placeholder"
                    ));
                }
                bytes.push(cp as u8);
            }
            Ok(bytes)
        }
    }
}

fn encode_utf16_bom(text: &str, to_bytes: fn(u16) -> [u8; 2], bom: [u8; 2]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(bom.len() + text.len() * 2);
    bytes.extend_from_slice(&bom);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&to_bytes(unit));
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(encoding: Encoding, line_ending: LineEnding) -> FileMeta {
        FileMeta { encoding, line_ending, trailing_newline: true, is_binary: false, line_count: 0 }
    }

    #[test]
    fn to_bytes_lf_utf8_is_unchanged() {
        let bytes = to_bytes("a\nb\nc\n", &meta(Encoding::Utf8, LineEnding::Lf)).unwrap();
        assert_eq!(bytes, b"a\nb\nc\n");
    }

    #[test]
    fn to_bytes_converts_lf_to_crlf_for_crlf_line_ending() {
        let bytes = to_bytes("a\nb\nc\n", &meta(Encoding::Utf8, LineEnding::Crlf)).unwrap();
        assert_eq!(bytes, b"a\r\nb\r\nc\r\n");
    }

    /// Documented, deliberate behavior (see DECISIONS.md): a `Mixed`-line-ending file that has
    /// been edited can't have its exact original per-line CRLF/LF pattern reconstructed (that
    /// information was already discarded by `normalize_line_endings` at load time), so an
    /// edited Mixed buffer saves as uniform LF rather than guessing.
    #[test]
    fn to_bytes_mixed_line_ending_normalizes_to_lf_when_edited() {
        let bytes = to_bytes("a\nb\nc\n", &meta(Encoding::Utf8, LineEnding::Mixed)).unwrap();
        assert_eq!(bytes, b"a\nb\nc\n");
    }

    #[test]
    fn to_bytes_encodes_utf16le_with_bom() {
        let bytes = to_bytes("hi\n", &meta(Encoding::Utf16Le, LineEnding::Lf)).unwrap();
        let mut expected = vec![0xFF, 0xFE];
        for u in "hi\n".encode_utf16() {
            expected.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn to_bytes_encodes_utf16be_with_bom() {
        let bytes = to_bytes("hi\n", &meta(Encoding::Utf16Be, LineEnding::Lf)).unwrap();
        let mut expected = vec![0xFE, 0xFF];
        for u in "hi\n".encode_utf16() {
            expected.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn to_bytes_utf16_round_trips_through_decode() {
        // encode -> decode must recover the exact text, for both byte orders, including an
        // astral character that needs a surrogate pair.
        let text = "a\nb😀\nc\n";
        for enc in [Encoding::Utf16Le, Encoding::Utf16Be] {
            let bytes = to_bytes(text, &meta(enc, LineEnding::Lf)).unwrap();
            assert_eq!(decode(&bytes, enc), text);
        }
    }

    #[test]
    fn to_bytes_encodes_latin1_ascii_and_high_bytes() {
        let bytes = to_bytes("a\u{80}b\n", &meta(Encoding::Latin1, LineEnding::Lf)).unwrap();
        assert_eq!(bytes, &[b'a', 0x80, b'b', b'\n']);
    }

    #[test]
    fn to_bytes_rejects_a_char_the_target_encoding_cannot_represent_rather_than_substituting() {
        let result = to_bytes("hello 😀\n", &meta(Encoding::Latin1, LineEnding::Lf));
        assert!(result.is_err(), "an astral emoji has no Latin-1 representation and must be refused, not replaced");
    }

    #[test]
    fn empty_bytes_is_not_binary() {
        assert!(!is_binary(b""));
    }

    #[test]
    fn a_nul_byte_anywhere_in_the_sniff_window_marks_binary() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world"));
    }

    #[test]
    fn a_nul_byte_past_the_sniff_window_is_not_seen() {
        let mut bytes = vec![b'a'; BINARY_SNIFF_LEN + 10];
        bytes[BINARY_SNIFF_LEN + 5] = 0;
        assert!(!is_binary(&bytes));
    }

    #[test]
    fn plain_ascii_is_detected_as_utf8() {
        assert_eq!(detect_encoding(b"hello world"), Encoding::Utf8);
    }

    #[test]
    fn multibyte_utf8_without_bom_is_detected_as_utf8() {
        assert_eq!(detect_encoding("héllo".as_bytes()), Encoding::Utf8);
    }

    #[test]
    fn utf8_bom_is_detected_and_stripped_on_decode() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hi");
        assert_eq!(detect_encoding(&bytes), Encoding::Utf8);
        assert_eq!(decode(&bytes, Encoding::Utf8), "hi");
    }

    #[test]
    fn utf16le_bom_round_trips_ascii_text() {
        let mut bytes = vec![0xFF, 0xFE];
        for b in "hi\n".encode_utf16() {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        assert_eq!(detect_encoding(&bytes), Encoding::Utf16Le);
        assert_eq!(decode(&bytes, Encoding::Utf16Le), "hi\n");
    }

    #[test]
    fn utf16be_bom_round_trips_ascii_text() {
        let mut bytes = vec![0xFE, 0xFF];
        for b in "hi\n".encode_utf16() {
            bytes.extend_from_slice(&b.to_be_bytes());
        }
        assert_eq!(detect_encoding(&bytes), Encoding::Utf16Be);
        assert_eq!(decode(&bytes, Encoding::Utf16Be), "hi\n");
    }

    #[test]
    fn invalid_utf8_without_a_bom_falls_back_to_latin1() {
        // 0x80 alone is a continuation byte with no lead byte -- invalid UTF-8.
        let bytes = &[b'a', 0x80, b'b'];
        assert_eq!(detect_encoding(bytes), Encoding::Latin1);
        // Latin-1's defining property: byte value == Unicode scalar value.
        assert_eq!(decode(bytes, Encoding::Latin1), "a\u{80}b");
    }

    #[test]
    fn detects_pure_lf() {
        assert_eq!(detect_line_ending("a\nb\nc\n"), LineEnding::Lf);
    }

    #[test]
    fn detects_pure_crlf() {
        assert_eq!(detect_line_ending("a\r\nb\r\n"), LineEnding::Crlf);
    }

    #[test]
    fn detects_mixed_line_endings() {
        assert_eq!(detect_line_ending("a\r\nb\nc\r\n"), LineEnding::Mixed);
    }

    #[test]
    fn no_newlines_at_all_defaults_to_lf() {
        assert_eq!(detect_line_ending("just one line"), LineEnding::Lf);
    }

    #[test]
    fn trailing_newline_is_detected() {
        assert!(has_trailing_newline("a\nb\n"));
        assert!(!has_trailing_newline("a\nb"));
        assert!(!has_trailing_newline(""));
    }

    #[test]
    fn normalize_converts_crlf_to_lf_without_touching_lone_lf() {
        assert_eq!(normalize_line_endings("a\r\nb\nc\r\n"), "a\nb\nc\n");
    }

    #[test]
    fn line_count_matches_real_line_semantics_not_split_segments() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 1);
        assert_eq!(line_count("a\nb\n"), 2);
        assert_eq!(line_count("a\nb"), 2);
    }

    #[test]
    fn load_does_not_mistake_utf16_for_binary_despite_its_nul_bytes() {
        // Every ASCII character in UTF-16LE is `byte, 0x00` -- a real, valid text file that
        // would trip the NUL-byte binary heuristic if encoding weren't checked first.
        let mut bytes = vec![0xFF, 0xFE];
        for u in "a\nb\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let loaded = load(&bytes);
        assert!(!loaded.meta.is_binary);
        assert_eq!(loaded.meta.encoding, Encoding::Utf16Le);
        assert_eq!(loaded.normalized, "a\nb\n");
    }

    /// Pins the wire format: `FileMeta`'s multi-word fields must serialize as camelCase to
    /// match the frontend's `FileMeta` TS interface. See diff-core's identically-motivated test
    /// on `Span` for the real bug this class of test caught (a missing `rename_all` produced
    /// `undefined` on the frontend with no compile-time or IPC-level error at all).
    #[test]
    fn file_meta_serializes_with_camel_case_field_names_matching_the_frontend_type() {
        let meta = FileMeta {
            encoding: Encoding::Utf16Le,
            line_ending: LineEnding::Crlf,
            trailing_newline: true,
            is_binary: false,
            line_count: 7,
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["encoding"], "utf16Le");
        assert_eq!(json["lineEnding"], "crlf");
        assert_eq!(json["trailingNewline"], true);
        assert_eq!(json["isBinary"], false);
        assert_eq!(json["lineCount"], 7);
    }

    #[test]
    fn load_detects_binary_and_refuses_to_decode() {
        let loaded = load(b"PK\x03\x04\0binary\0stuff");
        assert!(loaded.meta.is_binary);
        assert_eq!(loaded.normalized, "");
    }

    #[test]
    fn load_pipeline_combines_bom_stripping_crlf_normalization_and_meta() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"line1\r\nline2\r\n");
        let loaded = load(&bytes);
        assert!(!loaded.meta.is_binary);
        assert_eq!(loaded.meta.encoding, Encoding::Utf8);
        assert_eq!(loaded.meta.line_ending, LineEnding::Crlf);
        assert!(loaded.meta.trailing_newline);
        assert_eq!(loaded.meta.line_count, 2);
        assert_eq!(loaded.normalized, "line1\nline2\n");
    }
}
