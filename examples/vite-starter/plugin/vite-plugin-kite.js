// Import `.kite` files from a Vite project.
//
// Kite compiles to WebAssembly, and `kitec build` already writes everything a
// bundler needs: the module, the glue that instantiates it, and `api.js` — the
// typed door, with every `pub fn` converted so a JavaScript caller sees
// ordinary values. This plugin's whole job is to run that at the right moment
// and hand the result to Vite as a module.
//
//     import kite from "vite-plugin-kite";
//     export default { plugins: [kite()] };
//
//     import { load, add } from "./adder.kite";
//     await load();
//     add(2n, 3n);
//
// **It is not a framework and does not want to be one.** There is no runtime
// here, nothing injected into your app, and no opinion about how you structure
// it. What you import is what `kitec` produced.

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));

// ---- the compiler ----------------------------------------------------------
//
// **Nothing has to be installed.** `kitec.wasm` beside this file is the
// compiler — the same Rust, built for WebAssembly — so `npm install` is the
// whole setup, on every platform, with no native binary and no download.
//
// A native `kitec` is used when one is there, because it is faster. That is an
// optimisation and not a requirement, and the difference is invisible except
// in the time a build takes.

/// Compile with the WebAssembly build of the compiler.
///
/// The boundary is a pointer and a length each way. `kite_build` answers with
/// every file `kitec build --emit wasm` would have written, framed as
///
///     u32 count, then for each: u32 name length, name, u32 body length, body
///
/// or with a single `diagnostics` entry when the program did not compile.
export class WasmCompiler {
  #exports = null;

  async load() {
    if (this.#exports) return this.#exports;
    const bytes = await readFile(join(here, "kitec.wasm"));
    const { instance } = await WebAssembly.instantiate(bytes, {});
    this.#exports = instance.exports;
    return this.#exports;
  }

  /// One of the exports that answers with text: `kite_check`, `kite_format`.
  async text(name, source) {
    const w = await this.load();
    const memory = () => new Uint8Array(w.memory.buffer);
    const input = new TextEncoder().encode(source);
    const at = w.kite_alloc(input.length);
    memory().set(input, at);
    const answer = w[name](at, input.length);
    const length = w.kite_answer_length();
    const out = new TextDecoder().decode(memory().slice(answer, answer + length));
    w.kite_free(answer, length);
    w.kite_free(at, input.length);
    return out;
  }

  async build(source, release) {
    const w = await this.load();
    const memory = () => new Uint8Array(w.memory.buffer);
    const input = new TextEncoder().encode(source);
    const at = w.kite_alloc(input.length);
    memory().set(input, at);

    const answer = w.kite_build(at, input.length, release ? 1 : 0);
    const length = w.kite_answer_length();
    // Copied out before anything else runs: a later allocation can grow the
    // module's memory, and growing it detaches every view onto the old buffer.
    const framed = memory().slice(answer, answer + length);
    w.kite_free(answer, length);
    w.kite_free(at, input.length);

    const view = new DataView(framed.buffer, framed.byteOffset, framed.byteLength);
    const decoder = new TextDecoder();
    const files = new Map();
    let cursor = 4;
    for (let i = 0; i < view.getUint32(0, true); i += 1) {
      const nameLength = view.getUint32(cursor, true);
      cursor += 4;
      const name = decoder.decode(framed.subarray(cursor, cursor + nameLength));
      cursor += nameLength;
      const bodyLength = view.getUint32(cursor, true);
      cursor += 4;
      files.set(name, framed.subarray(cursor, cursor + bodyLength));
      cursor += bodyLength;
    }
    return files;
  }
}

/// A `.kite` import, absolute, with any Vite suffix (`?url`, `?raw`) removed.
const KITE = /\.kite$/;

/// The glue is a second module rather than being concatenated onto the first.
///
/// `api.js` imports `instantiate`, `str` and `text` from `app.js`, and joining
/// the two files would work right up to the first time a generated name in one
/// collided with a name in the other. Two modules cost nothing and cannot.
const GLUE = "\0kite-glue:";

/**
 * @param {object} [options]
 * @param {string} [options.bin] A native compiler to use instead of looking
 *   for `kitec` on `PATH`. Neither is required: the compiler ships with this
 *   package as WebAssembly and is used when no native one is found.
 * @param {boolean} [options.release] Build with `--release`: `assert` is
 *   dropped and `require` is not. Follows Vite's mode when not given.
 * @param {boolean} [options.jsStrings] Build with `--js-strings`, so a `str`
 *   is a real JavaScript string. Faster across the boundary, and it will not
 *   instantiate in an engine without the JS String Builtins proposal — which
 *   is why it is off unless asked for.
 */
export default function kite(options = {}) {
  const bin = options.bin ?? "kitec";
  const wasm = new WasmCompiler();
  /// Whether a native `kitec` answered. Decided once, on the first build.
  let native = options.bin ? true : null;
  let root = process.cwd();
  let release = options.release;
  let cacheDir;
  /** Compiled output per source file, so one edit rebuilds one module. */
  const built = new Map();

  /// Where the compiler's output goes.
  ///
  /// Under `node_modules` so it is inside the project — Vite will not serve a
  /// file from outside the root without being told to, and a temp directory
  /// would have to be — and keyed by the source path so two `.kite` files with
  /// the same basename do not overwrite each other.
  const outputFor = (file) => {
    const key = createHash("sha256").update(file).digest("hex").slice(0, 12);
    return join(cacheDir, `${basename(file, ".kite")}-${key}`);
  };

  async function compile(file) {
    const out = outputFor(file);
    await mkdir(out, { recursive: true });

    if (native !== false) {
      const args = [
        "build",
        file,
        "--emit",
        "wasm",
        "--out",
        out,
        ...(release ? ["--release"] : []),
        ...(options.jsStrings ? ["--js-strings"] : []),
      ];
      try {
        await run(bin, args, { cwd: root });
        native = true;
        return out;
      } catch (e) {
        if (e.code !== "ENOENT") {
          // `kitec` writes diagnostics to stderr and they are the useful part;
          // the exit status is not.
          throw new Error(
            `${file} did not compile:\n\n${(e.stderr || e.stdout || e.message).trim()}`,
          );
        }
        if (options.bin) {
          throw new Error(
            `vite-plugin-kite: cannot run \`${bin}\`.\n` +
              "That is the `bin` this plugin was given. Remove it to use the " +
              "WebAssembly compiler that ships with this package.",
          );
        }
        // No `kitec` on `PATH`, which is the ordinary case and not a problem:
        // the compiler ships with this package. Decided once rather than on
        // every file.
        native = false;
      }
    }

    const files = await wasm.build(await readFile(file, "utf8"), release);
    const diagnostics = files.get("diagnostics");
    if (diagnostics) {
      throw new Error(`${file} did not compile:\n\n${new TextDecoder().decode(diagnostics).trim()}`);
    }
    await Promise.all(
      [...files].map(([name, body]) => writeFile(join(out, name), body)),
    );
    return out;
  }

  /// Every `.kite` file beside this one.
  ///
  /// A module in Kite is a *directory*, so a program's meaning depends on its
  /// siblings and an edit to any of them has to rebuild it. Vite is told to
  /// watch them for that reason.
  async function siblings(file) {
    try {
      const dir = dirname(file);
      const names = await readdir(dir);
      return names.filter((n) => KITE.test(n)).map((n) => join(dir, n));
    } catch {
      return [file];
    }
  }

  return {
    name: "vite-plugin-kite",
    // Ahead of Vite's own asset handling, so `.kite` never reaches it as a
    // file to copy.
    enforce: "pre",

    /// A `.wasm` is never inlined as a data URI.
    ///
    /// Vite inlines an asset under `assetsInlineLimit` (4 KB by default), and
    /// a small Kite module is under it — `hello world` is 400 bytes. Base64
    /// costs a third more bytes than the thing it encodes, the module can no
    /// longer be cached or streamed on its own, and the behaviour would flip
    /// the day a module grew past the limit. None of that is a trade worth
    /// making silently, so it is turned off for `.wasm` and left alone for
    /// everything else — including whatever the project already set.
    config(user) {
      const existing = user.build?.assetsInlineLimit;
      return {
        build: {
          assetsInlineLimit(filePath, content) {
            if (filePath.endsWith(".wasm")) return false;
            if (typeof existing === "function") return existing(filePath, content);
            if (typeof existing === "number") return content.length < existing;
            return undefined;
          },
        },
      };
    },

    configResolved(config) {
      root = config.root;
      release ??= config.command === "build";
      cacheDir = join(config.cacheDir ?? join(root, "node_modules/.vite"), "kite");
    },

    async resolveId(source, importer) {
      if (source.startsWith(GLUE)) return source;
      if (!KITE.test(source)) return null;
      const file = importer ? resolve(dirname(importer), source) : resolve(root, source);
      return file;
    },

    async load(id) {
      if (id.startsWith(GLUE)) {
        return readFile(join(id.slice(GLUE.length), "app.js"), "utf8");
      }
      if (!KITE.test(id)) return null;

      const out = await compile(id);
      built.set(id, out);

      for (const s of await siblings(id)) this.addWatchFile(s);

      const api = await readFile(join(out, "api.js"), "utf8");
      const wasm = join(out, "app.wasm");

      // Two rewrites, and both are about letting Vite do its job rather than
      // this plugin doing it badly:
      //
      //   * the glue becomes a virtual module, so it is not read off disk by a
      //     browser that has no idea where the cache directory is;
      //   * the module is imported with `?url`, so Vite serves it in dev and
      //     emits it hashed and fingerprinted in a build. Nothing here has to
      //     know which of the two is happening.
      return (
        `import __wasm from ${JSON.stringify(wasm + "?url")};\n` +
        api
          .replace(/from "\.\/app\.js"/, `from ${JSON.stringify(GLUE + out)}`)
          .replace(/export async function load\(source = "app\.wasm"\)/,
                   "export async function load(source = __wasm)")
      );
    },

    /// An edit to any `.kite` file rebuilds every module in its directory.
    ///
    /// Because a Kite module is a directory, changing one file can change what
    /// its siblings mean — so the unit of invalidation is the directory rather
    /// than the file, and a rename or a new file counts as well as an edit.
    async handleHotUpdate({ file, server, modules }) {
      if (!KITE.test(file)) return;
      const dir = dirname(file);
      const affected = [];
      for (const [source] of built) {
        if (dirname(source) !== dir) continue;
        const mod = server.moduleGraph.getModuleById(source);
        if (mod) affected.push(mod);
      }
      return affected.length > 0 ? affected : modules;
    },
  };
}
