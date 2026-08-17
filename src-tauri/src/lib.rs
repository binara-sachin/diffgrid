use std::path::PathBuf;
use serde::Serialize;
use tauri::ipc::Response;

#[derive(Serialize)]
struct BenchFlags {
    disable_padding: bool,
}

/// M0 A/B toggle: lets the measurement harness isolate whether the block-widget alignment
/// padding is the source of the scroll-onset stall, rather than asserting a cause untested.
#[tauri::command]
fn bench_flags() -> BenchFlags {
    BenchFlags {
        disable_padding: std::env::var("DIFFGRID_DISABLE_PADDING").is_ok(),
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![diff_fixture, fixture_text, report_ready, report_bench, report_error, bench_flags])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
