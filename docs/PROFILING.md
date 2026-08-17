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

## Running optimization table

| # | change | metric before | metric after | verdict |
|---|---|---|---|---|
| 1 | `estimatedHeight` override + line-height fix | paint 2684ms, 23.3fps | paint 2839ms, 22.2fps | no measurable win — `estimatedHeight` reverted; line-height correction kept (correctness, not perf) |
| 2 | scroll-position mapping, no padding widgets | paint 2684ms, 23.3fps | paint 100.6ms, 58.1fps | measurable win, **reverted anyway** — functional regression (loses full-pane alignment), confirmed visually |

## Where this leaves M1

The shipped state at the end of this pass is **the original M0 baseline**, unchanged in
behavior, with only the line-height correctness fix applied. The performance ceiling this
investigation *proved reachable* (98ms paint, 58fps steady, in this same sandbox) requires
eliminating block-widget alignment padding, and doing so without a functional regression needs
an approach this pass didn't find in the time available — candidates worth trying next, in
roughly increasing order of effort:

- Batch/merge adjacent small gaps into fewer, larger widgets (untested here — the sparse-padding
  probe reduced *count* but not by merging, and even a single widget seems to trigger most of the
  fixed cost, so this may not help much; worth testing directly before investing further).
- Render alignment gaps as a separate absolutely-positioned overlay layer *outside* CM6's own
  document flow (each pane keeps real content at uniform line height; a thin overlay recomputed
  on scroll draws the visual gap indicators), rather than as CM6 decorations at all.
- Reconsider whether full pixel-for-pixel mid-pane alignment is required for every hunk, or
  whether an explicit, deliberate approximation (documented as such, unlike attempt 2) is
  acceptable for very large files specifically — this is a product decision, not an engineering
  one, and should go back to the user rather than being decided autonomously.
