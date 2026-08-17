#!/usr/bin/env node
// Deterministic synthetic file-pair generator for diff benchmark fixtures.
// Produces source-like content with unchanged runs interleaved with insert/delete/replace
// hunks at a controlled density, so hunk count/size is reproducible across regenerations.
//
// Usage: node gen-line-pair.mjs <totalLines> <outDir> [seed]

import { mkdirSync, writeFileSync } from "node:fs";

function mulberry32(seed) {
  return function () {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = [
  "value", "index", "state", "buffer", "handle", "result", "config", "session",
  "offset", "length", "cursor", "target", "source", "delta", "range", "token",
];

function makeLine(rng, n) {
  const a = WORDS[Math.floor(rng() * WORDS.length)];
  const b = WORDS[Math.floor(rng() * WORDS.length)];
  return `  const ${a}_${n} = compute${b[0].toUpperCase()}${b.slice(1)}(${a}, ${n});`;
}

function makeLines(rng, start, count) {
  const out = [];
  for (let i = 0; i < count; i++) out.push(makeLine(rng, start + i));
  return out;
}

function generate(totalLines, seed) {
  const rng = mulberry32(seed);
  const left = [];
  const right = [];
  let n = 0;
  let hunks = 0;

  while (n < totalLines) {
    // unchanged run
    const runLen = 40 + Math.floor(rng() * 400);
    const unchanged = makeLines(rng, n, Math.min(runLen, totalLines - n));
    left.push(...unchanged);
    right.push(...unchanged);
    n += unchanged.length;
    if (n >= totalLines) break;

    // changed hunk: insert, delete, or replace, 3-40 lines
    const kind = rng();
    const hunkLen = 3 + Math.floor(rng() * 37);
    if (kind < 0.34) {
      // insert: only right gets new lines
      right.push(...makeLines(rng, n + 100000, hunkLen));
    } else if (kind < 0.67) {
      // delete: only left had these lines
      left.push(...makeLines(rng, n + 200000, hunkLen));
      n += hunkLen;
    } else {
      // replace: both sides get different lines for the same span
      left.push(...makeLines(rng, n + 300000, hunkLen));
      right.push(...makeLines(rng, n + 400000, hunkLen));
      n += hunkLen;
    }
    hunks++;
  }

  return { left, right, hunks };
}

const [, , totalLinesArg, outDir, seedArg] = process.argv;
const totalLines = parseInt(totalLinesArg, 10);
const seed = seedArg ? parseInt(seedArg, 10) : 42;

if (!totalLines || !outDir) {
  console.error("Usage: gen-line-pair.mjs <totalLines> <outDir> [seed]");
  process.exit(1);
}

const { left, right, hunks } = generate(totalLines, seed);
mkdirSync(outDir, { recursive: true });
writeFileSync(`${outDir}/left.js`, left.join("\n") + "\n");
writeFileSync(`${outDir}/right.js`, right.join("\n") + "\n");
console.log(`${outDir}: left=${left.length} lines, right=${right.length} lines, ${hunks} hunks`);
