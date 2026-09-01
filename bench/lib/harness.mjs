// Shared process-spawning/measurement plumbing for bench/m0-spike.mjs and
// bench/open-file-bench.mjs — factored out once both scripts needed the same
// spawn/wait-for-marker-line/RSS-sample/kill-tree/stats machinery, not written generically
// up front. See PLATFORM_NOTES.md for why `ps -A -o pid=,ppid=,rss=` is safe to use uniformly
// on both Linux and macOS.

import { spawn, execFileSync } from "node:child_process";
import os from "node:os";

export function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// One process-table snapshot per call: {pid: {ppid, rssKb}}. A single `ps -A` covers every
// process on the system, which is simpler and more portable than walking /proc (Linux-only)
// or shelling out per-pid.
export function processTable() {
  const out = execFileSync("ps", ["-A", "-o", "pid=,ppid=,rss="], { encoding: "utf8" });
  const table = new Map();
  for (const line of out.trim().split("\n")) {
    const [pid, ppid, rss] = line.trim().split(/\s+/).map(Number);
    if (!Number.isNaN(pid)) table.set(pid, { ppid, rssKb: rss });
  }
  return table;
}

export function descendants(pid, table) {
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

export function sampleRssKb(pid) {
  const table = processTable();
  return descendants(pid, table).reduce((sum, p) => sum + (table.get(p)?.rssKb ?? 0), 0);
}

export async function ensureDisplay() {
  if (process.platform !== "linux") return { env: {}, xvfb: null };
  if (process.env.DISPLAY) return { env: {}, xvfb: null };

  const display = ":98";
  const xvfb = spawn("Xvfb", [display, "-screen", "0", "1280x800x24"], { stdio: "ignore" });
  await sleep(800);
  return { env: { DISPLAY: display }, xvfb };
}

export function killTree(pid) {
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

export function percentile(sorted, p) {
  const idx = Math.floor(sorted.length * p);
  return sorted[Math.min(idx, sorted.length - 1)];
}

export function stats(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mean = values.reduce((s, v) => s + v, 0) / values.length;
  return { mean, p50: percentile(sorted, 0.5), p95: percentile(sorted, 0.95), max: sorted[sorted.length - 1] };
}

export function memoryPressureLine() {
  const freeGb = os.freemem() / 1024 ** 3;
  const totalGb = os.totalmem() / 1024 ** 3;
  return `system memory: ${freeGb.toFixed(1)}GiB free / ${totalGb.toFixed(1)}GiB total`;
}
