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

## 2026-08-17 — Reverted a measured performance win because it regressed a shipped feature

**Decision**: the scroll-position-mapping alignment mechanism (`alignment.ts`, a rewritten
`syncScroll`) is not shipped, despite measuring a large, real improvement (paint 2684ms→100ms,
steady scroll 23fps→58fps in this sandbox). Reverted to the original block-widget padding
approach. Full writeup in `docs/PROFILING.md` "Attempt 2."

**Why**: verified visually (screenshots mid-scroll, not just the benchmark's numbers) and found
that alignment only held at the single line the sync mechanism actively corrects — everything
else visible in the pane drifted, confirmed by reading line numbers/content in the screenshots,
not by assumption. That's a regression against the UI model already approved earlier in this
project (`docs/UI/ui-01.png` shows full-pane hatched alignment, not single-point alignment). The
task's "reject non-wins" rule is about *performance* wins that don't measure out; this was a
performance win that traded away a different, already-committed requirement — a different
failure mode, and one judged not to be the autonomous-session's call to make silently.

**How to apply**: don't re-attempt this exact mechanism without first solving the alignment-drift
problem it has (the mapping is lossy/non-idempotent in the length-changing direction, so
`map(map(x)) ≠ x`, which alone would cause feedback-loop drift even if full-pane alignment were
otherwise addressed). `docs/PROFILING.md` lists candidate next approaches for M1.

## 2026-08-17 — CLI argument parsing lives in the `app` crate for now, not a new `vcs-cli` crate

**Decision**: `diffgrid FILE1 FILE2` argv handling (`launch_args` command in
`src-tauri/src/lib.rs`, branched on in `+page.svelte`) is implemented directly in the `app`
crate rather than creating the `vcs-cli` crate PLAN.md §5 describes.

**Why**: `vcs-cli`'s actual scope per PLAN.md is `git difftool`/`mergetool` argument conventions
and the exit-code contract — real complexity, explicitly assigned to M6, not M1. M1 only needs
two positional file paths recognized so the real diff pane can be exercised end-to-end; building
a whole crate (and its own module boundary) for that now would be speculative structure ahead of
the requirement that actually justifies it.

**How to apply**: when M6's git-integration work starts, this logic should move into a real
`vcs-cli` crate rather than growing in place inside `app` — `app` is supposed to stay thin
command/event wiring, and `launch_args`'s current one-line implementation is only appropriate
because the parsing it does is currently trivial (arg count, nothing else).

## 2026-08-17 — `text-io`'s "streaming reads for large files" (PLAN.md §5) deferred past M1

**Decision**: `text-io::load` takes an in-memory `&[u8]` and the `app` crate reads whole files
via `std::fs::read` before calling it. No chunked/streaming read path exists yet.

**Why**: M1's read-only two-file-diff slice doesn't need it to be correct or demoable — the
100k-line benchmark fixture already exercises whole-file reads at a representative size without
issue. Streaming reads are worth building once there's a concrete multi-hundred-MB file problem
to solve against, not speculatively.

**How to apply**: if a real file large enough to make `std::fs::read` itself a bottleneck shows
up (measured, not assumed), that's the trigger to revisit this — not before.

## 2026-08-17 — Intra-line diff uses prefix/suffix trimming, not full LCS/Myers

**Decision**: `diff_core::intra_line_spans` finds the common prefix and suffix between two lines
and marks everything in between as changed on each side, rather than running a full
LCS/Myers-style minimal-edit-script algorithm.

**Why**: a DP-based LCS diff is O(n·m) in line length, which is fine for typical lines but has no
upper bound on a pathologically long single line — exactly the kind of eager, unbounded cost this
project's fps-priority mandate has already spent a full investigation getting rid of elsewhere
(see `docs/PROFILING.md`). Prefix/suffix trimming is O(n), can't blow up regardless of input, and
is a well-established "good enough" technique for intra-line highlighting in practice.

**How to apply**: this will occasionally produce a larger "changed" span than a human would pick
when a line has two separate edits far apart (e.g. two single-character changes at opposite ends
of a long line get merged into one span covering everything between them). Don't fix this
speculatively — revisit only if it visibly produces unhelpfully large spans on real content, and
prefer a bounded approach (e.g. LCS only below a length cap, falling back to trimming above it)
over an unconditional full LCS if it does.

## 2026-08-18 — Collapsed unchanged regions shipped; click-to-expand deferred

**Decision**: `Decoration.replace({block: true})` collapsing large `Equal` hunks (>20 lines,
leaving 3 lines of context at each edge) ships as an always-on part of real file diffs. There is
no click-to-expand — a collapsed region stays collapsed for the life of the view.

**Why measured, not assumed, before shipping**: `Decoration.replace` is a block-level decoration,
the same class `docs/PROFILING.md` found triggers CM6's expensive non-uniform-height layout mode
for `Decoration.widget`. Before writing any UI, this was A/B measured with
`bench/m0-spike.mjs --collapse-equal` against the current (already-fixed) padding baseline: 392
collapsed ranges on the 100k-line fixture, fps 57.6→57.8 and paint 101ms→110ms — both within
noise. Unlike a widget (which adds height CM6 must track alongside surrounding lines), a
`replace` decoration removes its range from layout entirely, which is apparently why it doesn't
trip the same mode. Shipping this on the strength of that measurement, not the a priori
assumption that "block-level always means slow" — the earlier investigation characterized one
specific mechanism, not the whole decoration category.

**Why click-to-expand is deferred, not silently missing**: making a collapsed region
interactive means the decoration set has to react to a UI event, which means `createDiffEditor`'s
static `EditorView.decorations.of(...)` needs to become a `StateField` updated by dispatched
effects — the same refactor whitespace/case-ignore toggles need for their own reason (a toggle
changes the hunk list, which changes every decoration derived from it). Doing that refactor once,
for both features together, is the plan — see the next DECISIONS.md entry when that work starts.
Shipping collapse now without interactivity is a real, usable subset of the feature; it is not
scoped as done-done.

**How to apply**: don't add click-to-expand to collapse in isolation — do it as part of the
StateField refactor that whitespace/case-ignore toggles require anyway, so the same live-update
mechanism serves both.
