# diffgrid

A from-scratch Meld replacement — macOS first, Linux later. See `docs/PLAN.md` for the full
architecture, module boundaries, and milestone breakdown. `docs/M0-RESULTS.md` /
`docs/PROFILING.md` cover the feasibility spike and the performance investigation that fixed the
scroll/paint regression found there (now shipped — see the `perf:` commit in `git log`).

**Status: M5 complete** — one unified session window (`diffgrid FILE1 FILE2` or
`diffgrid DIR1 DIR2`) wrapping M1-M3 in multi-tab form, plus persisted global preferences, plus a
real three-way merge tool usable as `git mergetool -t diffgrid`. Files: encoding/line-ending/binary
detection, histogram line diff, lazy intra-line highlighting (off / word-level / character-level,
global setting), live per-tab whitespace/case-ignore toggles, collapsed unchanged regions
(context-line count configurable), hunk navigation, a minimap overview strip, both panes editable
with debounced live re-diff, per-side save (encoding/line-ending-preserving), and per-hunk
apply/revert. Directories: recursive gitignore-aware cancellable scan, tiered size/mtime/content
compare, a real collapsible/expandable sidebar tree (not a flat table) with a hide-identical
toggle, clicking a file opens it as a tab in the same window. Multiple file pairs can be open as
tabs simultaneously, each with independent edit/dirty state; a settings window (gear icon)
persists global preferences across launches. Merge: `diffgrid --merge BASE LOCAL REMOTE [MERGED]`
opens a 4-pane BASE/LOCAL/REMOTE/MERGED view, auto-resolves non-conflicting hunks, offers Take
Local/Remote/Both/Base plus direct manual editing of the merged result, and writes back to
`$MERGED` with a real process exit code (0 iff every hunk is resolved) — verified end-to-end
through a real `git mergetool -t diffgrid` invocation, not just direct invocation of the binary.
`git difftool -t diffgrid` also already works today, unmodified — confirmed via a real invocation,
not assumed.

M6 (git-integration hardening, packaging, full quality bar) is next: no `tauri-plugin-single-instance`
is wired up yet, no codesign/notarize, no bundler packaging config, and macOS has not yet been
verified at all (everything above has only run on Linux/WebKitGTK/Xvfb so far).

Stack: Rust core (histogram diff via `imara-diff`) + Tauri shell + CodeMirror 6 frontend.

## Prerequisites

**Rust** (stable, via [rustup](https://rustup.rs)) and **Node.js** (v22+) are required on both
platforms.

**Linux** additionally needs Tauri's GTK/WebKitGTK build dependencies:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev libgtk-3-dev patchelf pkg-config
```

**macOS** needs Xcode Command Line Tools (`xcode-select --install`); no other system packages
are required — Tauri uses the system WKWebView.

## Setup

```bash
npm install

# Generate the benchmark fixtures (gitignored — deterministic, regenerate on demand):
node fixtures/gen/gen-line-pair.mjs 100000 fixtures/100k-line-pair 42
node fixtures/gen/gen-line-pair.mjs 10000 fixtures/10k-line-pair 7
```

## Build

```bash
npm run build                                              # frontend -> build/
cargo build --release -p app --features tauri/custom-protocol
```

**The `--features tauri/custom-protocol` flag is required** when building with plain `cargo
build` instead of the Tauri CLI (`npm run tauri build`/`npm run tauri dev`) — without it, the
release binary tries to connect to the Vite dev server (`http://localhost:1420`) instead of
loading the embedded frontend, and fails with "Connection refused." See `docs/PROFILING.md` for
how this was diagnosed.

For day-to-day development, prefer the Tauri CLI, which handles this automatically and gives you
hot reload:

```bash
npm run tauri dev
```

## Run

```bash
target/release/app FILE1 FILE2                          # two-way file diff
target/release/app DIR1 DIR2                            # directory comparison
target/release/app --merge BASE LOCAL REMOTE MERGED     # three-way merge
```

**Files**: opens a real two-way diff — encoding/line-ending detection, histogram line diff, lazy
intra-line highlighting, live whitespace/case-ignore toggles, collapsed unchanged regions,
Prev/Next-diff navigation (buttons or Alt+Up/Alt+Down), and a minimap overview strip. Binary
files are refused with an error rather than diffed. Both panes are editable: typing debounces
(~300ms) into a live re-diff, "Copy to left"/"Copy to right" apply the currently-navigated hunk's
content to the other side, and "Save left"/"Save right" (or Cmd/Ctrl+S while a pane is focused)
write back to the original file, preserving its original encoding and line-ending style.

**Directories**: recursively scans and compares two directory trees (`.gitignore`-aware,
cancellable), streaming results into the sidebar as they're found — a real nested tree grouped by
path, with per-folder expand/collapse state, status (same/modified/leftOnly/rightOnly/
typeConflict) shown as a sigil + row color. "Hide identical" filters the already-fetched list
instantly, no re-scan (an unmodified folder stays visible if any descendant survives the filter).
Clicking a file row present and unchanged or modified on both sides opens it as a tab; clicking a
folder row toggles its expand/collapse state. Multiple tabs can be open at once, each independent.

**Settings**: the gear icon (top-right of the status bar) opens a separate settings window —
collapse-context-line count and intra-line highlight mode (off/word/character) are global,
persisted to disk (`app_config_dir()/settings.json`) and applied live to newly-opened tabs; a
tab's own whitespace/case-ignore toggles seed from the global default but are a per-tab override
that never writes back.

**Merge**: `diffgrid --merge BASE LOCAL REMOTE [MERGED]` opens a 4-pane view (BASE/LOCAL/REMOTE
read-only source panes, plus an editable MERGED pane seeded with the auto-merge result). Hunks
that only changed on one side auto-resolve; real conflicts are highlighted and need a decision —
click a hunk (in any pane) then Take Local/Remote/Both/Base, or edit the MERGED pane directly.
`MERGED` defaults to `LOCAL` when omitted (convenient for manual testing; a real `git mergetool`
invocation always passes all four). Save writes the merged text to `MERGED` and exits 0 only if
every hunk is resolved (non-zero otherwise, or on Abort) — set this up as a real git mergetool
with:

```bash
git config --global mergetool.diffgrid.cmd '/path/to/target/release/app --merge "$BASE" "$LOCAL" "$REMOTE" "$MERGED"'
git config --global mergetool.diffgrid.trustExitCode true   # otherwise git falls back to an mtime check + interactive prompt
git mergetool -t diffgrid
```

There's no file picker yet — launch args (or `git difftool`/`git mergetool`) are the only way to
open a session. `git difftool -t diffgrid` already works today (git invokes the same `FILE1 FILE2`
form as plain two-way diff, `$LOCAL`/`$REMOTE`, no app changes needed) — configure it the same way
as `mergetool` above, just with `difftool.diffgrid.cmd` and no `$BASE`/`$MERGED`. Hardening the
`difftool`/`mergetool` argument-convention edge cases (and the single-instance trap noted below) is
M6 scope.

Running the binary with **no arguments** instead launches the M0 benchmark flow: it loads the
100k-line synthetic fixture, renders the dual-pane diff, then runs a self-contained
scroll-performance benchmark and prints `DIFFGRID_READY` / `DIFFGRID_BENCH {...}` to stdout. This
is what `bench/m0-spike.mjs` below invokes — it is a measurement harness, not the real app.

## Test

```bash
npm test                       # frontend unit tests (vitest + jsdom)
cargo test --workspace         # Rust unit tests (diff-core)
```

Run both before every commit — neither is skippable per this project's git discipline.

## Benchmark

```bash
node bench/m0-spike.mjs 5
```

Spawns the release binary 5 times, measuring cold launch, in-app open-to-first-paint, idle
memory (host process + descendants, e.g. WebKit's Network/Web processes), and scroll fps. Runs
identically on Linux and macOS — see `PLATFORM_NOTES.md` for the one platform-conditional branch
(Xvfb management on Linux; macOS uses the real display).

**Idle memory is not a stable number** — see `docs/PROFILING.md`'s correction. It tracks ambient
system memory pressure (WebKitGTK/WKWebView both appear to adapt caching to available RAM), not
just the app's own footprint. The harness logs `os.freemem()`/`os.totalmem()` alongside every
run; always read the memory number together with that line, never alone.

Linux benchmark numbers in this repo's history were recorded in a sandboxed, GPU-less
environment with WebKitGTK — not representative of the macOS target. See `docs/M0-RESULTS.md`
and `PLATFORM_NOTES.md` before drawing conclusions from them.

## Project layout

```
crates/diff-core/    Rust: histogram line diff (imara-diff), no UI/Tauri dependency
crates/text-io/      Rust: encoding/line-ending/binary detection, save-time re-encoding
crates/session/      Rust: EditBuffer (ropey shadow buffer) for the edit/save pipeline
crates/dirwalk/      Rust: cancellable two-phase directory-pair scan (the ignore crate)
src-tauri/           Tauri shell: commands/events wiring only, no diff logic
src/                 SvelteKit frontend; src/lib/diffView.ts wires CodeMirror 6
fixtures/            Deterministic synthetic diff/tree fixtures (generated, not committed) +
                     the generator scripts (committed)
bench/               Cross-platform benchmark harness
docs/                Architecture plan, spike results, profiling report
DECISIONS.md         Ambiguous calls made autonomously, with rationale
PLATFORM_NOTES.md    Every platform-conditional code path and what to verify on macOS
```
