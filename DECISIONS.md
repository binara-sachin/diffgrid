# Decisions log

Ambiguous calls made autonomously during the "complete, working, benchmarked build" pass, per
the working agreement: choose what's most consistent with existing conventions, record it here,
keep going. Newest entries at the bottom.

## 2026-08-17 — Scope of "complete build" for this pass

**Decision**: "complete, working, benchmarked build" is scoped to hardening, testing, profiling,
and optimizing what M0 already established (the diff-core histogram engine + the CM6 dual-pane
render pipeline with alignment padding, decorations, and synced scroll) — not to starting
unbuilt M1 features (encoding/line-ending detection, in-place editing, save, hunk apply/revert).

**Why**: `docs/PLAN.md` scopes M0 as a feasibility gate and M1 as the first real vertical slice;
nothing in M1 has been started yet, and the task brief's "current state" is explicitly the M0
spike. Treating "complete" as "all of M1" would mean starting a milestone whose own plan
document says it needs encoding detection, binary refusal, etc. — a much larger scope than what
this pass's objective (profile → optimize → benchmark → hand off for macOS comparison) implies.
The M0 spike's own results (`docs/M0-RESULTS.md`) already identified a concrete, unresolved
performance defect (the alignment-padding widget mechanism) — fixing that and hardening the
surrounding code into something tested and documented is squarely "complete the thing that
exists," and is what the fps/render-time/cold-launch priority ordering in this task's brief is
clearly aimed at.

**How to apply**: Definition-of-done items ("all features implemented and passing tests") are
read against the M0 feature set (line-level diff, dual-pane render, decorations, scroll sync,
alignment), not the full M1 list. If this scoping later turns out to be wrong, the fix is cheap:
M1 work resumes cleanly on top of a now-tested, now-benchmarked M0 foundation either way.

## 2026-08-17 — Profiling methodology given no sampling profiler in headless WebKitGTK

**Decision**: use targeted `performance.mark`/`measure` instrumentation and a CM6
`EditorView.updateListener` (logging `update.geometryChanged` and transaction timing) to localize
hot paths, rather than a sampling JS profiler.

**Why**: the actual rendering happens inside the Tauri WebView's JS engine (WebKitGTK on Linux,
WKWebView on macOS), which has no remote-debugging protocol reachable from this headless sandbox
(WebKit Inspector Protocol isn't CDP-compatible with Playwright, and there's no display-attached
inspector client available here). The M0 A/B test already demonstrated that hypothesis-driven
instrumentation + isolation (disable one mechanism, remeasure) is effective in this environment.

**How to apply**: `docs/PROFILING.md` documents this methodology explicitly as a platform
constraint, not a shortcut — real macOS work should attach Safari Web Inspector to the WKWebView
and can use a proper sampling profiler; that's called out in `PLATFORM_NOTES.md`.

## 2026-08-17 — Don't commit generated benchmark fixtures

**Decision**: `fixtures/100k-line-pair/` and `fixtures/10k-line-pair/` are gitignored;
`fixtures/gen/gen-line-pair.mjs` (the deterministic, seeded generator) is committed instead.

**Why**: the initial scaffold commit included the generated fixture files directly, adding
~229k lines to the repo for content that's fully and deterministically reproducible from an
already-committed script. `docs/PLAN.md` §7 already established this principle for the (much
larger) 50k-file directory-tree fixture; the same reasoning applies here, just noticed late.

**How to apply**: `README.md`'s setup instructions must include the fixture-generation command
as a required step before running the benchmark harness — it is no longer implicit.
