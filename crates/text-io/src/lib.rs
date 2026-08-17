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

#[cfg(test)]
mod tests {
    use super::*;

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
