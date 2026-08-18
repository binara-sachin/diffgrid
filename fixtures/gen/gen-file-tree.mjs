#!/usr/bin/env node
// Deterministic synthetic paired-directory-tree generator for M3's dirwalk benchmark fixture.
// Produces a nested tree of files split across left/right with a controlled mix of
// same/modified/left-only/right-only entries, reproducible across regenerations via seed.
//
// Usage: node gen-file-tree.mjs <totalFiles> <outDir> [seed]

import { mkdirSync, writeFileSync, utimesSync } from "node:fs";

function mulberry32(seed) {
  return function () {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const FILES_PER_DIR = 40;
const SUBDIRS_PER_DIR = 6;

function pathFor(i) {
  // Nests a handful of levels deep rather than one flat directory of totalFiles entries --
  // closer to a real source tree, and exercises depth in the walker.
  const dirIndex = Math.floor(i / FILES_PER_DIR);
  const parts = [];
  let n = dirIndex;
  while (n > 0 || parts.length === 0) {
    parts.unshift(`d${n % SUBDIRS_PER_DIR}`);
    n = Math.floor(n / SUBDIRS_PER_DIR);
    if (parts.length > 4) break; // cap depth
  }
  return `${parts.join("/")}/file${i}.txt`;
}

function generate(totalFiles, seed) {
  const rng = mulberry32(seed);
  const entries = { same: 0, modified: 0, leftOnly: 0, rightOnly: 0 };
  const files = [];
  for (let i = 0; i < totalFiles; i++) {
    const path = pathFor(i);
    const r = rng();
    let kind;
    if (r < 0.8) kind = "same";
    else if (r < 0.9) kind = "modified";
    else if (r < 0.95) kind = "leftOnly";
    else kind = "rightOnly";
    entries[kind]++;
    files.push({ path, kind, i });
  }
  return { files, entries };
}

function contentFor(i, variant) {
  return `line ${i} variant ${variant}\ncontent for file ${i}\n`;
}

const [, , totalFilesArg, outDir, seedArg] = process.argv;
const totalFiles = parseInt(totalFilesArg, 10);
const seed = seedArg ? parseInt(seedArg, 10) : 42;

if (!totalFiles || !outDir) {
  console.error("Usage: gen-file-tree.mjs <totalFiles> <outDir> [seed]");
  process.exit(1);
}

const { files, entries } = generate(totalFiles, seed);
const leftRoot = `${outDir}/left`;
const rightRoot = `${outDir}/right`;

for (const { path, kind, i } of files) {
  if (kind === "same") {
    // Real "unchanged since last scan" files very often share an mtime in practice (a fresh
    // git checkout stamps everything with the checkout time; an untouched file simply keeps
    // whatever mtime it already had on both sides of a re-scan) -- a synthetic fixture that
    // instead gives left/right sequential wall-clock-apart mtimes would exercise dirwalk's
    // byte-compare fallback on every single "same" file, which is the heuristic's worst case,
    // not its typical one. Forcing an identical mtime here is what makes this fixture a
    // representative stand-in for the real workload the ≤1s target is written against.
    const content = contentFor(i, "a");
    mkdirSync(`${leftRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    mkdirSync(`${rightRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    writeFileSync(`${leftRoot}/${path}`, content);
    writeFileSync(`${rightRoot}/${path}`, content);
    const mtime = new Date();
    utimesSync(`${leftRoot}/${path}`, mtime, mtime);
    utimesSync(`${rightRoot}/${path}`, mtime, mtime);
  } else if (kind === "modified") {
    mkdirSync(`${leftRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    mkdirSync(`${rightRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    writeFileSync(`${leftRoot}/${path}`, contentFor(i, "a"));
    writeFileSync(`${rightRoot}/${path}`, contentFor(i, "b-modified"));
  } else if (kind === "leftOnly") {
    mkdirSync(`${leftRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    writeFileSync(`${leftRoot}/${path}`, contentFor(i, "left-only"));
  } else {
    mkdirSync(`${rightRoot}/${path}`.split("/").slice(0, -1).join("/"), { recursive: true });
    writeFileSync(`${rightRoot}/${path}`, contentFor(i, "right-only"));
  }
}

console.log(
  `${outDir}: ${totalFiles} files -- same=${entries.same} modified=${entries.modified} leftOnly=${entries.leftOnly} rightOnly=${entries.rightOnly}`,
);
