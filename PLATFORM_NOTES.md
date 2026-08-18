# Platform notes

Every platform-conditional code path in this repo, and what specifically needs re-verifying on
macOS before trusting it there. Target platforms: Linux (this sandbox) and macOS arm64.

## Code paths that actually branch on platform

**`bench/m0-spike.mjs`, `ensureDisplay()`** — the only `process.platform` check in the codebase.
On Linux without a `DISPLAY` env var, the script launches its own Xvfb; on any other platform
(macOS included) it assumes a real display exists and does nothing. **Verify on macOS**: just
confirm the harness runs without needing this branch at all — no action should be required.

**`bench/m0-spike.mjs`, `processTable()`/`descendants()`** — uses `ps -A -o pid=,ppid=,rss=`,
which both GNU ps (Linux) and BSD ps (macOS) document supporting. **Verify on macOS**: this has
only been run against GNU ps in this sandbox. Confirm the column parsing produces sane numbers
(a quick sanity check: `ps -A -o pid=,ppid=,rss=` from a Terminal and compare to what the harness
reports) — BSD ps's exact whitespace/column formatting hasn't been checked directly.

## Not a platform branch, but platform-sensitive — re-verify anyway

**`src/lib/diffView.ts`, `LINE_HEIGHT_PX = 18`.** This was measured empirically against what
this Linux sandbox's WebKitGTK actually renders for the font stack `ui-monospace, Menlo,
monospace` at `font-size: 13px` — but "Menlo" is a real, available font on macOS (Apple's own
monospace font) while it's just a fallback name on Linux, where the browser substitutes whatever
monospace font is installed. **The 18px measurement may not carry over to macOS**, where Menlo
will actually be selected. Before relying on this value there: measure `.cm-line`'s real
rendered height in the WKWebView (e.g. `getBoundingClientRect().height`) at the same font-size,
and update the constant if it differs — it's pinned explicitly in the theme specifically so
estimate and reality can't silently drift apart (see `docs/PROFILING.md`), but that guarantee
only holds if the constant is re-measured per platform/font-stack, not assumed.

**`--features tauri/custom-protocol` requirement** (see `README.md`). This is a general Tauri
behavior when bypassing the Tauri CLI, not Linux-specific, but it has only actually been
exercised on Linux in this repo's history. **Verify on macOS**: confirm `cargo build --release
-p app --features tauri/custom-protocol` produces a binary that loads correctly (not "Connection
refused"), the same way it was diagnosed here.

**Idle memory instability, and the 300MB target is unverified** (see `docs/PROFILING.md`'s
correction and `docs/M0-RESULTS.md` §5). Measured on Linux/WebKitGTK to vary ~5.6x (178MB →
995MB) on the 100k fixture with ambient system memory pressure alone, no code change; a third
reading on the smaller 10k fixture (the one the 300MB target was actually written against) came
in at 687MB — over the threshold, and not obviously explained by fixture size given the spread
already seen at fixed fixture size. **Verify on macOS**: don't assume WKWebView is stable just
because it's a different engine — check whether the same memory-pressure sensitivity exists
there, and re-run the 10k-fixture idle-memory check specifically against the 300MB PLAN.md
criterion. If WKWebView shows similar instability, any single idle-memory number reported without
the accompanying `os.freemem()`/`os.totalmem()` line from the harness should be treated as
meaningless.

## Sandbox environment setup (not code — won't apply on a real macOS machine)

This Linux sandbox required manual one-time setup that has no macOS equivalent and is not part
of the shipped repo: installing `libwebkit2gtk-4.1-dev`/GTK build deps via `apt`, installing and
configuring `Xvfb` (including fixing `/tmp/.X11-unix` ownership, which needed root), and
installing `imagemagick` for screenshot-based verification during the profiling investigation.
None of this is platform-conditional *code* — it's just absent on macOS, where Tauri uses the
system-provided WKWebView and a real display is already present.

## Rendering engine differences not yet characterized at all

Every performance number in `docs/M0-RESULTS.md` and `docs/PROFILING.md` was measured against
**WebKitGTK running under Xvfb with no GPU acceleration** (`libEGL warning: DRI3 error`). macOS's
WKWebView is hardware-accelerated by default. This affects every fps/paint-time number in this
repo's history, not just one metric — treat all of them as directional until re-measured on the
actual M4 Pro target, per the M0 gate's own stated next step (still open, not resolved by this
pass).

## M1 features verified only on Linux/WebKitGTK/Xvfb — no new platform-conditional code, but check visually

M1 (real two-way file diff) added no new `process.platform`/`cfg(target_os)` branches — the
`ensureDisplay()` one above is still the only one in the repo. Everything below was built using
the same CM6/WebKitGTK stack the M0 fixes already depend on, so the same caveats apply, plus a
few specific to the new features:

- **Collapsed-region and minimap rendering** were only screenshot-verified under Xvfb. Check on
  macOS that the `.diff-collapse` placeholder and the minimap's colored segments/viewport
  rectangle render with the same proportions — nothing here is Linux-specific by construction,
  but nothing here has been *seen* rendered by WKWebView either.
- **Alt+Up/Alt+Down hunk-navigation shortcuts** use `KeyboardEvent.altKey`, which should map to
  the Option key on macOS per the DOM spec — not verified on real macOS hardware. If it doesn't,
  the Prev/Next diff buttons are an unaffected fallback (same `goToHunk` call).
- **The intra-line-diff and collapse A/B probes** (`bench/m0-spike.mjs --collapse-equal`, the
  disable-padding one from M0) are Linux-verified only; re-run both on macOS if the padding/
  collapse mechanism is ever revisited, since the "block-level decorations trigger a slow layout
  mode" finding they're built on was itself only characterized against WebKitGTK.

## M2 features (editing) verified only on Linux/WebKitGTK/Xvfb

No new `process.platform`/`cfg(target_os)` branches here either. Manually verified under Xvfb
with `xdotool`: typing an edit and seeing the debounced re-diff land correctly (including a
line-count-changing insert, which re-triggers alignment padding); saving a CRLF-encoded file and
confirming the on-disk bytes both contain the edit and keep CRLF; per-hunk copy for a `Replace`
hunk and an `Insert` hunk, including the dirty-flag and minimap updates. None of this exercises
anything WebKitGTK-specific by construction, but none of it has been *seen* running under WKWebView
either — same caveat as M1's collapse/minimap rendering above.

**Cmd/Ctrl+S** checks `e.ctrlKey || e.metaKey`, matching the same DOM-level Alt-key caveat M1's
hunk-navigation shortcuts already carry: `metaKey` should map to Cmd on macOS per spec, not
verified on real hardware.

**Unedited-save byte-identity is the one correctness property most worth re-checking on macOS**:
`EditBuffer::to_bytes()` short-circuits to the exact original bytes when the buffer isn't dirty,
which is what makes a UTF-8-BOM or `Mixed`-line-ending file round-trip losslessly through an
open→save with no edits. This is pure Rust logic with no platform dependency, but it's the kind of
thing worth a real spot-check with a real macOS-authored file (e.g. one saved by Xcode or another
Mac-native editor) rather than assuming the Linux-generated test fixtures are representative.

## M3 (directory comparison): one real cross-platform correctness risk, not just an unverified caveat

**APFS filename normalization could silently split one logical file into `LeftOnly` +
`RightOnly`, and this cannot be exercised on Linux at all.** `dirwalk::relative_path` uses each
tree's path bytes directly as the join key between the two sides -- correct on Linux, where the
filesystem stores whatever byte sequence was given to it. APFS (macOS's default filesystem)
normalizes filenames containing decomposable Unicode (accented characters, some CJK) to NFD form
on disk, regardless of what form was used to create the file. If one side of a compared pair was
populated by something that writes NFC-normalized names (common: files that crossed through a
non-Mac tool, a git checkout with `core.precomposeunicode` off, an archive extracted by a
non-Apple tool) and the other side has the same logical filename in NFD form, `relative_path`
would produce two *different* strings for what a user considers the same file -- reported as a
`LeftOnly`/`RightOnly` pair instead of a match. This is not a hypothetical extrapolation from a
known OS difference; it's the exact failure mode Unicode-normalization-mismatch bugs take in
every cross-platform tool that joins paths by raw bytes. **Verify on macOS**: create a paired
directory tree containing at least one filename with a precomposed accented character (e.g.
`café.txt`) on each side via different tools/methods, and confirm the scan reports it as `Same`/
`Modified` rather than a spurious `LeftOnly` + `RightOnly` pair. If it reproduces, the fix is
Unicode-normalizing both sides' relative paths to the same form (NFC, matching what most other
tools assume) before using them as the join key -- not comparing raw bytes.

**Everything else in M3 is unverified-but-not-expected-to-differ**, same caveat class as M1/M2's
entries above: `ignore::WalkBuilder`'s `follow_links(false)`/`hidden(false)`/`require_git(false)`
settings and the byte-comparison content-equality tier are all plain filesystem operations with
no Linux-specific code path, but none of it has been *run* against APFS. The 50k-file first-rows
measurement (~130ms first batch, ~260ms full scan; see the fixture-generator commit) was taken
entirely on this sandbox's filesystem -- worth flagging specifically because this same sandbox's
raw filesystem syscall overhead already proved unrepresentative once during this exact
investigation (fixture *generation* itself, ~50k small file writes, took 14-16s of wall clock,
almost all in `sys` time -- ordinary local disk I/O on real hardware shouldn't be anywhere near
that slow for the same operation). Re-measure the scan timing on the real macOS target before
trusting the ~130ms/~260ms numbers as anything but directional.

**Broken symlinks were verified, not just reasoned about**: a symlink whose target doesn't exist,
present on only one side, correctly shows up as `LeftOnly`/`RightOnly` rather than silently
vanishing -- confirmed by reading `ignore` crate's source (`follow_links(false)` makes
`DirEntry::metadata()` call `fs::symlink_metadata`, which stats the link itself and succeeds
regardless of whether the target exists) and locked in by
`a_broken_symlink_present_on_only_one_side_is_reported_as_left_only` in `crates/dirwalk/src/
lib.rs`. Unix-only (`#[cfg(unix)]`, like the other symlink tests), so this is untested on Windows,
but that's not a target platform for this project.

**Cancel-click responsiveness under Xvfb/no-window-manager/`xdotool` is its own unverified layer,
separate from the underlying cancel *mechanism*, which is verified correct.** Reproducing and
diagnosing a real advisor-flagged gap (see DECISIONS.md, "Cancel is correct end-to-end but not
guaranteed responsive on very large trees") required synthesizing X11 clicks against a running
Tauri/WebKitGTK app with no window manager present -- `xdotool windowactivate` doesn't work in this
setup (`_NET_ACTIVE_WINDOW` unsupported), `windowfocus`/`windowraise` were needed instead, and even
then a click landed on the first attempt only sometimes; repeated clicks over ~2s were needed to
reliably land one during a large in-flight scan. This means the specific finding "clicks often
never reached the backend during a huge scan" is entangled with *how this environment delivers
synthetic input*, not just app behavior -- a real user with a real mouse under a real compositor on
macOS may see different (better or worse) responsiveness than what was measured here. On top of
that, `dirwalk::scan`'s own two-phase design (phase 1 streams zero rows) turned out to confound
before/after comparisons of candidate fixes -- see DECISIONS.md for why neither mitigation tried
was cleanly validated or invalidated, not just reported as "didn't help." What's
platform-independent and trustworthy regardless: the cancel flag/IPC mechanism itself was confirmed
correct via a real backend round-trip (a temporary debug print showed `cancel_scan` executing while
`scan_dirs` was still in flight, and cancelling mid-scan always produced the correct `ScanOutcome`).
**Verify on macOS**: click Cancel during a large real-world directory comparison (a big
`node_modules` tree is a good candidate) and confirm it's reasonably responsive; if not, see the
DECISIONS.md entry's "How to apply" for the next lever (table virtualization).

## M4 (session shell): a real, reproducible WebKitGTK rendering bug, not just an unverified caveat

**Native `title` attributes render as a solid unstyled black rectangle instead of a tooltip with
text, at least on hover-then-click sequences under this sandbox.** Discovered while building the
M4 sidebar: a `title` attribute added to a clickable row (to show file sizes on hover) produced a
100%-reproducible black box appearing below the sidebar's list after clicking a row -- confirmed
across 6+ fresh launch-and-click cycles with the `title` attribute present, and 0/6 with it
removed. See DECISIONS.md for the elimination process (two other structural hypotheses were tested
and ruled out first). This is plausibly tied to this sandbox's GPU-less/software-rendered WebKitGTK
path (`libEGL warning: DRI3 error` at every launch -- see the existing "Rendering engine
differences not yet characterized at all" entry above), which may not reproduce on the real macOS
WKWebView target at all. **Verify on macOS**: if any future feature wants a native tooltip
(`title` attribute) on an interactive element, hover it and confirm real text renders, not a blank
or malformed box, before trusting it there -- this repo currently has zero native tooltips in use
specifically because of this finding, so there's nothing existing to spot-check, only future
additions to gate.

**The settings window (a real second Tauri window, opened via `open_settings_window`) was
verified working end-to-end under Xvfb**, including the one part that looked most likely to be
platform-fragile: SvelteKit's static-adapter SPA fallback (`ssr = false`, `fallback: "index.html"`)
correctly serves the `/settings` route when Tauri's webview navigates straight to
`tauri://localhost/settings` with no prior in-app navigation -- confirmed by screenshot, not
assumed from reading the adapter's docs. Also confirmed: the settings window is a genuine
separate OS-level window (own title bar, own `xdotool` window ID, `set_focus()` on a second
`open_settings_window` call rather than a duplicate window), and the `settings-changed` app event
correctly reaches the main window's listener. None of this exercises anything WebKitGTK-specific
by construction (window creation and app-wide events are core Tauri, not a rendering concern), but
per this document's own standing caveat about everything only having run under WebKitGTK/Xvfb so
far, a real macOS spot-check (open Settings, toggle a value, confirm the main window's next-opened
tab picks it up) is still worth doing before fully trusting it there.
