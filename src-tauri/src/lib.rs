use std::path::PathBuf;
use serde::Serialize;
use tauri::ipc::Response;

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
/// normalized text. No editing, no directories yet — that's M2/M3.
#[tauri::command]
fn open_file_pair(left: String, right: String) -> Result<OpenPairResult, String> {
    let left_bytes = std::fs::read(&left).map_err(|e| format!("{left}: {e}"))?;
    let right_bytes = std::fs::read(&right).map_err(|e| format!("{right}: {e}"))?;
    diff_pair(&left_bytes, &right_bytes)
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
/// actual logic (and its tests) live in `diff_core::intra_line_spans_with_options`.
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
) -> Vec<diff_core::Span> {
    diff_core::intra_line_spans_with_options(
        &left_line,
        &right_line,
        diff_core::DiffOptions { ignore_whitespace, ignore_case },
    )
}

/// Re-diffs two already-loaded, already-normalized texts with whitespace/case-ignore toggles
/// applied, per docs/PLAN.md's M1 feature list. Takes text rather than a path so a toggle
/// change doesn't re-read the file from disk — the frontend already has both texts in memory
/// from `open_file_pair`/`open_file_text` at open time.
#[tauri::command]
fn diff_texts(left: String, right: String, ignore_whitespace: bool, ignore_case: bool) -> diff_core::FileDiffResult {
    diff_core::diff_lines_with_options(&left, &right, diff_core::DiffOptions { ignore_whitespace, ignore_case })
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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            diff_fixture, fixture_text, report_ready, report_bench, report_error, bench_flags,
            open_file_pair, open_file_text, launch_args, intra_line_spans, diff_texts
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
