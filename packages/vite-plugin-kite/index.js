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
import { dirname, join, resolve, basename, isAbsolute, sep } from "node:path";

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

/// A path with forward slashes, which is how Vite spells every id and every
/// file it reports — on Windows too. `node:path` answers with backslashes
/// there, so a directory recorded from `join` never equalled one read back
/// from a Vite id, and an edit to a sibling rebuilt nothing.
const slash = (path) => path.replace(/\\/g, "/");

/// Whether `file` is `dir` or inside it, compared the way Vite compares.
const within = (dir, file) => {
  const d = slash(resolve(dir)).replace(/\/$/, "");
  const f = slash(resolve(file));
  return f === d || f.startsWith(d + "/");
};

/// The glue and the wrapper name the module beside them, which in a Vite
/// build is not where it goes: both are pointed at the URL Vite gives it.
const BESIDE = /new URL\("\.\/app\.wasm", import\.meta\.url\)/g;

/// What a program with no `pub fn` of its own is imported as: `kitec` writes
/// no `api.js` for it, and the page still needs `load`.
const LOADER =
  'import { instantiate as $kiteInstantiate } from "./app.js";\n' +
  "let $kiteModule = null;\n" +
  "export async function load(source) {\n" +
  '  $kiteModule = await $kiteInstantiate(source ?? new URL("./app.wasm", import.meta.url));\n' +
  "  return $kiteModule;\n" +
  "}\n";

/**
 * @param {object} [options]
 * @param {boolean} [options.release] Build with `--release`: `assert` is
 *   dropped and `require` is not. Follows Vite's mode when not given.
 */
export default function kite(options = {}) {
  let root = process.cwd();
  let release = options.release;
  let cacheDir;
  /// Whether this is the dev server, which answers requests from a browser,
  /// and the directories it may answer with: the root and
  /// `server.fs.allow`, which is what Vite itself holds `/@fs/` to.
  let serving = false;
  let allowed = [];
  let strict = true;
  /**
   * Per source file: where its output went, and every directory its meaning
   * depends on — its own, and each declared dependency's. An edit anywhere in
   * those has to rebuild it.
   */
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
    // from here — so its siblings are handed over by module path, which for a
    // file beside the entry is the filename without its extension, because
    // that is what `use checkout` names.
    const sources = {};
    for (const path of await siblings(file)) {
      if (path === file) continue;
      sources[basename(path, ".kite")] = await readFile(path, "utf8");
    }
    // And everything the manifest declares, under its own name, so
    // `use markdown/render` reaches inside the package rather than beside the
    // entry file. Nothing is fetched: this reads `.kite/vendor`, which
    // `kitec pkg` filled, and a `path =` dependency where it already is.
    const vendored = await dependencySources(file);
    for (const [path, key] of vendored) {
      sources[key] = await readFile(path, "utf8");
    }

    let artefacts;
    try {
      artefacts = (await compiler()).build({
        entry: await readFile(file, "utf8"),
        siblings: sources,
        release,
        path: basename(file),
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
    return { out, files: vendored.map(([path]) => path) };
  }

  /// Where an import actually is on disk, or `null` when it is nowhere this
  /// plugin may read.
  ///
  /// A path from HTML is **root-relative** — `<script src="/src/main.kite">`
  /// means `<root>/src/main.kite`, not a file at the top of the filesystem —
  /// and a path from another module is relative to that module. Resolving the
  /// first as though it were absolute is how the entry came back as
  /// `cannot read /src/main.kite`.
  ///
  /// **An absolute path on disk is taken only from a module**, never from a
  /// request. The dev server hands a URL to `resolveId` as though the page's
  /// HTML had imported it, and a URL naming `/home/you/elsewhere/secret.kite`
  /// used to be compiled and served — with its `pub fn`s callable through the
  /// `api.js` that came back — while Vite itself refused `/@fs/` for the same
  /// file. The entry stub below imports its program by absolute path, and is
  /// a module.
  ///
  /// What it finds is spelled with forward slashes, as Vite spells an id: on
  /// Windows `join` answers with backslashes, and a module whose id differs
  /// from the one Vite records for the same file is a second module.
  async function locate(source, importer) {
    const exists = (path) => stat(path).then(() => true, () => false);
    if (source.startsWith("/") || isAbsolute(source)) {
      const fromRoot = join(root, source);
      if (within(root, fromRoot) && (await exists(fromRoot))) return slash(fromRoot);
      if (fromModule(importer) && isAbsolute(source) && (await exists(source))) return slash(source);
      return null;
    }
    const path = importer ? resolve(dirname(importer), source) : resolve(root, source);
    return (await exists(path)) ? slash(path) : null;
  }

  /// Whether an import came from a module of the project's, rather than from
  /// a page — which is what the dev server says a request came from.
  const fromModule = (importer) => Boolean(importer) && !/\.html?$/.test(importer);

  /// Whether the dev server may hand out what `file` compiles to: inside the
  /// root or `server.fs.allow`, the boundary Vite draws for every other file.
  /// A build reads what the project imports and serves nothing.
  const servable = (file) => !serving || !strict || allowed.some((dir) => within(dir, file));

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

  /// The `kite.toml` governing a file, found by walking upwards.
  ///
  /// Upwards because a program is usually `src/main.kite` and the manifest is
  /// beside `src/` — the same search `kitec` does, so a project means the same
  /// thing built either way.
  async function manifestNear(file) {
    let dir = dirname(file);
    for (;;) {
      const path = join(dir, "kite.toml");
      const text = await readFile(path, "utf8").catch(() => null);
      if (text !== null) return { dir, text };
      const up = dirname(dir);
      if (up === dir) return null;
      dir = up;
    }
  }

  /// The dependencies a manifest declares, as name to directory.
  ///
  /// A deliberately small reader, for the deliberately small subset `kitec`
  /// accepts: `[dependencies]`, one `name = { … }` per line. A manifest that
  /// needs more than this is a manifest that has grown a programming language.
  /// Anything it cannot read it ignores rather than guesses at — the compiler
  /// is the authority on the file, and a build that stopped on a key this
  /// parser had not heard of would make the bundler a second opinion.
  function dependencyDirs(manifest) {
    const dirs = new Map();
    let table = "";
    for (const raw of manifest.text.split("\n")) {
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
      const body = line.slice(at + 1);
      const path = /\bpath\s*=\s*"([^"]*)"/.exec(body);
      // A git dependency lives where `kitec pkg` cloned it. Nothing is
      // fetched here, and a name that was never vendored simply has no files
      // — which the compiler reports as the missing module it is.
      dirs.set(name, path
        ? resolve(manifest.dir, path[1])
        : join(manifest.dir, ".kite", "vendor", name));
    }
    return dirs;
  }

  /// Every `.kite` file in every declared dependency, with the module path it
  /// answers to: `[absolute file, "markdown/render"]`.
  ///
  /// One level deep, which is a package's own modules and not its
  /// dependencies' — transitive packages need `kitec pkg` to have flattened
  /// them into this project's vendor directory, which is what it does.
  async function dependencySources(file) {
    const manifest = await manifestNear(file);
    if (manifest === null) return [];
    const found = [];
    for (const [name, dir] of dependencyDirs(manifest)) {
      const names = await readdir(dir).catch(() => []);
      for (const entry of names) {
        if (!KITE.test(entry)) continue;
        found.push([join(dir, entry), `${name}/${basename(entry, ".kite")}`]);
      }
    }
    return found;
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
      serving = config.command === "serve";
      strict = config.server?.fs?.strict !== false;
      allowed = [root, ...(config.server?.fs?.allow ?? [])];
    },

    /// `<script type="module" src="…​.kite">` becomes the program's entry.
    ///
    /// Marked rather than rewritten to a generated file, so what a reader sees
    /// in the HTML is the file that actually runs. Only a module script is
    /// touched, and only its `src` — however the attributes are quoted: HTML
    /// takes `type=module` and `src='…'` as readily as double quotes, and a
    /// tag written that way used to be left alone, so the page loaded a
    /// program that never started and said nothing.
    ///
    /// A quoted value runs to its closing quote and may hold a space, as in
    /// `src="/src/my file.kite"`; only an unquoted one ends at whitespace. One
    /// pattern for all three stopped a quoted value at its first space too,
    /// and that page was left alone again.
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
        return html.replace(/<script\b[^>]*>/gi, (tag) =>
          /\stype\s*=\s*(["']?)module\1(?=[\s>/])/i.test(tag)
            ? tag.replace(
                /(\ssrc\s*=\s*)(?:"([^"]+\.kite)"|'([^']+\.kite)'|([^"'\s>]+\.kite)(?=[\s>/]))/i,
                (_, name, double, single, bare) =>
                  double !== undefined
                    ? `${name}"${double}${ENTRY}"`
                    : single !== undefined
                      ? `${name}'${single}${ENTRY}'`
                      : `${name}${bare}${ENTRY}`,
              )
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
      if (!servable(file)) {
        // A request is answered as though there were nothing there. A module
        // of the project's own reaching outside is told why, since the fix
        // is theirs to make.
        if (!fromModule(importer)) return null;
        this.error(
          `${file} is outside the Vite root and server.fs.allow, so the dev server ` +
            `will not serve it — add its directory to server.fs.allow`,
        );
      }
      return entry ? file + ENTRY : file;
    },

    async load(id) {
      if (id.startsWith(GLUE)) {
        const dir = id.slice(GLUE.length);
        if (!producedHere(dir)) return null;
        const glue = await readFile(join(dir, "app.js"), "utf8");
        return (
          `import __wasm from ${JSON.stringify(join(dir, "app.wasm") + "?url")};\n` +
          glue.replace(BESIDE, "__wasm")
        );
      }
      // The entry module is two lines and they are generated, which is the
      // point: a `.kite` page has no JavaScript in its source at all.
      if (id.endsWith(ENTRY)) {
        const file = id.slice(0, -ENTRY.length);
        if (!servable(file)) return null;
        return `import { start } from ${JSON.stringify(file)};\nawait start();\n`;
      }
      if (!KITE.test(id)) return null;
      // An id can arrive without `resolveId` having passed it — `/@id/` is
      // the dev server's way of naming one — so the boundary is held here too.
      if (!servable(id)) return null;

      const { out, files } = await compile(id);
      const own = await siblings(id);
      built.set(id, { out, dirs: new Set([...own, ...files].map((f) => slash(dirname(f)))) });

      // A dependency's files are watched exactly as siblings are: they are as
      // much a part of what this module means, and a package edited in place
      // — which is what a `path =` dependency is for — should show up in the
      // browser without restarting the server.
      for (const s of own) this.addWatchFile(s);
      for (const f of files) this.addWatchFile(f);

      const api = await readFile(join(out, "api.js"), "utf8").catch(() => LOADER);
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
          .replace(BESIDE, "__wasm") +
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

    /// An edit to any `.kite` file rebuilds every module that reads its
    /// directory.
    ///
    /// Because a Kite module is a directory, changing one file can change what
    /// its siblings mean — so the unit of invalidation is the directory rather
    /// than the file, and a rename or a new file counts as well as an edit.
    /// A dependency's directory counts the same way: a package edited in place
    /// changes every program that declared it, and those are usually not in
    /// the directory that changed.
    async handleHotUpdate({ file, server, modules }) {
      if (!KITE.test(file)) return;
      const dir = slash(dirname(file));
      const affected = [];
      for (const [source, { dirs }] of built) {
        if (!dirs.has(dir)) continue;
        const mod = server.moduleGraph.getModuleById(source);
        if (mod) affected.push(mod);
      }
      return affected.length > 0 ? affected : modules;
    },
  };
}
