# M0 spike results

Status: **inconclusive on this hardware, with one decisive isolated finding.** Do not treat the
headline numbers below as a verdict on Option C until re-run on the actual macOS target — see
§4. The A/B result in §3, however, is a real architectural finding independent of platform.

## 0. How to reproduce

```
npm run build
cargo build --release -p app --features tauri/custom-protocol
node bench/m0-spike.mjs 5                    # baseline (padding widgets enabled)
node bench/m0-spike.mjs 5 --disable-padding  # A/B: padding widgets disabled
```

**The `--features tauri/custom-protocol` flag is required** when building the binary directly
with `cargo build` instead of `tauri build`/`tauri dev`. Without it, the release binary tries to
load the frontend from `http://localhost:1420` (the dev server URL) and fails with "Connection
refused" — it does not fall back to the embedded `frontendDist` assets. This cost several failed
runs to diagnose; `src-tauri/build.rs` also needed a `cargo:rerun-if-changed=../build` line added,
since `tauri-build` embeds the frontend's *current* contents at compile time but doesn't watch
that directory, so a `cargo build` after only a frontend change silently reuses a stale embed.

## 1. Environment (read before the numbers)

This sandbox is Linux (aarch64), not macOS. Three compounding gaps between what was measured and
the actual target:

1. **WebKitGTK, not WKWebView.** Tauri on Linux uses a fundamentally different WebView engine
   than the macOS target.
2. **No GPU acceleration.** `libEGL warning: DRI3 error: Could not get DRI3 device` — WebKitGTK
   fell back to software rendering/compositing for this entire run. A real Linux desktop with a
   working GPU would already look substantially different from these numbers; macOS with
   hardware-accelerated WKWebView more so.
3. **Xvfb virtual display**, not a real compositor.

Idle memory is the one metric here least sensitive to (1)-(3) — it's dominated by process count
and heap size, not rendering path — so it's the most trustworthy number in this report as-is.
Cold-launch and scroll-fps are the most sensitive to the gap and should be re-measured on the
actual M4 Pro before being used to decide anything.

## 2. Baseline (padding widgets enabled) — 5 runs

| metric | mean | p50 | max |
|---|---|---|---|
| spawn → `DIFFGRID_READY` (total) | 3411.5ms | 3349.3ms | 3584.4ms |
| — launch-only portion (total − in-app paint) | 727.1ms | 712.1ms | 789.8ms |
| — in-app open-to-first-paint portion | 2684.4ms | 2640.0ms | 2868.0ms |
| idle memory (host + descendant processes, summed RSS) | 177.7MB | 177.5MB | 178.9MB |
| first scroll-triggered frame (one-time) | 16.4ms | 17.0ms | 17.0ms |
| steady-state scroll fps (mean frame time, excl. first frame) | 23.3 | 24.0 | — |
| steady-state p95 frame time | 110.0ms | — | — |
| steady-state worst frame time | 732.0ms | — | max 800.0ms |
| frames >33ms per ~90-frame run | 4-5 | — | — |

Against the kill criteria written into `docs/PLAN.md` §8 (>600ms cold launch, <55fps sustained
scroll, >350MB idle memory → fall back to Option A):

- **Idle memory: passes comfortably at 178MB vs 350MB threshold, but this claim needs a
  correction — see `docs/PROFILING.md` "Correction to M0-RESULTS.md."** A later re-run of the
  same code with the sandbox otherwise idle (vs. mid-toolchain-install here) measured ~995MB.
  Idle memory turned out to be *more* environment-sensitive than assessed below, not less — it
  tracks ambient system memory pressure, which WebKitGTK adapts its caching to. Do not treat
  either number as "the" idle-memory figure for this app; both are real, under different
  conditions.
- **Scroll fps: fails** (23fps vs 55fps threshold) — but see §3, this is not evenly distributed;
  it's a handful of severe periodic stalls (~700-800ms) against an otherwise decent frame rate.
- **Cold launch: ambiguous.** The raw total (3.4s) blows the threshold, but 2.68s of that is
  in-app paint work, not launch. The launch-only portion (≈710-790ms) is close to the 600ms
  threshold, not 6x over it — and launch time is exactly the number most inflated by
  software-rendered WebKitGTK startup on Linux.

## 3. A/B: padding widgets disabled — 5 runs

The alignment-padding block widgets (one per insert/delete/replace gap, inserted into CM6 as
block-level widget decorations so the two panes' line counts visually align) were the leading
hypothesis for both the paint cost and the periodic scroll stalls. Tested directly rather than
asserted:

| metric | mean | p50 | max |
|---|---|---|---|
| spawn → `DIFFGRID_READY` (total) | 790.7ms | 790.9ms | 811.0ms |
| — launch-only portion | 693.1ms | 698.9ms | 708.0ms |
| — in-app open-to-first-paint portion | **97.6ms** | 95.0ms | 103.0ms |
| idle memory | 177.5MB | 177.7MB | 178.1MB |
| steady-state scroll fps | **58.1** | 58.0 | — |
| steady-state worst frame time | 25.6ms | — | max 30.0ms |
| frames >33ms per ~234-frame run | **0** | — | — |

Removing the padding widgets: open-to-first-paint drops **~27x** (2684ms → 98ms), steady-state
scroll fps goes from 23 to **58** (essentially at the 60fps target), and the periodic multi-hundred-
millisecond stalls disappear entirely — zero frames over 33ms across all 5 runs, vs. 4-5 per run
in the baseline. Launch-only time is unchanged (≈700ms in both configurations), confirming it's
unrelated to the rendering approach — it's Tauri/WebKitGTK/Xvfb startup overhead.

**Conclusion: the bottleneck is isolated to the block-widget alignment-padding mechanism, not the
Option C stack as a whole.** The periodic stalls match a specific, known cause in virtualized
editors: CM6 estimates off-screen block-widget heights and re-measures/reconciles them against
actual DOM height as they scroll into view, which is consistent with stalls recurring every ~1s
during a 4s scroll of a 100k-line document with ~390 change hunks (and thus ~390 widgets) spread
through it, rather than one stall concentrated at scroll onset (which was the first, wrong,
hypothesis — see the harness's `firstScrollFrameMs`, which is small, ~15-17ms, in both configs).

This is real signal for M1's design, independent of the platform gap in §1: alignment padding
needs a cheaper mechanism than "one CM6 block widget per gap, eagerly, for the whole document" —
candidates to evaluate include viewport-driven widget instantiation, batching adjacent gaps, or a
non-widget positioning technique (e.g. CSS-transform-based visual offset computed from cumulative
gap heights rather than in-flow block elements CM6 must measure).

## 4. Verdict and recommendation

**Kill criteria triggered on the baseline, at face value.** That's not being softened — the
thresholds were written down precisely so this wouldn't turn into a negotiation.

**But the recommendation is not "fall back to Option A."** This run is not a measurement of the
actual target (§1), two of the three thresholds are exactly the ones most distorted by that gap,
and the one number least affected by it (idle memory) passed comfortably. The defensible read is:
**the M0 gate is inconclusive on this hardware.**

Next step, in order:
1. Re-run `node bench/m0-spike.mjs 5` (both with and without `--disable-padding`) on the actual
   M4 Pro before spending anything on an Option A spike. If steady-state scroll fps without
   padding is ≥55fps and launch-only time is ≤600ms there, Option C's M0 gate passes outright and
   the padding-widget redesign becomes ordinary M1 design work, not a stack-level crisis.
2. Regardless of platform, treat §3's finding as a fixed input to M1's diff-pane design: the
   current padding-widget approach does not scale to 100k-line files and needs to be replaced
   before, not after, M1 is built on top of it.
