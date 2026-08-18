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

## 2026-08-18 — Intra-line highlighting must honor ignore-whitespace/ignore-case, not just line-level diff

**Bug found by review, not by the existing test suite**: `intra_line_spans` did exact-match
prefix/suffix trimming with no awareness of `DiffOptions`. With ignore-whitespace on, a line
differing in *both* whitespace amount and real content (e.g. `"  foo  bar"` vs `"foo baz"`)
still enters a `Replace` hunk (normalized forms differ), but the raw first/last characters never
matched, so the entire line highlighted on both sides — toggling the checkbox visibly changed
nothing about that line's highlight even though it changed which lines counted as hunks at all.
Existing tests never caught this because the toggle-verification tests all used
whitespace-*only* differences (correctly producing zero hunks, so no intra-line fetch ever
fired), and diff-core's intra-line tests never passed options because the function couldn't
accept them.

**Fix**: added `intra_line_spans_with_options`, a second prefix/suffix trim that walks raw
UTF-16 units directly (not a normalized string) so the returned offsets stay valid indices into
the *raw* line — normalizing first and trimming that would misalign every highlight the same way
the earlier UTF-16-as-binary bug did, just silently. Whitespace-run matching mirrors
`normalize_line`'s `split_whitespace().join(" ")` semantics (a run on one side matches a run of
any length on the other; presence-vs-absence of a run is still a real difference); case-folding
at the unit level is ASCII-only (ASCII a-z/A-Z), narrower than `normalize_line`'s full Unicode
`to_lowercase()`, because a full Unicode case fold can change unit counts and break the
raw-offset guarantee. Documented as an accepted narrowing rather than left unremarked.

**How to apply**: any future change to how the line-level diff normalizes text for comparison
must have a matching change here, or the same class of "toggle changes the hunk, not the
highlight" bug reappears. If intra-line highlighting is ever generalized beyond ignore-
whitespace/ignore-case, keep the raw-offset invariant — verify with a test asserting
`start_utf16`/`len_utf16` index into the *raw* input strings, not just that span count changed.

## 2026-08-18 — Settings-window intra-line mode (Off/Word/Character) not built; Character only

`ui-02.png`'s mockup shows a three-way intra-line-highlight setting: Off / Word / Character. M1
ships Character-level highlighting only, always on for `Replace` hunks — there is no settings
window, and no way to switch to Word-level or turn highlighting off. This is a real gap against
the mockup, not an oversight discovered late: the settings window itself is scoped to a later
milestone (M4's session shell), so there was nowhere to put the toggle yet. Noting it explicitly
here rather than leaving it as an unremarked deviation from the approved design.

**How to apply**: when the M4 settings window is built, add the Off/Word/Character control then.
Word-level highlighting would need a different segmentation than the current prefix/suffix trim
(word-boundary-aware diffing, not raw UTF-16-unit trimming) — treat it as new work, not a trivial
mode flag on the existing function.

## 2026-08-18 — Hunk apply/revert exposed via toolbar acting on the current hunk, not inline gutter buttons

**Decision**: M2's "apply/revert individual hunks left↔right" (docs/PLAN.md) ships as two toolbar
buttons ("← Copy to left" / "Copy to right →") that act on whichever hunk the existing Prev/Next
diff navigation has selected, rather than a button/arrow rendered inline in the gutter next to
each individual hunk.

**Why**: the brief specifies the *capability*, not the UI placement. Per-hunk inline controls
would need a new CM6 decoration/widget layer purely for the buttons themselves (on top of the
existing line-highlight, padding, collapse, and intra-line-mark decorations already layered over
these documents), which is a materially larger and more failure-prone piece of work than reusing
navigation state that already exists and is already tested. Toolbar-driven copy is a complete,
usable version of the feature; inline per-hunk buttons are a polish item, not a correctness gap.

**Implementation note worth keeping**: the copy itself is *not* a new backend command. It's built
as a single CM6 `{from, to, insert}` change (`buildHunkCopyChange` in `diffView.ts`) dispatched on
the destination pane's `EditorView`, which flows through the exact same `onEdit` → `apply_edit` →
debounced `redo_diff` pipeline a keystroke would. `posAfterLine` mapping a hunk's `{start, len}`
line range to a *character* range is what makes one formula handle all three hunk kinds and both
directions (an `Insert`/`Delete` hunk has `len === 0` on one side, which the same formula turns
into an empty destination range — a pure insertion — or an empty source range — a deletion —
without any hunk-kind-specific branching).

**How to apply**: don't special-case `Insert`/`Delete` hunks in any future change to this code
path; the empty-range behavior of `posAfterLine` is what keeps them unified with `Replace`. If
inline per-hunk gutter buttons are ever added, they should call the same `buildHunkCopyChange` +
dispatch-on-destination-view mechanism, not a parallel implementation.

**Selection after a copy**: `currentHunk` resets to `-1` after any hunk copy, the same as after a
toggle or any other edit that invalidates the hunk list — chosen instead of trying to re-point at
"the same" hunk post-copy, since the copied hunk usually disappears and every hunk after it can
shift position. Consistent behavior across every hunks-invalidating event beats a bespoke
re-selection heuristic that would only apply to this one path.

**Real bug this caught, not a hypothetical**: the first version of this wiring crashed
(`TypeError: undefined is not an object (evaluating 's.kind')`) on the very first hunk-copy click
after a fresh file open, because `runRealFiles` set `changeLines` directly instead of going
through `applyNewHunks` (which is what actually populates `currentChangeHunks`, the array
`copyCurrentHunk` indexes into) — `currentChangeHunks` stayed `[]` until the first toggle or edit
had triggered a real re-diff. No unit test caught this, since none of them exercise
`runRealFiles`'s real `invoke()` calls end-to-end; it was only caught by manually clicking through
the feature under Xvfb. Fixed by having `runRealFiles` call `applyNewHunks` directly instead of
duplicating a subset of what it does.
