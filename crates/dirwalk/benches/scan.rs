// Pure-Rust regression benchmark for the directory-pair scan, independent of the UI -- per
// docs/PLAN.md §7 ("Pure-Rust algorithmic benchmarks... track diff-core/dirwalk regressions
// independent of the UI"). Not a substitute for the ≤1s first-rows target itself, which is
// asserted at the real 50k-file scale against the generated fixture in
// `first_batch_of_the_50k_fixture_scan_arrives_within_one_second` (src/lib.rs, `--ignored`) --
// that test needs `fixtures/50k-file-tree` generated first and is the actual acceptance-criteria
// check. This benchmark builds its own smaller tree so `cargo bench -p dirwalk` has no external
// setup step and stays fast enough to run on every invocation, not just on demand.

use criterion::{Criterion, criterion_group, criterion_main};
use dirwalk::{ScanOptions, scan};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

const FILES_PER_DIR: usize = 40;

/// Same mtime forced on every file (`filetime`, not just "created moments apart") -- otherwise
/// two independently-`fs::write`'d trees have close-but-unequal mtimes, and `classify`'s size+
/// mtime fast path (see its doc comment) misses on every single file, falling through to a real
/// content read for all of them. That's a real cost this crate genuinely pays whenever mtimes
/// don't line up, but it's not what this benchmark is for -- it exists to track the scan's own
/// per-entry overhead, not `contents_equal`'s I/O cost, so the setup pins mtimes to isolate that.
fn build_tree(root: &Path, total_files: usize, mtime: SystemTime) {
    let mut n = 0;
    let mut dir_idx = 0;
    while n < total_files {
        let dir = root.join(format!("dir{dir_idx}"));
        fs::create_dir_all(&dir).unwrap();
        for _ in 0..FILES_PER_DIR {
            if n >= total_files {
                break;
            }
            let path = dir.join(format!("f{n}.txt"));
            fs::write(&path, format!("content {n}\n")).unwrap();
            fs::File::open(&path).unwrap().set_modified(mtime).unwrap();
            n += 1;
        }
        dir_idx += 1;
    }
}

/// Built once, scanned many times by the benchmark loop below -- tree construction (thousands
/// of small file writes) is itself expensive and not part of what this benchmark measures.
fn setup_paired_tree(total_files: usize) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("dirwalk-bench-{}", std::process::id()));
    let (left, right) = (base.join("left"), base.join("right"));
    let _ = fs::remove_dir_all(&base);
    let mtime = SystemTime::now() - Duration::from_secs(60); // clearly in the past on both sides
    build_tree(&left, total_files, mtime);
    build_tree(&right, total_files, mtime); // identical trees -- every entry is `Same`
    (left, right)
}

fn bench_scan(c: &mut Criterion) {
    let (left, right) = setup_paired_tree(2_000);
    c.bench_function("scan/2k_files_identical", |b| {
        b.iter(|| {
            let cancel = AtomicBool::new(false);
            scan(&left, &right, &ScanOptions::default(), &cancel, |_batch| {})
        });
    });
    fs::remove_dir_all(left.parent().unwrap()).ok();
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
