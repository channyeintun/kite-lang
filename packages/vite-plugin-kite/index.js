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

import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve, basename, sep } from "node:path";

import { compiler, BuildFailed } from "@kite-lang/compiler-wasm";

// The compiler is WebAssembly, so there is nothing to install and nothing to
// spawn.
//
// `@kite-lang/compiler-wasm` is the compiler itself — the same crate `kitec`
// is built from, targeting Wasm instead of the machine — and its output is
// byte-for-byte what the native compiler writes. So this is not "a second
// compiler for the bundler": there is one compiler, reached two ways, and a
// test in this repository holds them to identical bytes.
//
// What it buys over spawning a binary: one artefact for every platform with no
// `os`/`cpu` matrix, nothing fetched at install time, and a build that works
// in a browser-based Node such as WebContainer — which is what StackBlitz and
// Bolt run, and where native machine code cannot execute at all.

/// A `.kite` import.
const KITE = /\.kite$/;

/// A `.kite` named as a page's entry, rather than imported by something.
///
/// `<script type="module" src="/src/main.kite">` is the whole wiring: the
/// plugin adds this marker in the HTML, and the module it loads for it starts
/// the program. A project using Vite should not have to keep a JavaScript file
/// whose only job is to call the thing it just compiled.
const ENTRY = "?kite-entry";

/// The glue is a second module rather than being concatenated onto the first.
///
/// `api.js` imports `instantiate`, `str` and `text` from `app.js`, and joining
/// the two files would work right up to the first time a generated name in one
/// collided with a name in the other. Two modules cost nothing and cannot.
const GLUE = "\0kite-glue:";

/**
 * @param {object} [options]
 * @param {boolean} [options.release] Build with `--release`: `assert` is
 *   dropped and `require` is not. Follows Vite's mode when not given.
 * @param {boolean} [options.jsStrings] Build with `--js-strings`, so a `str`
 *   is a real JavaScript string. Faster across the boundary, and it will not
 *   instantiate in an engine without the JS String Builtins proposal — which
 *   is why it is off unless asked for.
 */
export default function kite(options = {}) {
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

  /// Whether a glue id names output this plugin actually produced.
  ///
  /// The path in a `\0kite-glue:` id is not this plugin's word — in a dev
  /// server it can come from the browser. Vite's transform middleware turns a
  /// request for `/@id/__x00__kite-glue:/anywhere` back into exactly this id
  /// before any plugin sees it, so an id is untrusted input wearing a
  /// trusted-looking prefix. Reading whatever path it names handed out any
  /// `app.js` on the machine — a common name for a Node entrypoint, and one
  /// that tends to hold credentials — from outside the Vite root and outside
  /// `server.fs.allow`, which are the two boundaries meant to make that
  /// impossible.
  ///
  /// Checked against the cache directory rather than against a list of what
  /// has been built, because a warm dev server can serve a cached transform
  /// whose glue import is requested before the `.kite` module is loaded again.
  const producedHere = (dir) => {
    if (!cacheDir) return false;
    const root = resolve(cacheDir);
    const target = resolve(dir);
    return target === root || target.startsWith(root + sep);
  };

  async function compile(file) {
    const out = outputFor(file);
    await mkdir(out, { recursive: true });

    // A Kite module is a directory, and the compiler has no directory to read
    // from here — so its siblings are handed over by module name, which is the
    // filename without its extension, because that is what `use checkout`
    // names.
    const sources = {};
    for (const path of await siblings(file)) {
      if (path === file) continue;
      sources[basename(path, ".kite")] = await readFile(path, "utf8");
    }

    let artefacts;
    try {
      artefacts = (await compiler()).build({
        entry: await readFile(file, "utf8"),
        siblings: sources,
        release,
        jsStrings: options.jsStrings,
      });
    } catch (e) {
      // The diagnostics are the useful part, and they are already rendered the
      // way a terminal renders them.
      if (e instanceof BuildFailed) {
        throw new Error(`${file} did not compile:\n\n${e.diagnostics.trim()}`);
      }
      throw e;
    }

    await Promise.all(
      Object.entries(artefacts).map(([name, body]) => writeFile(join(out, name), body)),
    );
    return out;
  }

  /// Where an import actually is on disk.
  ///
  /// A path from HTML is **root-relative** — `<script src="/src/main.kite">`
  /// means `<root>/src/main.kite`, not a file at the top of the filesystem —
  /// and a path from another module is relative to that module. Resolving the
  /// first as though it were absolute is how the entry came back as
  /// `cannot read /src/main.kite`.
  async function locate(source, importer) {
    const candidates = source.startsWith("/")
      ? [join(root, source), source]
      : [importer ? resolve(dirname(importer), source) : resolve(root, source)];
    for (const path of candidates) {
      if (await stat(path).then(() => true, () => false)) return path;
    }
    return candidates[0];
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

    /// `<script type="module" src="…​.kite">` becomes the program's entry.
    ///
    /// Marked rather than rewritten to a generated file, so what a reader sees
    /// in the HTML is the file that actually runs. Only a `type="module"`
    /// script is touched, and only its `src`.
    ///
    /// **`order: "pre"`**, and it is not a preference. Vite reads the HTML for
    /// its entry points before the default transforms run, so a rewrite that
    /// happened afterwards changed the markup and not the build: the original
    /// module became the entry, `start` was exported and never called, and the
    /// page loaded a program that did nothing. There was no error — the module
    /// was there, and nothing asked it to run.
    transformIndexHtml: {
      order: "pre",
      handler(html) {
        return html.replace(
          /<script\b[^>]*>/g,
          (tag) =>
            tag.includes('type="module"')
              ? tag.replace(/(\bsrc=")([^"]+\.kite)(")/, `$1$2${ENTRY}$3`)
              : tag,
        );
      },
    },

    async resolveId(source, importer) {
      if (source.startsWith(GLUE)) {
        return producedHere(source.slice(GLUE.length)) ? source : null;
      }
      const entry = source.endsWith(ENTRY);
      const bare = entry ? source.slice(0, -ENTRY.length) : source;
      if (!KITE.test(bare)) return null;
      const file = await locate(bare, importer);
      if (!file) return null;
      return entry ? file + ENTRY : file;
    },

    async load(id) {
      if (id.startsWith(GLUE)) {
        const dir = id.slice(GLUE.length);
        if (!producedHere(dir)) return null;
        return readFile(join(dir, "app.js"), "utf8");
      }
      // The entry module is two lines and they are generated, which is the
      // point: a `.kite` page has no JavaScript in its source at all.
      if (id.endsWith(ENTRY)) {
        const file = id.slice(0, -ENTRY.length);
        return `import { start } from ${JSON.stringify(file)};\nawait start();\n`;
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
      //   * `start()` is added, for the shape where the Kite program owns its
      //     own part of the page rather than being called into. That program
      //     uses `std/dom` and never crosses the typed wrapper at all, so what
      //     it needs is what the site's own pages do: instantiate, register as
      //     resident so listeners and tasks keep running after `main` returns,
      //     and call `main`.
      return (
        `import __wasm from ${JSON.stringify(wasm + "?url")};\n` +
        `import { resident as __resident } from ${JSON.stringify(GLUE + out)};\n` +
        api
          .replace(/from "\.\/app\.js"/, `from ${JSON.stringify(GLUE + out)}`)
          .replace(/export async function load\(source = "app\.wasm"\)/,
                   "export async function load(source = __wasm)") +
        `
/// Instantiate and run \`main\`, for a program that owns its own page.
///
/// \`resident\` is what keeps a program alive after \`main\` returns — an event
/// listener or a task has nothing holding it up otherwise.
export async function start(source = __wasm) {
  const exports = await load(source);
  __resident(exports);
  if (typeof exports.main === "function") exports.main();
  return exports;
}
`
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
