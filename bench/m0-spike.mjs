#!/usr/bin/env node
// M0 spike measurement harness — see docs/PLAN.md §7/§8.
//
// Spawns the release binary N times, and for each run records:
//   - cold-launch-to-ready: process spawn -> DIFFGRID_READY on stdout (this conflates
//     cold launch with 100k-line first-render, by design for M0; M1 measures them
//     separately against an empty window vs. a warm app).
//   - idle memory: summed RSS of the app process + descendants (WebKit's Network/Web
//     processes are children on Linux), sampled ~1.2s after DIFFGRID_READY, before the
//     scroll benchmark starts.
//   - scroll fps: parsed from the DIFFGRID_BENCH line the app itself reports.
//
// On Linux without a DISPLAY, this script launches its own Xvfb. On macOS a real
// display is assumed and Xvfb is not touched — the only platform-conditional branch
// in this script; everything else (process-tree walking, RSS sampling) uses `ps`
// uniformly since both GNU ps (Linux) and BSD ps (macOS) support the same
// `-o pid=,ppid=,rss=` output form. See PLATFORM_NOTES.md.
//
// Usage: node bench/m0-spike.mjs [iterations] [--disable-padding]
//   --disable-padding sets DIFFGRID_DISABLE_PADDING=1 for the app, an A/B toggle to test
//   whether alignment-padding block widgets are the source of the scroll-onset stall,
//   rather than asserting that cause without testing it.

import { spawn, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import os from "node:os";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const BIN_PATH = path.join(REPO_ROOT, "target/release/app");
const ITERATIONS = parseInt(process.argv[2] ?? "5", 10);
const DISABLE_PADDING = process.argv.includes("--disable-padding");
const READY_TIMEOUT_MS = 20_000;
const BENCH_TIMEOUT_MS = 15_000;
const MEMORY_SAMPLE_DELAY_MS = 1200; // after READY, before the app's own scroll bench starts

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// One process-table snapshot per call: {pid: {ppid, rssKb}}. A single `ps -A` covers every
// process on the system, which is simpler and more portable than walking /proc (Linux-only)
// or shelling out per-pid.
function processTable() {
  const out = execFileSync("ps", ["-A", "-o", "pid=,ppid=,rss="], { encoding: "utf8" });
  const table = new Map();
  for (const line of out.trim().split("\n")) {
    const [pid, ppid, rss] = line.trim().split(/\s+/).map(Number);
    if (!Number.isNaN(pid)) table.set(pid, { ppid, rssKb: rss });
  }
  return table;
}

function descendants(pid, table) {
  const all = [pid];
  const queue = [pid];
  while (queue.length) {
    const p = queue.shift();
    for (const [candidate, info] of table) {
      if (info.ppid === p) {
        all.push(candidate);
        queue.push(candidate);
      }
    }
  }
  return all;
}

function sampleRssKb(pid) {
  const table = processTable();
  return descendants(pid, table).reduce((sum, p) => sum + (table.get(p)?.rssKb ?? 0), 0);
}

async function ensureDisplay() {
  if (process.platform !== "linux") return { env: {}, xvfb: null };
  if (process.env.DISPLAY) return { env: {}, xvfb: null };

  const display = ":98";
  const xvfb = spawn("Xvfb", [display, "-screen", "0", "1280x800x24"], { stdio: "ignore" });
  await sleep(800);
  return { env: { DISPLAY: display }, xvfb };
}

function killTree(pid) {
  let table;
  try {
    table = processTable();
  } catch {
    table = new Map(); // ps failed (e.g. process already gone) — just kill the pid itself
  }
  for (const p of descendants(pid, table).reverse()) {
    try {
      process.kill(p, "SIGKILL");
    } catch {
      // already gone
    }
  }
}

async function runOnce(env) {
  return new Promise((resolve, reject) => {
    const t0 = performance.now();
    const spawnEnv = { ...process.env, ...env };
    if (DISABLE_PADDING) spawnEnv.DIFFGRID_DISABLE_PADDING = "1";
    const child = spawn(BIN_PATH, [], { env: spawnEnv });

    let readyMs = null;
    let benchResult = null;
    let rssKb = null;
    let buf = "";
    let settled = false;

    const readyTimer = setTimeout(() => {
      if (readyMs === null) finish(new Error(`timed out waiting for DIFFGRID_READY after ${READY_TIMEOUT_MS}ms`));
    }, READY_TIMEOUT_MS);
    const benchTimer = setTimeout(() => {
      if (benchResult === null) finish(new Error(`timed out waiting for DIFFGRID_BENCH after ${BENCH_TIMEOUT_MS}ms`));
    }, BENCH_TIMEOUT_MS);

    function finish(err) {
      if (settled) return;
      settled = true;
      clearTimeout(readyTimer);
      clearTimeout(benchTimer);
      killTree(child.pid);
      if (err) reject(err);
      else resolve({ readyMs, rssKb, benchResult });
    }

    child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (line === "DIFFGRID_READY" && readyMs === null) {
          readyMs = performance.now() - t0;
          setTimeout(() => {
            rssKb = sampleRssKb(child.pid);
          }, MEMORY_SAMPLE_DELAY_MS);
        } else if (line.startsWith("DIFFGRID_BENCH ")) {
          benchResult = JSON.parse(line.slice("DIFFGRID_BENCH ".length));
          // give the memory sample time to land before tearing down
          setTimeout(() => finish(null), 300);
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

function percentile(sorted, p) {
  const idx = Math.floor(sorted.length * p);
  return sorted[Math.min(idx, sorted.length - 1)];
}

function stats(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mean = values.reduce((s, v) => s + v, 0) / values.length;
  return { mean, p50: percentile(sorted, 0.5), p95: percentile(sorted, 0.95), max: sorted[sorted.length - 1] };
}

function memoryPressureLine() {
  const freeGb = os.freemem() / 1024 ** 3;
  const totalGb = os.totalmem() / 1024 ** 3;
  return `system memory: ${freeGb.toFixed(1)}GiB free / ${totalGb.toFixed(1)}GiB total`;
}

async function main() {
  const { env, xvfb } = await ensureDisplay();
  console.log(
    `platform=${process.platform} display-managed=${xvfb !== null} iterations=${ITERATIONS} disablePadding=${DISABLE_PADDING}`,
  );
  // Idle-memory RSS was observed to vary by ~5x across otherwise-identical runs depending on
  // ambient system memory pressure (see docs/PROFILING.md) — logging this so results can be
  // correlated with it rather than treated as a fixed property of the app.
  console.log(memoryPressureLine());

  const runs = [];
  for (let i = 0; i < ITERATIONS; i++) {
    try {
      const result = await runOnce(env);
      runs.push(result);
      const b = result.benchResult;
      const launchOnlyMs = result.readyMs - b.paintMs;
      console.log(
        `run ${i + 1}/${ITERATIONS}: readyMs=${result.readyMs?.toFixed(1)} (launch~${launchOnlyMs.toFixed(1)} + paint ${b.paintMs.toFixed(1)}) ` +
          `rssMB=${(result.rssKb / 1024).toFixed(1)} firstScrollFrame=${b.firstScrollFrameMs.toFixed(1)}ms ` +
          `steady(mean=${b.steadyMeanFrameMs.toFixed(1)}ms p95=${b.steadyP95FrameMs.toFixed(1)}ms worst=${b.steadyWorstFrameMs.toFixed(1)}ms fps=${b.steadyEstimatedFps.toFixed(1)}) ` +
          `framesOver16.7ms=${b.framesOver16_7ms}/${b.frames} framesOver33ms=${b.framesOver33ms}/${b.frames}`,
      );
    } catch (e) {
      console.error(`run ${i + 1}/${ITERATIONS} FAILED: ${e.message}`);
    }
    await sleep(500);
  }

  if (xvfb) xvfb.kill();

  if (runs.length === 0) {
    console.error("all runs failed — no report to produce");
    process.exit(1);
  }

  const ready = stats(runs.map((r) => r.readyMs));
  const launchOnly = stats(runs.map((r) => r.readyMs - r.benchResult.paintMs));
  const rss = stats(runs.map((r) => r.rssKb / 1024)); // MB
  const paint = stats(runs.map((r) => r.benchResult.paintMs));
  const firstScrollFrame = stats(runs.map((r) => r.benchResult.firstScrollFrameMs));
  const steadyFps = stats(runs.map((r) => r.benchResult.steadyEstimatedFps));
  const steadyP95 = stats(runs.map((r) => r.benchResult.steadyP95FrameMs));
  const steadyWorst = stats(runs.map((r) => r.benchResult.steadyWorstFrameMs));

  console.log("\n=== M0 spike report ===");
  console.log(memoryPressureLine());
  console.log(`successful runs: ${runs.length}/${ITERATIONS}, disablePadding=${DISABLE_PADDING}`);
  console.log(`spawn -> DIFFGRID_READY, decomposed (ms):`);
  console.log(`  total:  mean=${ready.mean.toFixed(1)} p50=${ready.p50.toFixed(1)} p95=${ready.p95.toFixed(1)} max=${ready.max.toFixed(1)}`);
  console.log(`  launch-only (total - in-app paint): mean=${launchOnly.mean.toFixed(1)} p50=${launchOnly.p50.toFixed(1)} max=${launchOnly.max.toFixed(1)}`);
  console.log(`  in-app open-to-first-paint:         mean=${paint.mean.toFixed(1)} p50=${paint.p50.toFixed(1)} max=${paint.max.toFixed(1)}`);
  console.log(`idle memory, host process + descendants summed (MB):`);
  console.log(`  mean=${rss.mean.toFixed(1)} p50=${rss.p50.toFixed(1)} p95=${rss.p95.toFixed(1)} max=${rss.max.toFixed(1)}`);
  console.log(`first scroll-triggered frame (one-time layout cost, ms):`);
  console.log(`  mean=${firstScrollFrame.mean.toFixed(1)} p50=${firstScrollFrame.p50.toFixed(1)} max=${firstScrollFrame.max.toFixed(1)}`);
  console.log(`steady-state scroll, excluding the first frame (fps derived from mean frame time):`);
  console.log(`  fps: mean=${steadyFps.mean.toFixed(1)} p50=${steadyFps.p50.toFixed(1)}`);
  console.log(`  p95 frame time: mean=${steadyP95.mean.toFixed(1)}ms  worst frame time: mean=${steadyWorst.mean.toFixed(1)}ms max=${steadyWorst.max.toFixed(1)}ms`);
}

main();
