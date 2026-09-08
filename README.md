# diffgrid

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)](#prerequisites)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri-24C8DB)](https://tauri.app)

A fast, native diff and merge tool for files and directories — a from-scratch
alternative to Meld, built with a Rust core and a CodeMirror 6 frontend. Drop
it in as your `git difftool`/`git mergetool` and it just works.

## Features

- **Two-way file diff** — histogram-based line diff, lazy intra-line
  highlighting (off / word / character), whitespace and case-insensitive
  comparison, collapsible unchanged regions, and hunk-by-hunk navigation.
- **Editable panes** — both sides are live editors with debounced re-diff,
  copy-hunk-to-other-side, and per-side save that preserves the original
  encoding and line endings.
- **Directory comparison** — recursive, `.gitignore`-aware, cancellable scan
  of two directory trees with a collapsible tree view, same/modified/left-only/
  right-only status, and a "hide identical" filter. Click any file to open it
  as a diff tab in the same window.
- **Three-way merge** — a 4-pane BASE/LOCAL/REMOTE/MERGED view that
  auto-resolves non-conflicting hunks and lets you take Local/Remote/Both/Base
  per conflict, or edit the merged result directly.
- **Git integration** — works as a drop-in `git difftool` and `git mergetool`,
  including multi-file diffs, `--dir-diff`, renames, and delete/symlink
  conflicts.
- **Multi-tab sessions** — open several file pairs or directory comparisons in
  one window, each with independent state, plus a settings window for global
  preferences that persist across launches.

## Installation

### Prerequisites

Both platforms need [Rust](https://rustup.rs) (stable) and Node.js v22+.

Linux additionally needs Tauri's GTK/WebKitGTK build dependencies:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev libgtk-3-dev patchelf pkg-config
```

macOS needs the Xcode Command Line Tools (`xcode-select --install`); Tauri
uses the system WKWebView, so no other system packages are required.

### Build from source

```bash
git clone https://github.com/binara-sachin/diffgrid.git
cd diffgrid
npm install
npm run build
cargo build --release -p app --features tauri/custom-protocol
```

The resulting binary is at `target/release/app`. There are no prebuilt
binaries yet — see [Roadmap](#roadmap).

## Usage

```bash
target/release/app FILE1 FILE2                          # two-way file diff
target/release/app DIR1 DIR2                            # directory comparison
target/release/app --merge BASE LOCAL REMOTE MERGED     # three-way merge
```

### As a git difftool / mergetool

```bash
git config --global difftool.diffgrid.cmd '/path/to/target/release/app "$LOCAL" "$REMOTE"'
git config --global mergetool.diffgrid.cmd '/path/to/target/release/app --merge "$BASE" "$LOCAL" "$REMOTE" "$MERGED"'
git config --global mergetool.diffgrid.trustExitCode true

git difftool -t diffgrid
git mergetool -t diffgrid
```

`diffgrid --merge` exits `0` only once every hunk is resolved, so
`trustExitCode` can be trusted instead of falling back to git's mtime check.

There's no file picker yet, so launch arguments (or `git difftool`/
`git mergetool`) are the only way to open a session today.

## Development

```bash
npm run tauri dev      # run the app with hot reload
npm test                # frontend unit tests (vitest)
cargo test --workspace  # Rust unit tests
```

### Project layout

```
crates/diff-core/   Histogram line diff (imara-diff), no UI/Tauri dependency
crates/text-io/     Encoding/line-ending/binary detection, save-time re-encoding
crates/session/     Edit-buffer management for the edit/save pipeline
crates/dirwalk/     Cancellable two-phase directory-pair scan
crates/merge-core/  Three-way merge and conflict resolution
src-tauri/          Tauri shell: commands/events wiring only, no diff logic
src/                SvelteKit frontend (src/lib/diffView.ts wires CodeMirror 6)
bench/               Cross-platform performance benchmark harness
fixtures/gen/        Generators for synthetic diff/tree fixtures used by benchmarks
```

### Benchmarks

```bash
node bench/m0-spike.mjs 5          # cold launch, scroll fps
node bench/open-file-bench.mjs 5   # open-to-first-paint latency, idle memory
cargo bench -p diff-core -p dirwalk # algorithmic micro-benchmarks
```

## Roadmap

- [ ] Code signing / notarization and packaged installers (`.dmg`, `.deb`/AppImage)
- [ ] A file picker for launching without CLI arguments
- [ ] Windows support

## Contributing

Issues and pull requests are welcome. Please run `npm test` and
`cargo test --workspace` before opening a PR.

## License

[MIT](LICENSE)
