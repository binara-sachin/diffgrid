# Profiling report — M0 diff-pane render pipeline

Scope: hardening pass on the M0 spike (see `docs/M0-RESULTS.md` for the original spike numbers
and the environment caveats — Linux sandbox, no GPU, WebKitGTK, Xvfb — that apply to every
number in this document too). Priority order per the task brief: fps > render time > cold launch.

## Methodology

No sampling JS profiler is reachable from this headless sandbox — the diff panes render inside
the Tauri WebView's JS engine (WebKitGTK on Linux), which has no remote-debugging protocol
attachable here (WebKit Inspector Protocol isn't CDP-compatible with the tooling available, and
there's no display-attached inspector client). Instead: targeted `performance.now()`
instrumentation, a CM6 `EditorView.updateListener` logging `geometryChanged`/`heightChanged`/
`viewportChanged` with timestamps, and hypothesis-driven A/B isolation (toggle one mechanism,
re-measure, compare) — the same technique the M0 A/B test already used successfully. See
`DECISIONS.md` for why this was chosen over alternatives.

Real macOS work should attach Safari Web Inspector to the WKWebView and can use an actual
sampling profiler — noted in `PLATFORM_NOTES.md`.

## Baseline (start of this pass)

Carried over from `docs/M0-RESULTS.md` §2 — the M0 spike's own baseline, 5 runs:

| metric | value |
|---|---|
| in-app open-to-first-paint (100k-line fixture) | 2684ms mean |
| steady-state scroll fps | 23.3 mean |
| steady-state worst frame | 732ms mean, 800ms max |
| idle memory (host + descendants) | 177.7MB mean |
| launch-only (total − paint) | ~710-790ms |

## Root-cause investigation

**Instrumentation evidence** (single diagnostic run, padding enabled): `actualLineHeightPx=18`
against a hardcoded `LINE_HEIGHT_PX=20` — a real ~10% calibration error. `geometryChanged` fired
8 times total; the 4 that occurred during the scroll window all had `heightChanged: true,
viewportChanged: true`, and their count matched `framesOver33ms` exactly. This looked like a
clean correlation between "CM6 re-measures a widget's height as it scrolls into view" and the
severe frame stalls.

## Attempt 1: accurate `estimatedHeight` — no measurable win, reverted

**Hypothesis**: `PadWidget` never overrode `WidgetType.estimatedHeight` (CM6 defaults unmeasured
widgets to `-1`), so every widget forced a full layout reconciliation the first time it was
approached during scroll.

**Change**: added `get estimatedHeight()` returning `lines * LINE_HEIGHT_PX`, and corrected
`LINE_HEIGHT_PX` from the unmeasured guess of 20 to the real measured value of 18.

**Result after fix** (5 runs, padding still enabled): paint 2839ms mean (vs. 2684ms before — no
improvement, within noise), steady fps 22.2 mean (vs. 23.3 — no improvement, within noise).

**Verdict**: no measurable win. Per the task's own rule, reverted — except the `LINE_HEIGHT_PX`
correction itself, which is kept as an independent correctness fix (alignment padding was
visually ~10% too tall regardless of any perf question) and shipped as its own `fix:` commit.

## Discriminating probes: what actually drives the cost

Two further probes, varying one variable at a time rather than guessing:

| probe | widget count | per-widget DOM cost | paint (mean) | steady fps |
|---|---|---|---|---|
| baseline (full padding) | ~390 | real (`lines × 18px`) | 2839ms | 22.2 |
| sparse padding (1-in-10) | ~39 | real | 2435ms (−15%) | 43.7 (+97%) |
| zero-height padding | ~390 | trivial (`0px`) | 2674ms (≈0%) | 23.1 (≈0%) |
| padding disabled entirely | 0 | n/a | 98ms (−96%) | 58.1 (+162%) |

Reading across the rows: **scroll fps scales with widget *count*** (sparse padding, same
per-widget cost, 10x fewer widgets → fps roughly doubles) — consistent with one stall per widget
newly scrolled into view. But **mount-time paint does *not* scale with count** (390→39 widgets
only saves 15%) **and does not depend on per-widget DOM cost** (zero-height widgets at full count
change nothing). Paint only recovers once widget count reaches exactly zero.

**Conclusion**: CM6 pays a large, roughly fixed cost the moment *any* non-uniform-height block
content exists in the document — consistent with switching from a fast uniform-line-height
layout path to a heightmap/variable-height layout structure over the whole 100k-line document.
This is a categorical cost (present vs. absent), not a per-widget or per-pixel one. No amount of
widget-count reduction or per-widget cost reduction closes it; only removing block widgets
entirely does.

## Attempt 2: scroll-position mapping instead of content-flow padding — reverted (functional regression)

**Hypothesis**: since disabling padding entirely recovers the full performance ceiling (98ms
paint, 58fps), replace the alignment *mechanism* — instead of reserving space in each document's
own flow (forcing the expensive layout mode), compute which line on one side corresponds to the
top-of-viewport line on the other (via a pure hunk-based `mapLine` function) and set `scrollTop`
to that line's real, CM6-reported position. Both documents stay on the fast uniform-line-height
path; no widgets at all.

**Result**: matched the "padding disabled" ceiling almost exactly — 100.6ms paint, 58.1fps
steady, near-zero severe stalls (5 runs).

**Why it was reverted anyway**: verified visually (screenshots taken mid-scroll-benchmark under
Xvfb) rather than trusting the benchmark numbers alone, and found real problems the benchmark
cannot see because it only scrolls one pane programmatically and never inspects content:

1. **Alignment drifts below the top line.** The mapping only corrects the single line at the
   top of the viewport on each scroll event; everything else visible in the pane is positioned
   by each document's own (different) real line count. A visible screenshot partway through the
   scroll benchmark showed the two panes off by 2 lines even near the top of the viewport, not
   just deeper in — confirmed by content inspection, not assumption. This is a direct regression
   against the approved UI model (`docs/UI/ui-01.png` shows hatched gap regions spanning the
   full pane specifically so corresponding content aligns throughout the visible area, not only
   at one reference point).
2. **The sync is asymmetric and non-idempotent**, which independently causes drift even ignoring
   (1): `mapLine` is lossy in the length-changing direction (multiple lines on the longer side
   collapse to one line on the shorter side), so `map(map(x)) ≠ x`. Combined with `scrollTop`
   writes firing the target's `scroll` event asynchronously (after the naive `syncing` boolean
   has already been reset), this created a feedback loop with no fixed point — each round-trip
   through a length-changing hunk could nudge both panes further off correspondence.

**Verdict**: a genuine, large, measured performance win that trades away a feature the user
already approved (full-pane alignment, not just top-line alignment). Reverted in full —
`alignment.ts`/`alignment.test.ts` deleted, `diffView.ts`/`syncScroll` restored to block-widget
padding. Recorded here rather than shipped quietly as "alignment restored," since the numbers
alone would have said otherwise.

## Attempt 3: CSS padding on a line attribute instead of a block widget — shipped

**Hypothesis**: the root-cause finding above is specific to *block-level* decorations (widgets,
`block: true`, or block-level `Decoration.replace`) — CM6's height-model oracle only switches out
of its cheap fixed-line-height path when the document contains one of those, because that's what
it inspects when deciding whether every line can be assumed to share one measured height. A plain
`Decoration.line()` attribute that sets `padding-top`/`padding-bottom` via inline CSS is not a
block decoration at all — CM6 never measures it, so it should never trip the switch — but the
browser still lays out the extra padding as real box height, so the two panes' subsequent content
still gets pushed apart by the correct amount. Unlike Attempt 2, this changes *how* space is
reserved, not *whether* space is reserved — the gap is still real, in-document, full-pane, exactly
like the original mechanism — so it does not carry Attempt 2's functional-regression risk by
construction, not just by measurement.

**Implementation**: `buildDecorations` in `src/lib/diffView.ts` replaced the
`Decoration.widget({ block: true, ... })` call with `Decoration.line({ attributes: { class:
"diff-pad", style: "padding-top: Npx" } })` (or `padding-bottom` when the gap trails the last line
of the document, since there's no following line to attach `padding-top` to). `PadWidget` and its
DOM-node class were deleted. The `.diff-pad` hatched-stripe CSS rule (previously styling the
widget's own `<div>`) still applies — `padding`, unlike `margin`, is inside the element's
background-painted box, so the visual "this is filler, not content" treatment survives the
mechanism change unmodified.

**Result** — 5 runs, padding fully enabled, same 100k-line fixture, same sandbox:

| metric | before (block widget) | after (line padding) |
|---|---|---|
| in-app open-to-first-paint | 2684.4ms mean | 98.8ms mean |
| steady-state scroll fps | 23.3 mean | 54.6 mean |
| p95 frame time | 110.0ms | 22.0ms |
| worst frame time | 732.0ms mean | 23.4ms mean |
| frames >33ms per run | 4-5 | **0** (every run, all 5) |

This matches Attempt 2's measured ceiling (98-101ms paint, 54-58fps) while keeping the padding
mechanism itself unchanged in kind — real reserved space in each document's own flow, not a
scroll-position hack layered on top of unmodified documents.

**Visual verification** (the gate Attempt 2 failed, applied here as a precondition rather than an
afterthought): screenshots taken under Xvfb at 0.5s intervals through the live scroll benchmark.
One frame caught the exact moment the mechanism was exercised — the right pane one line ahead of
the left (an unpadded 1-line insert about to scroll into view) — followed by frames showing line
numbers and content back in 1:1 correspondence at matching pixel Y-positions for 60+ consecutive
lines afterward, including across a second hunk boundary later in the same scroll. This is the
*sustained*, full-pane alignment Attempt 2 could not produce (it only ever corrected the single
line the sync handler actively touched); here every line stays aligned because the browser's own
box layout is doing the pushing, not a per-frame correction.

**Verdict: shipped.** Real, measured win, and — checked directly rather than assumed — no
functional regression.

## Running optimization table

| # | change | metric before | metric after | verdict |
|---|---|---|---|---|
| 1 | `estimatedHeight` override + line-height fix | paint 2684ms, 23.3fps | paint 2839ms, 22.2fps | no measurable win — `estimatedHeight` reverted; line-height correction kept (correctness, not perf) |
| 2 | scroll-position mapping, no padding widgets | paint 2684ms, 23.3fps | paint 100.6ms, 58.1fps | measurable win, **reverted anyway** — functional regression (loses full-pane alignment), confirmed visually |
| 3 | CSS `padding` line attribute instead of block widget | paint 2684ms, 23.3fps | paint 98.8ms, 54.6fps | measurable win, **shipped** — same reserved-space mechanism, no functional regression, confirmed visually |

## Correction to M0-RESULTS.md: idle memory is not stable, contradicting the earlier claim

While making the benchmark harness cross-platform (replacing `/proc`-based RSS sampling with a
`ps`-based one that works on macOS too), the harness was re-run against **unchanged code** and
idle memory measured **~995MB** — a ~5.6x increase from the ~178MB recorded in
`docs/M0-RESULTS.md`. This is not a measurement bug: `ps`'s `rss` column and `/proc/<pid>/status`'s
`VmRSS` were checked side-by-side for the same live process and agree exactly (777600kB both
ways). The actual `WebKitWebProcess` RSS itself changed by that much, with the same binary, the
same fixture, the same everything except wall-clock time in a long session.

The correlated variable: system-wide free memory. `docs/M0-RESULTS.md`'s measurements were taken
while this sandbox was mid-toolchain-install (rustup, apt packages, multiple `cargo build`s
running or having just run) — memory pressure was real. This later re-run happened with the
system otherwise idle (~11GiB free out of ~12GiB total). WebKitGTK appears to scale its
cache/heap behavior to ambient system memory pressure, which is unsurprising for a browser
engine but means **RSS is not a fixed property of this app** in this environment — it reflects
how much memory WebKit believes it can spend, not how much the app needs.

`docs/M0-RESULTS.md` §4 explicitly claimed idle memory was "the metric least distorted by the
[Linux/no-GPU/WebKitGTK] gap" and "real evidence, not just directional." **That claim is wrong**
— idle memory turned out to be *more* environment-sensitive than initially assessed, just to a
different environmental variable (memory pressure) than the ones originally considered (GPU,
compositor, WebView engine). The 178MB and 995MB numbers are both real, valid measurements of
the same code under different ambient memory conditions; neither one alone is "the" idle-memory
number for this app.

The harness now logs `os.freemem()`/`os.totalmem()` (portable, cross-platform) alongside every
run specifically so this can't silently happen again unnoticed — any future idle-memory number
should be reported together with the ambient memory-pressure line, not alone. This needs to be
checked on macOS too, not assumed away: whether WKWebView has comparable adaptive behavior is
unverified.

## Where this leaves M1

**Resolved.** Attempt 3 (CSS padding on a `Decoration.line()` attribute) reaches the same
performance ceiling as the reverted scroll-mapping attempt — paint 2684ms→98.8ms, fps 23.3→54.6,
zero frames over 33ms across every run — without its functional regression, verified visually
under Xvfb rather than assumed from the numbers. This is now shipped, and the block-widget
alignment-padding problem that blocked M1's diff-pane design is no longer open. M1 can build
collapsed-unchanged-region widgets (PLAN.md §6) on top of this pane without inheriting the
non-uniform-height cost, as long as they're implemented the same way — as line attributes, not
block-level decorations — or verified independently if they can't be.

One item deliberately not chased further: `disable-padding` A/B runs (58.1fps, zero widgets at
all) are still marginally faster than Attempt 3's enabled-padding numbers (54.6fps) — a small,
consistent gap (~3fps) that first-scroll-frame and steady-fps numbers agree on across repeated
runs. This is far inside the noise band relative to the ~35fps win just banked, and pursuing it
further is not warranted by the fps>render>launch priority ordering unless a future workload
shows it actually matters.
