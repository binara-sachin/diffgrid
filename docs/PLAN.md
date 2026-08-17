# diffgrid — Implementation Plan

Stack: **Rust core (diff engine, directory walker, file I/O, CLI/git integration) + Tauri shell + CodeMirror 6 as a rendering surface** (Option C). Decision record and trade-off analysis against Options A/B live in the conversation that produced this plan; not repeated here.

## 1. Architecture

```
┌─────────────────────────────── Tauri process (native) ───────────────────────────────┐
│  Rust core (workspace crates)                                                         │
│   diff-core · text-io · dirwalk · session · merge-core · vcs-cli                       │
│                              │  Tauri commands (invoke) + events/Channel               │
└──────────────────────────────┼─────────────────────────────────────────────────────────┘
                               │  IPC boundary — metadata only, see §3
┌──────────────────────────────┼─────────────────────────────────────────────────────────┐
│  WebView (WKWebView)         ▼                                                         │
│   app shell (Svelte): session sidebar/tree, tabs, toolbar, settings window              │
│   diff surface (imperative, outside Svelte's reactivity): two CM6 instances per          │
│   file-diff tab, driven directly by hunk/decoration data from Rust                      │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

**Frontend framework**: Svelte for the app shell (compiles away, no VDOM tax on a target this performance-sensitive). The diff panes themselves are managed imperatively via CM6's own API, not through Svelte bindings — mixing two reactive systems over the same hot-path DOM is asking for dropped frames. This is a lower-stakes choice than the rendering stack itself; revisit if it causes friction.

## 2. Who owns the text (read this before writing any editing code)

Two editable CM6 instances plus a Rust-side buffer is two sources of truth unless one of them is designated authoritative now:

- **CM6's document is authoritative for editing.** All keystrokes, IME composition, undo/redo happen in CM6's own state. We do not fight the editor for ownership of the buffer — that's the whole reason Option C was chosen over a custom renderer.
- **Rust keeps a shadow buffer** (`ropey::Rope`, one per open file side) fed by **incremental deltas** taken from CM6's `transaction.changes` (`from`, `to`, `insertedText`) — never a full-document resend on every keystroke. `ropey` is in the dependency list *specifically* to make applying a stream of small deltas to a large buffer cheap; if we ever find ourselves passing whole-file strings on edit, the dependency has no purpose.
- **Full file text crosses the IPC boundary exactly once per file open, in one direction** (Rust → frontend, since Rust does the initial read/encoding-detection). After that: only edit deltas go frontend → Rust, and only hunk/diff *metadata* comes back Rust → frontend. A 3MB round-trip on every keystroke (measured at ~200ms for 3MB over Tauri IPC) would blow the debounce budget and the 60fps target outright — this constraint is non-negotiable, not an optimization to defer.
- **Saves go through Rust**, not the WebView's filesystem access. Rust retains the encoding, line-ending style (including mixed-CRLF/LF), and trailing-newline state captured at load time, and writes the file back consistent with the original — otherwise a save silently normalizes Latin-1→UTF-8 or CRLF→LF and the correctness tests in §7 will catch it late and expensively.

## 3. IPC contract

Nothing containing file text crosses the boundary in the diff-result direction. What does:

| Direction | Payload | Mechanism |
|---|---|---|
| FE → Rust | open file/dir pair, settings changes, edit deltas (`from`, `to`, `insertedText` as UTF-16 offsets), save request | `invoke` (JSON — small, not perf-sensitive) |
| Rust → FE | initial file text + `FileMeta` (once, per open) | `ipc::Response` raw bytes, not JSON |
| Rust → FE | `FileDiffResult`: hunks as line ranges + kind; intra-line spans as UTF-16 offset/length pairs | `invoke` return (JSON is fine — this is metadata, KB not MB, even at 100k lines, since it's line ranges, not line content) |
| Rust → FE | directory scan results | `Channel`, batched (~one flush per animation frame, not one event per file — 50k individual IPC events would itself blow the "first rows in 1s" target) |

Two boundary traps closed now rather than discovered in a fixture test:

- **Offset unit is UTF-16**, everywhere the boundary is crossed. Rust counts bytes/chars internally; CM6 counts UTF-16 code units. All offsets in `IntraLineDiff`/`EditDelta` are defined as UTF-16 code-unit offsets, converted on the Rust side. Skipping this misaligns every intra-line highlight that touches an astral character (emoji, some CJK extensions).
- **No codec/framing exists in Tauri today for the binary path** — `ipc::Response`/`Channel` give raw bytes, not batching or sequencing. We hand-roll a minimal framing (length-prefixed) for the one-time full-text transfer; this is a small, contained piece of code, not a dependency to go shopping for.

## 4. Data model (shapes, not final Rust)

```rust
struct FileMeta { encoding: Encoding, line_ending: LineEnding, trailing_newline: bool,
                   is_binary: bool, line_count: u32 }
enum Encoding { Utf8, Utf16Le, Utf16Be, Latin1 }
enum LineEnding { Lf, Crlf, Mixed }

struct FileDiffResult { left: FileMeta, right: FileMeta, hunks: Vec<Hunk>, stats: DiffStats }
enum HunkKind { Equal, Insert, Delete, Replace }
struct Hunk { kind: HunkKind, left: LineRange, right: LineRange }
struct LineRange { start: u32, len: u32 }

// computed lazily, per §6 — not eagerly for every Replace hunk in the file
struct IntraLineDiff { left_line: u32, right_line: u32, spans: Vec<Span> }
struct Span { side: Side, kind: SpanKind, start_utf16: u32, len_utf16: u32 }

struct EditDelta { file_id: FileId, from_utf16: u32, to_utf16: u32, inserted: String }

struct DirEntry { path: String, status: EntryStatus, is_dir: bool, is_symlink: bool,
                   symlink_target: Option<String>, size_left: Option<u64>, size_right: Option<u64> }
enum EntryStatus { Same, Modified, LeftOnly, RightOnly, TypeConflict }

struct MergeHunk { kind: MergeHunkKind, base: LineRange, local: LineRange, remote: LineRange,
                    merged: LineRange, resolution: Option<Resolution> }
enum MergeHunkKind { AutoMerged, Conflict }
enum Resolution { TakeLocal, TakeRemote, TakeBoth, TakeBase, Manual }
```

TypeScript types are generated from these via `tauri-specta`/`specta` (build-time codegen from `#[derive(Type)]`), not hand-mirrored — this is dev-tooling, not a hot-path dependency, and it removes an entire class of boundary bugs (see the UTF-16 trap above; a generated type doesn't stop a unit mismatch, but it stops a shape mismatch).

## 5. Module boundaries

**Rust workspace:**
- `diff-core` — `imara-diff` (histogram algorithm) for line-level diff; intra-line char/word diff for `Replace` hunks (lazy, see §6); whitespace/case-ignore normalization; cancellation tokens.
- `text-io` — encoding detection (UTF-8/UTF-16/Latin-1, BOM), line-ending detection (incl. mixed), binary detection/refusal, trailing-newline state, streaming reads for large files.
- `dirwalk` — tiered compare (size+mtime shallow, hash on demand), `ignore`-crate-based gitignore-aware filtering, incremental/cancellable walk, symlink detection.
- `session` — in-memory session state: root pair (files or dirs), open-file edit buffers (`ropey`), dirty tracking, resolved settings (global + per-session override).
- `merge-core` — three-way hunk classification (clean vs conflict), take-left/right/both/base, merged-buffer construction, write-back.
- `vcs-cli` — `git difftool`/`mergetool` argument convention, exit-code contract, `diffgrid FILE1 FILE2` / `diffgrid DIR1 DIR2` entry points.
- `app` — Tauri command/event wiring; owns cancellation handles; the only crate allowed to depend on `tauri`.
- `bench` — harness + fixture generators, §7.

**Frontend (`src/`):**
- `editor/` — CM6 wrapper: two synced instances, decoration application from `Hunk`/`IntraLineDiff`, padding widgets for alignment, collapsed-region rendering, edit-delta capture and forwarding.
- `session/` — sidebar tree, tabs, toolbar state.
- `merge/` — three-way view.
- `settings/` — preferences window.
- `ipc/` — typed wrapper over generated bindings.

## 6. Two design points that fall out of the research above

- **Intra-line diff is viewport-driven, not eager.** Computing character spans for every `Replace` hunk across a 100k-line file on diff completion is wasted work against the 300ms render target when only ~60 lines are visible. `diff-core` returns line-level hunks immediately; intra-line spans are requested per-visible-range as CM6 scrolls, cached per hunk once computed. The mockup's Off/Word/Character setting is a tokenizer choice (character-boundary split vs. word-boundary split before running the same char-level algorithm), applied at this same lazy step.
- **Collapsed-unchanged-region widgets and diff-padding widgets are both CM6 decorations over the same document**, and both interact with line-number display (real file line numbers must keep showing through a collapsed region). Precedence: padding widgets (alignment) are computed first from the raw hunk list; collapse decorations are applied on top, over `Equal` hunks only, and never over a region that a padding widget occupies. This gets exercised in M0, not discovered in M3.

## 7. Benchmark harness — one mechanism per target

| Target | Mechanism | Fixture | Measured from → to |
|---|---|---|---|
| Cold launch ≤ 500ms | External process-timing script (spawn app, wait for a "window-ready" signal Tauri emits to stdout), median of N runs | none (empty launch) | process exec → first interactive paint |
| 10k-line first render ≤ 300ms | In-app instrumentation (timestamp at open-command dispatch → timestamp at post-decoration `requestAnimationFrame`) | `fixtures/10k-line-pair/` (representative hunk density, real source-like content) | warm app, file-open invoked → viewport painted, including syntax highlighting *of the visible viewport only* (whole-file tokenization is explicitly not in scope, per lazy design) |
| 100k-line scroll @ 60fps | Scripted scroll (Playwright driving the Tauri webview) sampling frame timings over a sustained scroll | `fixtures/100k-line-pair/` (generated) | sustained scroll, frame time distribution, p95 ≤ ~16.7ms |
| 50k-file tree, first rows ≤ 1s, cancellable | In-app instrumentation for first-rows-rendered; Rust integration test asserting the walker stops within a bounded time of a cancellation signal and issues no further FS reads after | `fixtures/50k-file-tree/` (generated by a script, not committed) | scan start → first rows visible; cancel signal → walker halt |
| 60fps floor, scroll + resize | Same scroll harness + a resize-simulation variant | 100k-line fixture | as above |
| Idle memory ≤ 300MB @ 10k lines | OS-level RSS sampling of **host process + WebView process(es) summed**, 5s after settling | `fixtures/10k-line-pair/` | post-open, idle |

Pure-Rust algorithmic benchmarks (`criterion`, in `bench`) track `diff-core`/`dirwalk` regressions independent of the UI — useful signal, but not a substitute for the end-to-end numbers above, which are what the acceptance criteria actually specify.

Edge-case correctness fixtures (small, committed, exact): mixed CRLF/LF, no trailing newline, UTF-16/Latin-1 sources, very long single lines, binary files. These are correctness tests (§ "Quality bar" in the brief), not benchmarks — verified against known-good expected output.

## 8. Milestones — vertical slices, each independently runnable and demoable

**M0 — Feasibility spike (gate, not shipped).** Minimal Tauri shell, two CM6 instances, synthetic 100k-line fixture, Rust computes the diff via `imara-diff` and sends hunk metadata over the boundary defined in §3; frontend applies line decorations + padding widgets + synced scroll. Measure cold launch, sustained-scroll fps, idle memory (host+WebView summed), open-to-first-paint. **Kill criteria, decided now, not in the moment**: sustained scroll drops below ~55fps, or cold launch exceeds 600ms, or idle memory exceeds ~350MB → stop, fall back to Option A, re-spike there before continuing past M0. Either outcome gets written down.

**M1 — Two-way file diff, read-only, real.** `diffgrid FILE1 FILE2` on real files. Encoding/line-ending detection, binary detection/refusal, histogram diff, intra-line highlighting (lazy, §6), whitespace/case-ignore toggles (live), collapsed unchanged regions, hunk navigation, minimap. No editing, no directories, no session shell yet — this slice is complete and demoable on its own.

**M2 — Editing.** Both panes editable, live re-diff (debounced) via the delta pipeline in §2, save (encoding/line-ending-preserving, §2), apply/revert individual hunks left↔right. Still single file pair, no session shell.

**M3 — Directory comparison.** Recursive tree, tiered size/mtime→hash compare, incremental cancellable streamed scan, glob/`.gitignore` filters, hide-identical toggle, symlinks shown as links, opening a row reuses M1/M2's file view. Demoable via `diffgrid DIR1 DIR2` standalone.

**M4 — Session shell.** The unified window from the UI-model discussion: sidebar tree + tabs + toolbar quick-toggles + settings window (persisted global preferences), wrapping M1–M3 as one session rather than standalone views.

**M5 — Three-way merge.** BASE/LOCAL/REMOTE panes + merged output, auto-resolve clean hunks vs. real conflicts, take-left/right/both/base, manual edit of merged result, write-back with correct exit status. Demoable via `git mergetool -t diffgrid` on a real conflicted repo.

**M6 — Git integration hardening, packaging, full quality bar.** `difftool`/`mergetool` argument-convention edge cases; exit-code contract; the **single-instance trap**: if a single-instance plugin is used for normal launches, a second `git difftool`/`mergetool` invocation must not just forward to the running instance and exit 0 immediately (git would record an instant, unreviewed "successful" merge) — either exempt CLI tool-mode invocations from single-instance entirely, or make the forwarding process block until the corresponding window closes and forward the real exit status. This is decided here because it constrains the app-launch model, not just the CLI parser. Also: codesign/notarize, Tauri bundler config, benchmark harness wired against the fixture corpus, full edge-case test suite from §7.

---

Open items I want confirmed before implementation starts: does the M0 kill-criteria threshold set (600ms/55fps/350MB, with margin above the hard targets) match your risk tolerance, or should the spike gate be stricter/looser? And is Svelte an acceptable default for the app shell, or is there a framework preference I should use instead?
