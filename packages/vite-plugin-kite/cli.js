#!/usr/bin/env node
// `kitec fmt` and `kitec check`, for a project that installed nothing.
//
// The plugin already carries the compiler so a build needs no toolchain. The
// same is true of the rest of the tooling, and a project that has to install
// something before it can format its own source has only moved the problem.
//
//     kite fmt src            rewrite every .kite file
//     kite fmt --check src    say which would change, and exit 1 if any would
//     kite check src          diagnostics, exactly as the terminal prints them
//
// A native `kitec` is used when one is on `PATH`, because it is faster. The
// answers are the same either way — it is the same compiler.

import { execFile } from "node:child_process";
import { readdir, readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { WasmCompiler } from "./index.js";

const run = promisify(execFile);
const wasm = new WasmCompiler();

/// Every `.kite` file under the given paths, in a settled order.
///
/// Sorted, because a formatter that reported its files in whatever order the
/// file system handed them back would produce a different diff on every
/// machine.
async function sources(paths) {
  const out = [];
  for (const path of paths) {
    const info = await stat(path).catch(() => null);
    if (!info) throw new Error(`no such file or directory: ${path}`);
    if (info.isDirectory()) {
      for (const name of (await readdir(path)).sort()) {
        out.push(...(await sources([join(path, name)])));
      }
    } else if (path.endsWith(".kite")) {
      out.push(path);
    }
  }
  return out;
}

/// Whether a native compiler is there, asked once.
///
/// When it is, it *is* the tool: the work is handed to it and its output and
/// exit code are passed through, rather than half the job being done here.
/// When it is not, the same work happens through the WebAssembly build. The
/// answers agree because it is the same compiler.
let native = null;

async function haveNative() {
  if (native !== null) return native;
  native = await run("kitec", ["--version"]).then(() => true, () => false);
  return native;
}

async function viaNative(args) {
  try {
    const { stdout, stderr } = await run("kitec", args);
    process.stdout.write(stdout + stderr);
    return 0;
  } catch (e) {
    // A non-zero exit is the compiler disagreeing with the program, which is
    // an answer rather than a failure to get one.
    process.stdout.write((e.stdout || "") + (e.stderr || ""));
    return e.code === "ENOENT" ? 1 : (e.code ?? 1);
  }
}

async function fmt(paths, checkOnly) {
  const files = await sources(paths);
  if (files.length === 0) {
    process.stdout.write("no .kite files\n");
    return 0;
  }
  if (await haveNative()) {
    return viaNative(["fmt", ...files, ...(checkOnly ? ["--check"] : [])]);
  }

  let changed = 0;
  for (const file of files) {
    const before = await readFile(file, "utf8");
    const after = await wasm.text("kite_format", before);
    if (after === before) continue;
    changed += 1;
    if (checkOnly) {
      process.stdout.write(`${file} is not formatted\n`);
    } else {
      await writeFile(file, after);
      process.stdout.write(`formatted ${file}\n`);
    }
  }
  if (changed === 0) {
    process.stdout.write(`${files.length} file(s), all formatted\n`);
  }
  return checkOnly && changed > 0 ? 1 : 0;
}

async function check(paths) {
  const files = await sources(paths);
  if (files.length === 0) {
    process.stdout.write("no .kite files\n");
    return 0;
  }
  if (await haveNative()) {
    let code = 0;
    for (const file of files) code = (await viaNative(["check", file])) || code;
    return code;
  }

  let failed = 0;
  for (const file of files) {
    const raw = (await wasm.text("kite_check", await readFile(file, "utf8"))).trim();
    // The WebAssembly compiler has no path to be given, so it names the source
    // `playground.kite` in every span. Left alone, `npm run check` reports an
    // error in a file the project does not contain — which is worse than no
    // location at all, because it reads as a real one.
    const out = raw.split("playground.kite:").join(`${file}:`);
    // `kite_check` answers with nothing at all when a program is clean.
    if (out === "" || out === "ok") continue;
    failed += 1;
    process.stdout.write(`${out}\n`);
  }
  if (failed === 0) {
    process.stdout.write(`${files.length} file(s), no diagnostics\n`);
  }
  return failed > 0 ? 1 : 0;
}

const [command, ...rest] = process.argv.slice(2);
const paths = rest.filter((a) => !a.startsWith("-"));
const flags = rest.filter((a) => a.startsWith("-"));

try {
  let code = 0;
  if (command === "fmt") code = await fmt(paths.length ? paths : ["."], flags.includes("--check"));
  else if (command === "check") code = await check(paths.length ? paths : ["."]);
  else {
    process.stdout.write(
      "kite — the parts of the Kite toolchain a project needs\n\n" +
        "  kite fmt [--check] [paths]   lay the source out the one way\n" +
        "  kite check [paths]           diagnostics, without running anything\n\n" +
        "The compiler ships with this package, so there is nothing to install.\n",
    );
    code = command ? 1 : 0;
  }
  process.exit(code);
} catch (e) {
  process.stderr.write(`error: ${e.message}\n`);
  process.exit(1);
}
