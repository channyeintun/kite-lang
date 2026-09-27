#!/usr/bin/env node
// `kitec`, over the WebAssembly compiler.
//
// This is not a second compiler and reimplements nothing: every subcommand
// hands the source to the same crate `kitec` is built from and prints what it
// answers. It exists so a project that has this package installed can run
// `kitec fmt` and `kitec check` without also needing a native binary — which
// matters on a platform that has none, and in a browser-based Node such as
// WebContainer, where machine code cannot run at all.
//
// The native `kitec` is the fuller tool: `bundle`, `pkg`, `--native` and the
// language server are the machine's to run. What is here is what a project's
// scripts reach for.

import { readFile, writeFile, readdir } from "node:fs/promises";
import { basename, dirname, extname, join, relative, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { compiler, BuildFailed } from "./compiler.js";

const USAGE = `kitec — the Kite compiler, as WebAssembly

USAGE:
    kitec run   <file.kite>          compile and run
    kitec check <file.kite>          check without running
    kitec build <file.kite>          compile to WebAssembly
    kitec fmt   <file.kite>...       lay the files out the one way
    kitec doc   <file.kite>          the reference, from the doc comments

OPTIONS:
    --release         build for release: \`assert\` is dropped, \`require\` is not
    --out <dir>       with \`build\`, where to write (default: alongside the file)
    --check           with \`fmt\`, report rather than rewrite

Run \`kitec\` itself for the whole compiler: \`bundle\`, \`pkg\`, the language
server and native execution live there. https://kite-lang.dev/install
`;

/**
 * A Kite module is a directory, so a program's siblings are the other `.kite`
 * files beside it — keyed by module path, which for a file beside the entry is
 * the filename without its extension, because that is what `use checkout`
 * names.
 *
 * Everything the project's `kite.toml` declares comes too, under its own name,
 * so `use markdown/render` reaches inside the package. Nothing is fetched:
 * this reads a `path =` dependency where it is and a git one out of
 * `.kite/vendor`, which `kitec pkg` filled.
 *
 * `origins` is filled with where each one was read from, by the name the
 * compiler gives it — its key and `.kite` — for a source map to name it by.
 */
async function siblingsOf(file, origins = {}) {
  const dir = dirname(resolve(file));
  const self = basename(file);
  const siblings = {};
  const add = async (key, at) => {
    siblings[key] = await readFile(at, "utf8");
    origins[`${key}.kite`] = at;
  };
  for (const name of await readdir(dir)) {
    if (name === self || extname(name) !== ".kite") continue;
    await add(basename(name, ".kite"), join(dir, name));
  }
  for (const [name, from] of await dependencyDirs(file)) {
    for (const entry of await readdir(from).catch(() => [])) {
      if (extname(entry) !== ".kite") continue;
      await add(`${name}/${basename(entry, ".kite")}`, join(from, entry));
    }
  }
  return siblings;
}

/**
 * The dependencies declared by the nearest `kite.toml` above a file, as name
 * to directory.
 *
 * The manifest is searched for upwards because a program is usually
 * `src/main.kite` and the manifest is beside `src/` — the same search the
 * native compiler does, so a project means one thing however it is built.
 *
 * A deliberately small reader for a deliberately small subset. What it cannot
 * read it ignores: the compiler is the authority on the file, and a second
 * opinion here would only ever disagree.
 */
async function dependencyDirs(file) {
  let dir = dirname(resolve(file));
  let text = null;
  for (;;) {
    text = await readFile(join(dir, "kite.toml"), "utf8").catch(() => null);
    if (text !== null) break;
    const up = dirname(dir);
    if (up === dir) return [];
    dir = up;
  }

  const found = [];
  let table = "";
  for (const raw of text.split("\n")) {
    const line = raw.split("#")[0].trim();
    if (line === "") continue;
    const heading = /^\[(.+)\]$/.exec(line);
    if (heading) {
      table = heading[1].trim();
      continue;
    }
    if (table !== "dependencies") continue;
    const at = line.indexOf("=");
    if (at < 0) continue;
    const name = line.slice(0, at).trim();
    const path = /\bpath\s*=\s*"([^"]*)"/.exec(line.slice(at + 1));
    found.push([
      name,
      path ? resolve(dir, path[1]) : join(dir, ".kite", "vendor", name),
    ]);
  }
  return found;
}

function fail(message) {
  process.stderr.write(message.endsWith("\n") ? message : `${message}\n`);
  process.exit(1);
}

const argv = process.argv.slice(2);
const command = argv[0];
const flags = new Set(argv.filter((a) => a.startsWith("--")));
const positional = argv.slice(1).filter((a) => !a.startsWith("--"));
const outIndex = argv.indexOf("--out");
const outDir = outIndex === -1 ? null : argv[outIndex + 1];
// `--out <dir>` puts its value in the positional list; take it back out.
const files = positional.filter((a) => a !== outDir);

if (flags.has("--version") || command === "--version") {
  // `fileURLToPath`, not `.pathname`: the path of a URL is percent-encoded,
  // so an install directory with a space in it was not found, and on Windows
  // it is `/C:/…`, which is not a path at all.
  const here = dirname(fileURLToPath(import.meta.url));
  const { version } = JSON.parse(await readFile(join(here, "package.json"), "utf8"));
  process.stdout.write(`kitec ${version} (WebAssembly)\n`);
  process.exit(0);
}

if (!command || flags.has("--help") || command === "help") {
  process.stdout.write(USAGE);
  process.exit(command ? 0 : 1);
}

const [file] = files;
if (!file && command !== "help") {
  fail(`kitec: \`${command}\` needs a file\n\n${USAGE}`);
}

const kite = await compiler();

switch (command) {
  case "run": {
    const output = kite.runModule({
      entry: await readFile(file, "utf8"),
      siblings: await siblingsOf(file),
      path: file,
    });
    process.stdout.write(output);
    // Diagnostics are rendered into the same answer, so a failed compile is
    // recognised by what it says rather than by a separate channel.
    process.exit(/^error(\[|:)/m.test(output) ? 1 : 0);
  }

  case "check": {
    const diagnostics = kite.checkModule({
      entry: await readFile(file, "utf8"),
      siblings: await siblingsOf(file),
      path: file,
    });
    process.stdout.write(diagnostics);
    // An error fails the check; a warning is said and does not, as with the
    // native `kitec`. Any output at all used to count as failure.
    process.exit(/^error(\[|:)/m.test(diagnostics) ? 1 : 0);
  }

  case "doc": {
    process.stdout.write(kite.docs(await readFile(file, "utf8")));
    break;
  }

  case "fmt": {
    let changed = 0;
    for (const each of files) {
      const source = await readFile(each, "utf8");
      const formatted = kite.format(source);
      if (formatted === source) {
        if (!flags.has("--check")) process.stdout.write(`${each} is already formatted\n`);
        continue;
      }
      changed += 1;
      if (flags.has("--check")) {
        process.stdout.write(`${each} is not formatted\n`);
      } else {
        await writeFile(each, formatted);
        process.stdout.write(`formatted ${each}\n`);
      }
    }
    process.exit(flags.has("--check") && changed > 0 ? 1 : 0);
  }

  case "build": {
    const out = outDir ?? dirname(resolve(file));
    const { mkdir, realpath } = await import("node:fs/promises");
    await mkdir(out, { recursive: true });
    // The source map names each file relative to where it is written, as the
    // native `kitec` does: a browser looks a source up against the map's own
    // URL, so the bare `main.kite` it used to say was looked for in `out`.
    // Measured between the real paths, as there, so a directory reached
    // through a link is not climbed out of and back into. Diagnostics still
    // name the file as it was typed.
    const origins = { [file]: resolve(file) };
    const siblings = await siblingsOf(file, origins);
    const real = async (path) => realpath(path).catch(() => resolve(path));
    const from = await real(out);
    const sourceNames = {};
    for (const [name, at] of Object.entries(origins)) {
      sourceNames[name] = relative(from, await real(at)).replaceAll("\\", "/");
    }
    let artefacts;
    try {
      artefacts = kite.build({
        entry: await readFile(file, "utf8"),
        siblings,
        release: flags.has("--release"),
        path: file,
        sourceNames,
      });
    } catch (error) {
      if (error instanceof BuildFailed) fail(error.diagnostics);
      throw error;
    }
    for (const [name, body] of Object.entries(artefacts)) {
      await writeFile(join(out, name), body);
      process.stdout.write(`wrote ${join(out, name)} (${body.length} bytes)\n`);
    }
    break;
  }

  default:
    fail(`kitec: unknown command \`${command}\`\n\n${USAGE}`);
}
