// Pure-Rust regression benchmark for the histogram line-diff hot path, independent of the UI
// -- per docs/PLAN.md §7 ("Pure-Rust algorithmic benchmarks... track diff-core/dirwalk
// regressions independent of the UI"). Not a substitute for the end-to-end open-to-first-paint
// numbers in bench/open-file-bench.mjs, which are what the actual acceptance criteria specify --
// this just catches an algorithmic regression in `diff_lines` itself, in isolation, on every
// `cargo bench` without needing a built app or a generated fixture on disk.
//
// Input is synthesized in-process (not read from fixtures/, which are gitignored and
// JS-generated) so `cargo bench -p diff-core` has no external setup step.

use criterion::{Criterion, criterion_group, criterion_main};
use diff_core::diff_lines;

// Deterministic, dependency-free PRNG (mulberry32, matching fixtures/gen/gen-line-pair.mjs's
// generator in spirit) -- reproducible across runs without pulling in a `rand` dependency for a
// single benchmark input.
struct Rng(u32);
impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6d2b79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
}

/// 10k lines/side, ~5% of lines replaced in small clusters -- a representative hunk density,
/// same shape of workload as `fixtures/10k-line-pair` without depending on it being generated.
fn make_line_pair(total_lines: u32) -> (String, String) {
    let mut rng = Rng(7);
    let mut left = String::new();
    let mut right = String::new();
    let mut n = 0u32;
    while n < total_lines {
        let run_len = (40 + rng.below(400)).min(total_lines - n);
        for i in 0..run_len {
            let line = format!("  const value_{} = compute(offset, {});\n", n + i, n + i);
            left.push_str(&line);
            right.push_str(&line);
        }
        n += run_len;
        if n >= total_lines {
            break;
        }
        let hunk_len = (1 + rng.below(5)).min(total_lines - n);
        for i in 0..hunk_len {
            left.push_str(&format!("  const old_{} = legacy(index, {});\n", n + i, n + i));
            right.push_str(&format!("  const new_{} = updated(index, {});\n", n + i, n + i));
        }
        n += hunk_len;
    }
    (left, right)
}

fn bench_diff_lines(c: &mut Criterion) {
    let (left, right) = make_line_pair(10_000);
    c.bench_function("diff_lines/10k_lines_5pct_changed", |b| {
        b.iter(|| diff_lines(&left, &right));
    });
}

criterion_group!(benches, bench_diff_lines);
criterion_main!(benches);
