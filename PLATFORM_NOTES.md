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

**Idle memory instability** (see `docs/PROFILING.md`'s correction). Measured on Linux/WebKitGTK
to vary ~5.6x (178MB → 995MB) with ambient system memory pressure alone, no code change.
**Verify on macOS**: don't assume WKWebView is stable just because it's a different engine —
check whether the same memory-pressure sensitivity exists there. If it does, any single idle-
memory number reported without the accompanying `os.freemem()`/`os.totalmem()) line from the
harness should be treated as meaningless.

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
