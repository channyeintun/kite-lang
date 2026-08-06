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
    .then((bytes) => WebAssembly.instantiate(bytes, {}))
    .then(({ instance }) => new Compiler(instance.exports));
  return instantiated;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

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
  #exports;

  constructor(exports) {
    this.#exports = exports;
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
   * @param {{entry: string, siblings?: Record<string, string>}} module
   *   `siblings` is keyed by module name — `checkout`, not `checkout.kite`.
   */
  checkModule({ entry, siblings = {} }) {
    const answer = this.#call("kite_check_module", this.#frame(entry, siblings));
    return decoder.decode(answer);
  }

  /**
   * Compile a module the way `kitec build --emit wasm` does.
   *
   * @param {{
   *   entry: string,
   *   siblings?: Record<string, string>,
   *   release?: boolean,
   *   jsStrings?: boolean,
   * }} module
   * @returns {Record<string, Uint8Array>} `app.wasm`, `app.js`, `api.js` and
   *   `api.d.ts`, exactly as the native compiler writes them.
   * @throws {BuildFailed} with the diagnostics a terminal would have shown.
   */
  build({ entry, siblings = {}, release = false, jsStrings = false }) {
    const answer = this.#call(
      "kite_build",
      this.#frame(entry, siblings),
      release ? 1 : 0,
      jsStrings ? 1 : 0,
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
    const pointer = exports.kite_alloc(input.length);
    new Uint8Array(exports.memory.buffer, pointer, input.length).set(input);
    try {
      const answer = exports[name](pointer, input.length, ...args);
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
   * program; the rest are its siblings by module name.
   */
  #frame(entry, siblings) {
    const entries = [["main", encoder.encode(entry)]];
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
