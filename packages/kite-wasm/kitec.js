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
import { basename, dirname, extname, join, resolve } from "node:path";
import process from "node:process";

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
 */
async function siblingsOf(file) {
  const dir = dirname(resolve(file));
  const self = basename(file);
  const siblings = {};
  for (const name of await readdir(dir)) {
    if (name === self || extname(name) !== ".kite") continue;
    siblings[basename(name, ".kite")] = await readFile(join(dir, name), "utf8");
  }
  for (const [name, from] of await dependencyDirs(file)) {
    for (const entry of await readdir(from).catch(() => [])) {
      if (extname(entry) !== ".kite") continue;
      siblings[`${name}/${basename(entry, ".kite")}`] =
        await readFile(join(from, entry), "utf8");
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
  const here = dirname(new URL(import.meta.url).pathname);
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
    });
    process.stdout.write(diagnostics);
    process.exit(diagnostics.trim() === "" ? 0 : 1);
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
    let artefacts;
    try {
      artefacts = kite.build({
        entry: await readFile(file, "utf8"),
        siblings: await siblingsOf(file),
        release: flags.has("--release"),
      });
    } catch (error) {
      if (error instanceof BuildFailed) fail(error.diagnostics);
      throw error;
    }
    const out = outDir ?? dirname(resolve(file));
    const { mkdir } = await import("node:fs/promises");
    await mkdir(out, { recursive: true });
    for (const [name, body] of Object.entries(artefacts)) {
      await writeFile(join(out, name), body);
      process.stdout.write(`wrote ${join(out, name)} (${body.length} bytes)\n`);
    }
    break;
  }

  default:
    fail(`kitec: unknown command \`${command}\`\n\n${USAGE}`);
}
