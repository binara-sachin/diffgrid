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

## 2026-08-18 — `redo_diff` held two locks sequentially instead of together (caught by review, not tests)

**Bug**: `redo_diff_impl`'s first version read `state.left.lock().unwrap().as_ref()...text()` and
`state.right.lock().unwrap().as_ref()...text()` as two separate statements/expressions. Each
temporary lock guard is dropped at the end of its own statement, so there was a real gap between
releasing the left lock and acquiring the right one. Tauri commands aren't guaranteed to run
serialized, so a concurrent `apply_edit` on either side landing in that gap would make `redo_diff`
diff a left snapshot from time T against a right snapshot from time T+1 -- producing a
`FileDiffResult` whose `LineRange`s don't correspond to either pane's actual content. That result
then feeds directly into `buildCollapseRanges`/`spansToMarkRanges` on the frontend, the same
"metadata silently stops matching the text it describes" failure class as the earlier UTF-16 and
serde bugs, just from a torn read instead of a wire-format mismatch.

**Fix**: bind both lock guards to variables first, then read both `.text()` values while both are
held (see `redo_diff_impl`). Always locks left before right; nothing else in `src-tauri/src/lib.rs`
locks both sides at once, so this can't deadlock against another caller locking in the opposite
order.

**Why this wasn't caught by the existing test suite, and won't get a regression test either**: the
race window is a handful of CPU instructions between two lock acquisitions. Reliably forcing a
real interleaving there in a test would need either a production-code test-only synchronization
hook (bad: changes shipped code for testability) or a probabilistic thread-timing test (bad: the
kind of flaky test that erodes trust in the suite and might not even reproduce the failure
reliably in CI). This was caught by code review reasoning about lock scope, not by running
anything -- noted here explicitly rather than pretending a test covers it. If this class of bug
recurs, the tool to reach for is a model checker (e.g. `loom`) over a hand-rolled timing test.

**How to apply**: any future function that needs a consistent view across `state.left` and
`state.right` together must acquire both guards before reading either side, the same way. A
single `state.left.lock()...` expression is fine when only one side is touched (see `apply_edit`,
`save_file`), but never when a value derived from `left` and a value derived from `right` need to
describe the *same instant*.

## 2026-08-18 — Unsaved edits are silently discarded on window close; no confirmation guard yet

**Gap, not an oversight**: M2 makes both panes editable and tracks a dirty flag per side, but
nothing consults `dirtyLeft`/`dirtyRight` when the window closes -- a user can edit, close the
window, and lose the changes with no prompt. This is a real data-loss gap in shipped M2 behavior,
being written down explicitly rather than left as an unremarked hole (same treatment as the
Off/Word/Character mockup gap above).

**Why not fixed now**: a close-confirmation dialog is naturally part of M4's session shell (the
unified window that owns app-level lifecycle, settings, and multi-file state) -- M2 has no
"session" concept yet, just a single always-open file pair with no window-management layer to
hook a beforeunload-style guard into in a way that wouldn't be thrown away when M4 replaces this
window structure anyway.

**How to apply**: when M4's session shell is built, add a close guard that checks `dirtyLeft`/
`dirtyRight` (or their session-shell equivalents) before allowing the window to close, prompting
to save/discard/cancel. Until then, do not represent M2 as data-loss-safe -- users editing real
files should be told to save explicitly before closing.

**Extended for M3**: opening a directory-scan row now makes re-running the file-pair view a
real, reachable, in-process flow for the first time (`runRealFiles` previously ran exactly once
per launch) -- this reintroduces the same unsaved-changes-discarded risk on two new paths
("open another row", "back to directory list"), and this time it's cheap enough to actually
fix rather than defer: `confirmDiscardIfDirty()` (a `window.confirm` guard) blocks both. The
*window-close* case above is still open and still deferred to M4; this only covers navigation
within the app while it stays running.

## 2026-08-18 — M3's mtime fast path: a real, observed false-Same, not a hypothetical risk

**What happened**: while manually verifying the directory-compare view under Xvfb, two files
created moments apart by back-to-back shell `echo` commands (no delay between them) -- one
containing `"old\n"`, the other `"new\n"`, same byte length -- were reported as `Same`. Checked
directly: both files had byte-identical size (4) and identical mtime down to the timestamp
`dirwalk` actually compares. This is `classify()`'s documented size+mtime fast path (see the
dirwalk commit and its doc comments) doing exactly what it says it can do, caught in the act
rather than only reasoned about in the abstract.

**Why this is real, not a sandbox artifact**: a separate direct check (write a file, sleep 50ms,
write another, compare `st_mtime_ns`) showed this filesystem *does* resolve mtimes with real
sub-second precision -- a 50ms gap was clearly visible. The two `echo`-generated files landed on
identical timestamps because there was *no* gap: two sequential, non-sleeping writes can land
within whatever interval the OS's mtime clock source actually updates at, which is not
necessarily as fine as the timestamp's nominal resolution. Any real workflow that touches paired
files in a tight loop -- a build script, a test-fixture generator, a fast bulk copy -- can
reproduce this, not just a synthetic demo.

**Decision: ship the heuristic anyway, as already planned, but treat this as confirmation, not
just a caveat.** The tradeoff was decided and endorsed before implementation (see the dirwalk
commit): always content-comparing on a size match is correct but reads every same-sized file on
every scan, which is what the `≤1s`/50k-file target is written against; the mtime fast path is
what makes that target reachable at all (see the fixture-generator commit's measurement -- with
the fast path defeated, the same 50k-file scan went from ~260ms to ~11s). A tool that's fast but
occasionally reports a real difference as unchanged is still more useful than one that's
unusably slow on large trees; rsync's own default "quick check" makes the identical bet.

**What this means in practice, stated plainly**: `diffgrid DIR1 DIR2`'s directory list can say
`Same` for a file pair that actually differs, with no visual distinction from a genuinely
identical pair. Opening the row and viewing the real diff is unaffected (`open_file_pair` always
does a real line-by-line diff regardless of what the directory scan's heuristic concluded) --
the risk is purely that a user might never click into a file the summary told them was unchanged.

**How to apply**: don't "fix" this by always content-comparing on a size match -- that trades
away the exact performance property that was measured and is the point of the tiered design. If
this ever needs to be more trustworthy than the current tradeoff allows, the right lever is a
user-facing "thorough compare" mode (always hash/byte-compare, accept the slower scan) offered
as an *option* alongside the fast default, not a silent change to what the default does.

## 2026-08-18 — M3 ships a flat, sorted results table; a real collapsible tree is M4's job

**Decision**: `diffgrid DIR1 DIR2`'s results view is a flat table of every `DirEntry` (one row
per file/directory, full relative path as the label), not a collapsible/expandable tree widget
with per-directory expand/collapse state.

**Why**: `docs/PLAN.md` describes M3's scan mechanism as walking a "recursive tree," which is
about how the *comparison* is computed, not a commitment to a specific results UI -- and
`docs/PLAN.md`'s own M4 milestone explicitly owns "sidebar tree + tabs + toolbar... wrapping
M1-M3 as one session," meaning a real tree widget is already scoped work for later, not an
M3 gap. Building a second, throwaway tree-rendering implementation now (only to likely replace
it when M4's sidebar tree arrives) would be wasted work in the same shape as the toolbar-vs-
gutter-buttons call in M2's DECISIONS entry -- a real, complete, usable version of the required
*capability* (compare two directory trees, see what differs, open a file) shipped now, with the
nicer presentation layered on later rather than gating the milestone.

**How to apply**: when M4's sidebar tree is built, it should consume the same `DirEntry` list
`scan_dirs` already streams (path + status + size, full relative path as the join key) --
grouping it into a tree is a pure frontend transform over data this milestone already produces
correctly, not a scan-logic change.

**Correction (same day, caught by advisor review before declaring M3 done)**: this entry's title
claimed "sorted" from the start, but the first cut of `visibleDirEntries` only filtered
(hide-identical) -- rows displayed in scan arrival order, and the `LeftOnly` tail specifically was
`HashMap` iteration order, not even deterministic run to run. Fixed by extracting the filter+sort
into `src/lib/dirView.ts` (`visibleDirEntries(entries, hideIdentical)`, sorted by path,
unit-tested in `dirView.test.ts`) and having the component call it inside its `$derived`. Recorded
here rather than silently editing the title so the gap-then-fix is visible, matching how the mtime
false-Same entry below documents an observed instance rather than just the abstract risk.

## 2026-08-18 — Cancel is correct end-to-end but not guaranteed *responsive* on very large trees; shipping anyway, scope is the documented 50k target

**Decision**: ship M3's cancel mechanism as-is (cooperative `AtomicBool` checked once per entry
in both scan phases, `cancel_scan` IPC command, per-scan-generation `Arc` in `ScanState`) plus one
real mitigation (coalesce `Channel` batches into at most one `dirEntries` update per animation
frame, in `runDirCompare`), rather than chasing full responsiveness at arbitrary scale.

**Why**: an advisor review flagged that Cancel had only ever been exercised at the unit level
(`cancel_scan_impl` called directly) and never through a real in-flight `scan_dirs` IPC call --
the exact path that matters. Verifying it under Xvfb against a synthetic 800k-file tree (16x the
PLAN.md 50k target, built specifically to widen the cancel-click window) surfaced a real,
reproducible finding: with tens of thousands of rows already streamed into the unvirtualized
results table, a `Cancel scan` click frequently never reached the Rust backend at all (confirmed
via a temporary `eprintln!` in `cancel_scan` -- 0 hits across many repeated clicks while the count
kept climbing). That specific finding is solid: it was reproduced from a genuinely unfixed
baseline build with direct backend-side evidence, not inferred from the frontend alone.

What's *not* solid is any claim about which of the two candidate mitigations tried --
coalescing frontend updates via `requestAnimationFrame`, and raising `BATCH_SIZE` 256 -> 4096 to
cut IPC round-trips 16x -- actually helped. Both were "tested" by repeating a fixed click sequence
and comparing how far the scan got before one landed, but that comparison turned out to be
confounded by something only noticed afterward: `dirwalk::scan`'s two-phase design (this crate's
own doc comment on `scan`) means phase 1 -- walking the entire left tree into a `HashMap` --
streams *zero* rows and drives *zero* table re-renders, so a click's odds of landing depend
heavily on whether it happens to fall inside phase 1 (uncontended, easy) or phase 2 (contended,
hard) rather than on anything either mitigation changed. Re-running the same "baseline" build
(rAF fix stashed out) produced a `0 entries found before stopping` result on the very trial meant
to establish a fixed-free control -- i.e. an easy landing on the unfixed build, which by itself
would (wrongly) look like proof the fix doesn't matter. Rather than run enough trials to average
out both that confound and Xvfb/`xdotool`'s own input-delivery jitter (uncertain how many that
would take, and this is a fixup pass, not a perf investigation), `BATCH_SIZE` was reverted to its
already-measured-and-documented value of 256 (no evidence justified changing it), and the
`requestAnimationFrame` coalescing was kept on its own merits -- it is a strict reduction in
redundant work with no downside, regardless of whether it moves the click-landing needle -- rather
than being credited with fixing the large-scale case. Three probes in (eprintln evidence, rAF
timing, batch size) without a clean isolated cause is the "stop and question the architecture"
signal, so the scope was re-anchored instead to what M3 actually promises: at the documented 50k
target, the whole scan finishes in ~260ms (measured, see the fixture-generator commit), well under
any plausible click reaction time, making click-landing-probability irrelevant there. What *is*
verified correct at every scale tried, independent of all the above: the cancel mechanism itself --
a click that does land (early or late) always produces the right result (`"scan cancelled — N
entries found before stopping"` with the right N, `dirScanOutcome.cancelled === true`).

**How to apply**: this is a real, documented scope boundary, not a silently-accepted bug -- if a
future milestone commits to comparing very large real-world trees (`node_modules`, monorepos,
vendor directories) as a stated requirement, resist repeating this session's mistake of comparing
"before" and "after" builds without controlling for the two-phase scan's phase boundary and without
enough trials to average out synthetic-input jitter. The most promising untried lever is still
virtualizing the results table (bounding DOM node count regardless of entry count), since it's the
one variable that would remove the suspected dominant cost (full keyed-`{#each}` reorders) rather
than just reducing how often it's triggered -- but treat that as a hypothesis to test properly, not
a conclusion to build on. See PLATFORM_NOTES.md for the Xvfb/no-WM caveat on these measurements.

## 2026-08-18 — M4: multiple open-file tabs, keyed by a frontend-generated `TabId`

**Decision**: M2's `SessionState` (one `Mutex<Option<EditBuffer>>` per side, exactly one open
pair) becomes `SessionState { tabs: Mutex<HashMap<TabId, TabBuffers>> }`, where `TabBuffers`
holds *both* sides of one tab under a single lock. Every edit-pipeline command (`open_file_pair`,
`apply_edit`, `redo_diff`, `save_file`) gains a `tab_id: String` parameter; a new `close_tab`
command frees a tab's buffers. The frontend mirrors this with a `tabs: FileTab[]` array (dirty
flags, hunk state, minimap geometry -- plain Svelte-reactive data) plus a separate, deliberately
non-reactive `tabRuntimes: Map<string, TabRuntime>` holding the live `EditorView` instances and
edit-queue promises.

**Why**: docs/PLAN.md's M4 description ("wrapping M1-M3 as one session") and the `session` crate's
own module-boundary doc ("open-file edit buffers" plural) both call for more than one open file
pair. Keying by `TabId` rather than replacing a single slot is the natural generalization -- and
turned out to be a strict simplification on the Rust side too: with both sides of a tab under one
map-entry lock, `redo_diff_impl` is trivially consistent (impossible to read a left snapshot from
one instant and a right from another), removing the "lock both, in a fixed order" discipline the
old dual-`Mutex` shape needed specifically to avoid that hazard. Keeping `EditorView`s out of
Svelte's `$state` is a direct application of docs/PLAN.md §1's own stated principle ("mixing two
reactive systems over the same hot-path DOM is asking for dropped frames") -- a CM6 view has no
business being deep-proxied.

**How to apply**: any future per-tab feature (the settings resolved-per-tab override mentioned in
docs/PLAN.md §5, still to come) should add fields to `FileTab`, not introduce a second id scheme.

## 2026-08-18 — M4 unifies the M3 flat table and the M1/M2 file view into one persistent layout

**Decision**: a directory-rooted session shows a sidebar (root paths + a changed-files list) and
the main tab/toolbar/panes area *simultaneously*, per docs/UI/ui-01.png's mockup -- not M3's
full-page table with a "Back to directory list" button toggling between two mutually-exclusive
views. Opening a row opens (or focuses, if already open) a tab; the sidebar never disappears.
`mode` collapses M3's separate `"files"`/`"dirs"` states into one `"session"` state, with a new
`sessionKind: "file" | "dir" | null` distinguishing whether a sidebar applies at all (a bare
`diffgrid FILE1 FILE2` invocation gets no sidebar, just its one tab).

**Why**: building the M3 toggle-based layout as an intermediate step and then immediately
replacing it with the mockup's unified layout in the very next milestone would be pure throwaway
work -- the target layout was already known from the mockup before any M4 code was written, so
there was no reason to build the thing being replaced.

**A related simplification**: a bare two-file CLI invocation still opens through the same
`openFileTab`/tab-bar machinery a directory session's rows use (one auto-opened tab, tab bar
shown even for a single tab) rather than keeping M1/M2's chrome-less single-pane code path alive
in parallel. A one-tab bar is harmless, and maintaining two separate rendering paths for
"one file pair" (with vs. without a tab bar) for a cosmetic difference wasn't worth the
duplication.

**Sidebar is a compact path + status-sigil list, not M3's four-column table**: a 260px sidebar
can't fit path/status/size-left/size-right as separate columns. Status is conveyed by a single
sigil character (`~`/`+`/`-`/`!`) plus the existing color-coding, matching real tools' sidebar
conventions (git status characters, VS Code's Source Control view) rather than a wordy column
this width can't accommodate. Per-entry size was originally going to be shown via a hover
tooltip, but that turned out to trigger a real WebKitGTK rendering bug (see the entry right
below) and was dropped rather than replaced with something else. `DirEntry.sizeLeft`/`sizeRight`
are unused by the UI right now as a result -- the data isn't gone, just not surfaced; a future
affordance (a details pane, a status-bar readout on selection) can pick it back up without a
scan-side change.

**Sidebar tree note**: this remains a flat, sorted list (M3's `dirView.ts`), not yet a real
collapsible directory tree -- M3's own DECISIONS.md entry deferred that to M4 "when built"; it
was not built in this pass either. Tracked as its own open task (upgrade the sidebar to a real
tree), not silently dropped.

## 2026-08-18 — WebKitGTK native `title` tooltips render as an unstyled black box under this Xvfb sandbox

**Decision**: dropped the sidebar row's `title` attribute (a hover tooltip showing left/right
file size) rather than debugging WebKitGTK's native tooltip rendering further.

**Why**: manual Xvfb verification of the new sidebar surfaced a real, 100%-reproducible visual
bug -- a solid black rectangle appeared below the changed-files list every time a row was clicked
(the `xdotool`-driven mouse stayed stationary over the row afterward, which is exactly a tooltip
trigger condition). Ruled out several structural hypotheses first (an unstyled `overflow:auto`
container's default background, a WebKit `outline`-on-`table-row` rendering bug) by testing each
in isolation and watching the artifact survive both fixes unchanged; adding temporary debug
outlines around every layout element made it vanish once by coincidental timing, which briefly
looked like a false lead before a clean re-run reproduced it again 3/3 times. Removing the `title`
attribute (and only that) made it disappear cleanly across 3/3 fresh launches. This was cheap
scope to drop -- the sizeTitle tooltip was a speculative addition on top of what the sidebar
redesign required, not a stated requirement -- so no replacement UI was built for it (see the
sidebar-compaction entry above for where that information could resurface later).

**How to apply**: don't add native `title` attributes to interactive/clickable elements in this
app without checking they render correctly under Xvfb first -- this specific failure mode (a
filled block instead of rendered text) is plausibly tied to this sandbox's GPU-less/software
WebKitGTK rendering path (see PLATFORM_NOTES.md's existing "Rendering engine differences not yet
characterized at all" entry) and may simply not reproduce on the real macOS/WKWebView target, but
that hasn't been verified either way -- treat any future native tooltip as needing the same
Xvfb-first check this one got, until macOS says otherwise.

## 2026-08-18 — M4 settings: global-only fields vs. per-tab override, and where each setting's logic lives

**Decision**: `session::Settings` (ignoreWhitespace, ignoreCase, collapseContextLines,
intraLineMode) persists to `<app-config-dir>/settings.json`, resolved via a Tauri command in the
`app` crate but with the actual load/save/default logic living in `session` per docs/PLAN.md §5's
module boundary -- `app`'s own Cargo.toml describes it as "command/event wiring only... the only
crate allowed to depend on `tauri`", so the path lookup (`app.path().app_config_dir()`, which
needs a live `AppHandle`) has to stay there, but nothing else does. Only `ignoreWhitespace`/
`ignoreCase` get a per-tab override (the existing toolbar checkboxes, seeded from `Settings` when
a tab opens, never written back) -- `collapseContextLines`/`intraLineMode` apply uniformly to
every tab, matching `docs/UI/ui-01.png`'s toolbar (only whitespace/case appear as quick-toggles)
and `ui-02.png`'s settings window (collapse-lines and highlight-mode live only there).

**Why**: the mockups themselves draw this exact line, so it's not an arbitrary scope call --
building per-tab overrides for collapse-lines/highlight-mode would be scope the approved design
never asked for, and would also require re-mounting or live-reconfiguring already-open CM6
instances for a capability nothing in the brief requests.

**How it's wired end-to-end (verified under Xvfb, not just unit-tested)**: the settings window
(a second Tauri window, `docs/UI/ui-02.png`, opened via a gear-icon button and
`open_settings_window`) persists on every change and emits a `settings-changed` app event; the
main window listens and updates its already-loaded `settings` state so a currently-open tab's
*global-only* fields (and the defaults any *new* tab will seed from) stay current without
polling. Confirmed: toggling a value in the settings window writes the real config-dir JSON file;
a fresh launch picks up the new defaults; two tabs opened from the same session have fully
independent `ignoreWhitespace` toggles (toggling one leaves the other's checkbox and diff result
untouched).

**"Off" is a frontend short-circuit, not a `diff-core`/backend mode** (this was flagged before
implementation, not discovered after): when `intraLineMode` is `"off"`, `mountFileTab` never
constructs an `intraLine` options object for `createDiffEditor` at all, so the intra-line
highlighter extension isn't wired into CM6 and no `intra_line_spans` IPC call happens per visible
`Replace` line. The Rust command still accepts `IntraLineMode::Off` and returns an empty result if
somehow called with it (defensive, not the expected path) rather than erroring -- cheap to keep,
never actually exercised by the frontend.

## 2026-08-18 — Word-mode intra-line diff: token-level histogram diff, not word-aware prefix/suffix trim

**Decision**: `diff_core::intra_line_spans_word_mode` tokenizes each line into maximal
word/whitespace/other-character-class runs (UTF-16-unit-based, consistent with the character-mode
path) and runs `imara_diff`'s histogram algorithm over those tokens -- the same algorithm
`diff_lines` already uses at line granularity, just re-purposed one level down -- rather than
extending the existing prefix/suffix-trim approach to work at word boundaries.

**Why**: prefix/suffix trimming fundamentally cannot express "two separate changed words with
unchanged words between them" as two small spans; it can only ever produce one span running from
the first difference to the last, which for a line with two separate edits far apart *is* word
mode's whole reason to exist per M2's original DECISIONS.md deferral ("word-boundary-aware
diffing, not raw UTF-16-unit trimming... treat it as new work"). `imara_diff`'s token interner
already supports custom token types; a hand-written `Eq`/`Hash` on the token type is what applies
ignore-whitespace/ignore-case at comparison time (any whitespace-class token equals any other
under ignore-whitespace; ASCII case-fold equality under ignore-case) while keeping raw UTF-16
spans for the final offsets -- consistent with how `common_prefix_len_raw`/`eq_unit` already
handle those same two options for character mode.

**Verified, not just unit-tested**: mutation-tested the token-range-to-UTF16-offset conversion
(swapped a `.0`/`.1` field access, confirmed 3 of the new tests fail for the right reason, then
reverted) and visually confirmed under Xvfb that a line with two separate word changes highlights
as two disjoint spans per side, not one span covering the whole distance between them -- the exact
property character mode can't provide, now visibly true in the running app.

**How to apply**: non-ASCII UTF-16 units (accented Latin, CJK, emoji, astral surrogate halves) are
all classified as "word" characters rather than falling through one-unit-at-a-time -- deliberately
coarser than true Unicode word-segmentation (UAX #29), chosen so word mode doesn't degenerate to
character mode for non-Latin text. A consequence worth knowing before "fixing" it: an emoji
directly adjacent to letters with no separator (e.g. "😀bc") merges into *one* token with them,
not two -- exercising this required correcting a test whose own expectation didn't match the
tokenizer's deliberate design, not a bug in the tokenizer itself.

## 2026-08-18 — M4 sidebar: a real nested/collapsible directory tree, not the flat list M3 shipped

**Decision**: the sidebar's CHANGED FILES panel now renders a real nested tree (`buildDirTree` /
`pruneDirTree` / `flattenDirTree` in `src/lib/dirView.ts`) with per-folder expand/collapse state
(`collapsedDirPaths`, a `Set<string>` of collapsed folder paths, default empty = all expanded),
replacing the flat sorted-by-path table M3 shipped and M3's own DECISIONS.md entry explicitly
deferred this work to.

**Why now, when the actual mockup (`docs/UI/ui-01.png`) shows a flat list**: this milestone's own
scoping question surfaced a real conflict between two sources that both matter here --
`docs/PLAN.md` names the M4 deliverable "sidebar **tree**," and M3's DECISIONS.md entry commits
in its own words to "a collapsible/expandable directory tree with per-directory expand/collapse
state" being M4's job specifically. The mockup screenshot, by contrast, only ever shows a small,
shallow fixture (7 changed files, 2 levels deep) where a flat list and a tree would render
identically -- it's not strong evidence the design *intends* a flat list at real scale, just that
the fixture never exercised the difference. Asked directly, the call was to honor the two written
commitments (PLAN.md's wording, M3's own deferral note) over an ambiguous mockup rather than
silently reinterpret both away from what they say.

**Implementation shape**: `buildDirTree` groups the flat `DirEntry[]` `scan_dirs` already streams
by splitting `path` on `/` — a pure frontend transform, exactly as M3's DECISIONS.md "How to
apply" note specified, no scan-logic change. A folder is synthesized (`isDir: true, entry: null`)
wherever a path implies an intermediate directory that was never itself scanned as its own
`DirEntry`. `pruneDirTree` keeps a folder whenever *any* descendant survives `hideIdentical`, even
if the folder itself is `same` — otherwise an unmodified folder containing one modified file would
vanish along with its only visible child. `flattenDirTree` is a depth-first walk producing the
row list actually rendered, skipping a folder's children when its path is in `collapsedPaths`.

**What's proven vs. not**: unit-tested (15 new tests in `dirView.test.ts`) — tree grouping, folder
synthesis, alphabetical sort per level, prune-keeps-ancestor / prune-drops-empty-folder, collapse
suppressing children, and depth/hasChildren row metadata. Visually verified under Xvfb against a
real synthetic tree (`src/lib/{a,b}.ts`, `src/ui/pane.tsx`, `README.md`, 3 of 4 files modified):
correct indentation and disclosure triangles, `b.ts` (unchanged) correctly hidden under
`hideIdentical` while its unmodified parent folders `src/`/`lib/` correctly stay visible as
ancestors of modified descendants, clicking a folder row collapses/re-expands it and the
CHANGED FILES count updates to only the still-visible file rows, and the previously-missing
selected-row highlight (light blue) now shows correctly on an open tab's sidebar row.

**Not built**: a changed-line-count column per file (mockup shows one, e.g. "compare.ts ... 20")
— `DirEntry` only carries `sizeLeft`/`sizeRight` (bytes), not a diff-derived line count, so this
isn't a pure frontend transform like the tree grouping was; it would need either a cheap
line-count-only diff per changed file during the scan (a real scan-side cost this milestone hasn't
measured) or computing it lazily on first sidebar render. Left as a documented gap rather than
silently building a wrong/fake number.

## 2026-08-18 — Fixed: CHANGED FILES count undercounted when a folder was collapsed

**Decision**: the sidebar's "CHANGED FILES · N" header count is now computed from
`countDirTreeFiles` over the pruned tree directly, not from `dirTreeRows.filter(!isDir).length`.

**Why**: caught by advisor review, not by my own testing, despite having personally collapsed a
folder and screenshotted the sidebar minutes earlier in the same session without noticing —
`dirTreeRows` is the *rendered* row list, which `flattenDirTree` deliberately omits a collapsed
folder's children from; counting rows meant a collapsed folder's changed files silently
disappeared from the header count along with their rows, even though they're still real changes
the scan found. Reproduced concretely: 2 changed files under `src/lib/`, header correctly read
"· 2" while expanded, dropped to "· 1" the moment `src/` was collapsed. `countDirTreeFiles`
(new in `dirView.ts`, unit-tested) recurses the tree unconditionally, independent of
`collapsedPaths`, so the count and the collapse/expand UI state are no longer coupled.
