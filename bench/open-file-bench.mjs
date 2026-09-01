#!/usr/bin/env node
// docs/PLAN.md §7's "10k-line first render ≤300ms" target — separate from m0-spike.mjs's
// cold-launch/scroll measurements, which exercise the no-args M0 spike flow, not a real
// `diffgrid FILE1 FILE2` open. This spawns the release binary against a real file pair and
// measures "open-command dispatch -> post-decoration requestAnimationFrame" exactly as
// mountFileTab in src/routes/+page.svelte instruments it, reported via the same
// `report_bench`/`DIFFGRID_BENCH` marker line m0-spike.mjs already knows how to wait for
// (shape here is `{mode: "open_file_pair", paintMs}`, not the spike's scroll-stats shape).
//
// This number is measured on an app that was just cold-started for this one open, so it does
// include cold-launch overhead in when the marker line arrives -- but the *value it reports* is
// the isolated in-app span, immune to that, because the timer starts inside the frontend only
// once the open command actually dispatches (after Tauri/webview init is already done).
//
// Also samples idle memory ~5s after paint, same delay m0-spike.mjs uses -- this is the *other*
// half of docs/PLAN.md §7's "Idle memory ≤300MB @ 10k lines" row: m0-spike.mjs's no-args flow
// always loads the 100k fixture (see its own header comment), so its idle-memory numbers were
// never actually measured against the 10k-line target that row names. This is.
//
// Usage: node bench/open-file-bench.mjs [iterations] [leftFile] [rightFile]
//   Defaults to fixtures/10k-line-pair/{left,right} -- generate it first if missing:
//   node fixtures/gen/gen-line-pair.mjs 10000 fixtures/10k-line-pair 7 (writes left.js/right.js)

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { existsSync } from "node:fs";
import { ensureDisplay, killTree, sampleRssKb, stats, memoryPressureLine, sleep } from "./lib/harness.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const BIN_PATH = path.join(REPO_ROOT, "target/release/app");
const ITERATIONS = parseInt(process.argv[2] ?? "5", 10);
const LEFT = path.resolve(process.argv[3] ?? path.join(REPO_ROOT, "fixtures/10k-line-pair/left.js"));
const RIGHT = path.resolve(process.argv[4] ?? path.join(REPO_ROOT, "fixtures/10k-line-pair/right.js"));
const TARGET_MS = 300;
const MEMORY_TARGET_MB = 300;
const BENCH_TIMEOUT_MS = 15_000;
const MEMORY_SAMPLE_DELAY_MS = 5_000; // matches m0-spike.mjs's "5s after settling" per PLAN.md §7

async function runOnce(env) {
  return new Promise((resolve, reject) => {
    const child = spawn(BIN_PATH, [LEFT, RIGHT], { env });
    let settled = false;
    let buf = "";

    const timer = setTimeout(() => {
      finish(new Error(`timed out waiting for DIFFGRID_BENCH after ${BENCH_TIMEOUT_MS}ms`));
    }, BENCH_TIMEOUT_MS);

    function finish(err, result) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      killTree(child.pid);
      if (err) reject(err);
      else resolve(result);
    }

    child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (line.startsWith("DIFFGRID_BENCH ")) {
          const parsed = JSON.parse(line.slice("DIFFGRID_BENCH ".length));
          if (parsed.mode === "open_file_pair") {
            clearTimeout(timer); // the line arrived -- only the memory-settle wait remains
            // `finish` is idempotent (guarded by `settled`), so no need to guard this sample --
            // the only things that could settle first are `error`/`exit`, which already finish
            // (as a failure) before this resolves, making the sample moot either way.
            sleep(MEMORY_SAMPLE_DELAY_MS).then(() => finish(null, { ...parsed, rssKb: sampleRssKb(child.pid) }));
          }
        } else if (line.startsWith("DIFFGRID_ERROR ")) {
          finish(new Error(`app reported frontend error: ${line}`));
        }
      }
    });

    child.on("error", finish);
    child.on("exit", (code) => {
      if (!settled) finish(new Error(`process exited early with code ${code}`));
    });
  });
}

async function main() {
  if (!existsSync(LEFT) || !existsSync(RIGHT)) {
    console.error(`fixture not found: ${LEFT} / ${RIGHT}`);
    console.error("generate it with: node fixtures/gen/gen-line-pair.mjs 10000 fixtures/10k-line-pair 7");
    process.exit(1);
  }

  const { env, xvfb } = await ensureDisplay();
  console.log(`platform=${process.platform} display-managed=${xvfb !== null} iterations=${ITERATIONS}`);
  console.log(memoryPressureLine());
  console.log(`left=${LEFT} right=${RIGHT}`);

  const runs = [];
  for (let i = 0; i < ITERATIONS; i++) {
    try {
      const result = await runOnce({ ...process.env, ...env });
      runs.push(result);
      console.log(`run ${i + 1}/${ITERATIONS}: paintMs=${result.paintMs.toFixed(1)} rssMB=${(result.rssKb / 1024).toFixed(1)}`);
    } catch (e) {
      console.error(`run ${i + 1}/${ITERATIONS} FAILED: ${e.message}`);
    }
  }

  if (xvfb) xvfb.kill();

  if (runs.length === 0) {
    console.error("all runs failed — no report to produce");
    process.exit(1);
  }

  const s = stats(runs.map((r) => r.paintMs));
  const rss = stats(runs.map((r) => r.rssKb / 1024));
  console.log(`\n=== open-file-bench report (docs/PLAN.md §7) ===`);
  console.log(`successful runs: ${runs.length}/${ITERATIONS}`);
  console.log(memoryPressureLine());
  console.log(`open-command dispatch -> post-decoration paint (ms, target ≤${TARGET_MS}): mean=${s.mean.toFixed(1)} p50=${s.p50.toFixed(1)} p95=${s.p95.toFixed(1)} max=${s.max.toFixed(1)}`);
  console.log(s.mean <= TARGET_MS ? "  PASS (mean within target)" : "  OVER TARGET (mean exceeds target)");
  console.log(`idle memory @ 10k lines, ~5s after paint (MB, target ≤${MEMORY_TARGET_MB}): mean=${rss.mean.toFixed(1)} p50=${rss.p50.toFixed(1)} max=${rss.max.toFixed(1)}`);
  console.log(rss.mean <= MEMORY_TARGET_MB ? "  PASS (mean within target)" : "  OVER TARGET (mean exceeds target)");
}

main();
