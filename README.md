# diffgrid

A from-scratch Meld replacement — macOS first, Linux later. See `docs/PLAN.md` for the full
architecture, module boundaries, and milestone breakdown. `docs/M0-RESULTS.md` /
`docs/PROFILING.md` cover the feasibility spike and the performance investigation that fixed the
scroll/paint regression found there (now shipped — see the `perf:` commit in `git log`).

**Status: M3 complete** — two-way file diff with editing (`diffgrid FILE1 FILE2`) and directory
comparison (`diffgrid DIR1 DIR2`). Files: encoding/line-ending/binary detection, histogram line
diff, lazy intra-line highlighting, live whitespace/case-ignore toggles, collapsed unchanged
regions, hunk navigation, a minimap overview strip, both panes editable with debounced live
re-diff, per-side save (encoding/line-ending-preserving), and per-hunk apply/revert. Directories:
recursive gitignore-aware cancellable scan, tiered size/mtime/content compare, a flat streamed
results table with a hide-identical toggle, opening a row reuses the file-pair view. No session
shell (M4) yet.

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
target/release/app FILE1 FILE2   # two-way file diff
target/release/app DIR1 DIR2     # directory comparison
```

**Files**: opens a real two-way diff — encoding/line-ending detection, histogram line diff, lazy
intra-line highlighting, live whitespace/case-ignore toggles, collapsed unchanged regions,
Prev/Next-diff navigation (buttons or Alt+Up/Alt+Down), and a minimap overview strip. Binary
files are refused with an error rather than diffed. Both panes are editable: typing debounces
(~300ms) into a live re-diff, "Copy to left"/"Copy to right" apply the currently-navigated hunk's
content to the other side, and "Save left"/"Save right" (or Cmd/Ctrl+S while a pane is focused)
write back to the original file, preserving its original encoding and line-ending style.

**Directories**: recursively scans and compares two directory trees (`.gitignore`-aware,
cancellable), streaming results into a flat table as they're found — path, status
(same/modified/leftOnly/rightOnly/typeConflict), and size on each side. "Hide identical" filters
the already-fetched list instantly, no re-scan. Clicking a row for a file present and unchanged
or modified on both sides opens it in the file-pair view above; "Back to directory list" returns
without re-scanning.

There's no file picker yet and no unified session across multiple open pairs — that's M4's
session shell.

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
