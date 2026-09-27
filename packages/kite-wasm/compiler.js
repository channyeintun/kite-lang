// The Kite compiler, as WebAssembly.
//
// `kitec` is Rust and already targets WebAssembly, so a build tool needs no
// binary: it depends on this package and gets the compiler itself. Not a
// reimplementation and not a subset — the same crate, built for a different
// target, and its output is byte-for-byte what the native compiler writes.
//
// What that buys, over shipping a native binary per platform:
//
//   - One artefact for every operating system, with no `os`/`cpu` matrix and
//     no `optionalDependencies` that can resolve to nothing.
//   - It runs wherever WebAssembly runs — including a browser-based Node
//     such as WebContainer, where StackBlitz and Bolt run projects and where
//     native machine code cannot execute at all.
//   - Nothing is downloaded at install time, so there is no postinstall step
//     and no supply-chain surface beyond the tarball npm already verified.
//
// The module imports nothing. It is instantiated with an empty import object
// and talks through a pointer and a length, which is why it needs no glue
// generator and no binding framework.

import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const WASM = join(dirname(fileURLToPath(import.meta.url)), "kite-compiler.wasm");

/** @type {Promise<Compiler> | undefined} */
let instantiated;

/**
 * The compiler, instantiated once per process.
 *
 * Instantiation costs a few tens of milliseconds and a build tool asks for the
 * compiler once per file, so the module is kept rather than rebuilt.
 *
 * @returns {Promise<Compiler>}
 */
export function compiler() {
  instantiated ??= readFile(WASM)
    .then((bytes) => WebAssembly.compile(bytes))
    .then(async (module) => new Compiler(module, await WebAssembly.instantiate(module, {})));
  return instantiated;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * The frame entry `build`'s `sourceNames` travel in: a line per file, its
 * name as compiled, a tab, and the name the source map gives it. No module is
 * called this, since no module name holds a `?`.
 */
const SOURCE_NAMES = "?source-names";

/** What a build produced, or why it produced nothing. */
export class BuildFailed extends Error {
  /** @param {string} diagnostics Rendered exactly as a terminal renders them. */
  constructor(diagnostics) {
    super(diagnostics.trim() || "the build failed");
    this.name = "BuildFailed";
    this.diagnostics = diagnostics;
  }
}

class Compiler {
  #module;
  #exports;

  constructor(module, instance) {
    this.#module = module;
    this.#exports = instance.exports;
  }

  /** Compile and run a program; answers with what it printed. */
  run(source) {
    return this.#text("kite_run", source);
  }

  /** Diagnostics for one file, or "" when it is clean. */
  check(source) {
    return this.#text("kite_check", source);
  }

  /** The source, laid out the one way. */
  format(source) {
    return this.#text("kite_format", source);
  }

  /** The reference, from the doc comments. */
  docs(source) {
    return this.#text("kite_docs", source);
  }

  /**
   * Diagnostics for a whole module, or "" when it is clean.
   *
   * A Kite module is a *directory*, so a program that says `use checkout` has
   * a sibling the checker has to see. `check` takes one file and would report
   * a missing module for it.
   *
   * @param {{entry: string, siblings?: Record<string, string>, path?: string}} module
   *   `siblings` is keyed by module path — `checkout`, not `checkout.kite`,
   *   and `markdown/render` for a module inside a declared dependency.
   *   `path` is what diagnostics call the entry file; `main.kite` without it.
   */
  checkModule({ entry, siblings = {}, path }) {
    const answer = this.#call("kite_check_module", this.#frame(entry, siblings, path));
    return decoder.decode(answer);
  }

  /**
   * Compile and run a whole module; answers with what it printed.
   *
   * `run` takes one file, for the same reason `check` does and with the same
   * consequence: a program with a `use` in it reports a missing module rather
   * than running.
   *
   * @param {{entry: string, siblings?: Record<string, string>, path?: string}} module
   */
  runModule({ entry, siblings = {}, path }) {
    const answer = this.#call("kite_run_module", this.#frame(entry, siblings, path));
    return decoder.decode(answer);
  }

  /**
   * Compile a module the way `kitec build --emit wasm` does.
   *
   * `sourceNames` says what the source map calls each file, by the name it
   * is compiled under — `path` for the program, a sibling's module name and
   * `.kite` for a sibling — as a path relative to where the map is written.
   * The compiler has no filesystem to work that out from, and a file left
   * out keeps its bare name.
   *
   * @param {{
   *   entry: string,
   *   siblings?: Record<string, string>,
   *   release?: boolean,
   *   path?: string,
   *   sourceNames?: Record<string, string>,
   * }} module
   * @returns {Record<string, Uint8Array>} `app.wasm` and `app.js`; `api.js`
   *   and `api.d.ts` when the program has a `pub fn` of its own; and
   *   `app.wasm.map` in a debug build — exactly the files the native compiler
   *   writes.
   * @throws {BuildFailed} with the diagnostics a terminal would have shown.
   */
  build({ entry, siblings = {}, release = false, path, sourceNames = {} }) {
    const names = Object.entries(sourceNames)
      .map(([compiled, mapped]) => `${compiled}\t${mapped}\n`)
      .join("");
    const answer = this.#call(
      "kite_build",
      this.#frame(entry, names === "" ? siblings : { ...siblings, [SOURCE_NAMES]: names }, path),
      release ? 1 : 0,
    );
    const artefacts = unframe(answer);
    // A failed compile answers with one entry named `diagnostics`, so the
    // caller reads the same frame either way.
    if (artefacts.diagnostics !== undefined) {
      throw new BuildFailed(decoder.decode(artefacts.diagnostics));
    }
    return artefacts;
  }

  /** A call taking one source string and answering with text. */
  #text(name, source) {
    return decoder.decode(this.#call(name, encoder.encode(source)));
  }

  /**
   * Write `input` into the module's memory, call `name`, and copy the answer
   * back out.
   *
   * The copy is not an optimisation to remove. A call may grow the module's
   * memory, and growing it **detaches every existing view** onto the old
   * buffer — so `exports.memory.buffer` is read again after the call, and the
   * bytes are copied before anything else can allocate.
   */
  #call(name, input, ...args) {
    const exports = this.#exports;
    if (exports === null) {
      throw new Error("the compiler stopped on an earlier input: ask `compiler()` for a fresh one");
    }
    const pointer = exports.kite_alloc(input.length);
    new Uint8Array(exports.memory.buffer, pointer, input.length).set(input);
    let answer;
    try {
      answer = exports[name](pointer, input.length, ...args);
    } catch (error) {
      // The compiler trapped — a panic, or a stack it ran out of. Its memory
      // is whatever it was when it stopped, and every call after this one
      // used to fail on that same broken instance, so one bad file took a
      // dev server down until it was restarted. The instance is replaced; the
      // compiled module is kept, so that costs no recompilation. Where a host
      // will not instantiate synchronously this object is retired instead,
      // and `compiler()` hands out a fresh one either way.
      instantiated = undefined;
      try {
        this.#exports = new WebAssembly.Instance(this.#module, {}).exports;
      } catch {
        this.#exports = null;
      }
      throw error;
    }
    try {
      const length = exports.kite_answer_length();
      const bytes = new Uint8Array(exports.memory.buffer, answer, length).slice();
      exports.kite_free(answer, length);
      return bytes;
    } finally {
      exports.kite_free(pointer, input.length);
    }
  }

  /**
   * The framing `kite_build` and `kite_check_module` read:
   *
   *     u32 count, then per entry: u32 name length, name, u32 body length, body
   *
   * Little-endian, which is what Wasm's memory is. The first entry is the
   * program, named by the path diagnostics should give it; the rest are its
   * siblings by module name.
   */
  #frame(entry, siblings, path) {
    const entries = [[path ?? "main", encoder.encode(entry)]];
    for (const [name, source] of Object.entries(siblings)) {
      entries.push([name, encoder.encode(source)]);
    }
    let length = 4;
    for (const [name, body] of entries) {
      length += 4 + encoder.encode(name).length + 4 + body.length;
    }
    const out = new Uint8Array(length);
    const view = new DataView(out.buffer);
    let at = 0;
    view.setUint32(at, entries.length, true);
    at += 4;
    for (const [name, body] of entries) {
      const encoded = encoder.encode(name);
      view.setUint32(at, encoded.length, true);
      at += 4;
      out.set(encoded, at);
      at += encoded.length;
      view.setUint32(at, body.length, true);
      at += 4;
      out.set(body, at);
      at += body.length;
    }
    return out;
  }
}

/** The same framing, read back. */
function unframe(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out = {};
  let at = 0;
  const count = view.getUint32(at, true);
  at += 4;
  for (let i = 0; i < count; i += 1) {
    const nameLength = view.getUint32(at, true);
    at += 4;
    const name = decoder.decode(bytes.subarray(at, at + nameLength));
    at += nameLength;
    const bodyLength = view.getUint32(at, true);
    at += 4;
    out[name] = bytes.subarray(at, at + bodyLength);
    at += bodyLength;
  }
  return out;
}
