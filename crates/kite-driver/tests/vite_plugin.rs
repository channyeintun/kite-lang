//! `vite-plugin-kite`, exercised without Vite.
//!
//! The plugin is a handful of hooks Vite calls — `configResolved`,
//! `resolveId`, `load`, `transformIndexHtml`, `handleHotUpdate` — and every
//! one of them is a plain function. So the tests call them the way Vite would,
//! under Node, with the compiler replaced by a stand-in through a module hook:
//! what is under test is which files the plugin agrees to compile and serve,
//! not the compiler, which has tests of its own. No `vite` package is needed,
//! which is what lets this run wherever the rest of the suite does.
//!
//! One run swaps `node:path` and `node:fs/promises` for Windows versions over
//! an in-memory disk, because the bug it pins only exists where a path is
//! spelled with backslashes and a Vite id is not.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::Workspace;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn plugin() -> PathBuf {
    plain(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/vite-plugin-kite/index.js")
            .canonicalize()
            .expect("the plugin"),
    )
}

/// A path as Node and a `file:` URL can take it.
///
/// `canonicalize` on Windows answers in the verbatim form, `\\?\D:\a\…` —
/// which Rust's file APIs accept and a URL cannot carry: it became
/// `file:////?/D:/a/…`, which Node refuses as not absolute. The prefix is
/// dropped, and `\\?\UNC\server\share` goes back to `\\server\share`.
fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{}", rest));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// Module hooks: the compiler becomes a stand-in, and — for the Windows run —
/// the plugin's `node:path` and `node:fs/promises` become Windows ones.
const HOOKS: &str = r#"
let windows = false;
export function initialize(data) {
  windows = data.windows;
}
export async function resolve(specifier, context, next) {
  if (specifier === "@kite-lang/compiler-wasm") {
    return { url: new URL("./compiler.mjs", import.meta.url).href, shortCircuit: true };
  }
  const fromPlugin = context.parentURL && context.parentURL.endsWith("/vite-plugin-kite/index.js");
  if (windows && fromPlugin && specifier === "node:path") {
    return { url: new URL("./winpath.mjs", import.meta.url).href, shortCircuit: true };
  }
  if (windows && fromPlugin && specifier === "node:fs/promises") {
    return { url: new URL("./winfs.mjs", import.meta.url).href, shortCircuit: true };
  }
  return next(specifier, context);
}
"#;

/// The compiler, standing in: every build produces the same four files. A
/// module whose source says `// no pub fn` gets no `api.js`, as `kitec`
/// writes none for it.
const COMPILER: &str = r#"
export class BuildFailed extends Error {}
export async function compiler() {
  return {
    build({ entry }) {
      const out = {
        "app.wasm": new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]),
        "app.js":
          'export async function instantiate(source) {\n' +
          '  return source ?? new URL("./app.wasm", import.meta.url);\n}\n' +
          "export function resident() {}\n",
        "app.wasm.map": new TextEncoder().encode('{"version":3}'),
      };
      if (!entry.includes("// no pub fn")) {
        out["api.js"] =
          'import { instantiate as $kiteInstantiate } from "./app.js";\n' +
          "export async function load(source) {\n" +
          '  return $kiteInstantiate(source ?? new URL("./app.wasm", import.meta.url));\n}\n';
      }
      return out;
    },
  };
}
"#;

const WINPATH: &str = r#"
import { win32 } from "node:path";
export const { dirname, join, resolve, basename, isAbsolute, sep } = win32;
export default win32;
"#;

/// `C:\proj\src\main.kite` and a sibling, and whatever the plugin writes.
const WINFS: &str = r#"
const files = new Map();
const key = (p) => String(p).replace(/\//g, "\\");
const disk = new Map([
  ["C:\\proj\\src\\main.kite", "pub fn f() {}\n"],
  ["C:\\proj\\src\\checkout.kite", "pub fn g() {}\n"],
]);
export async function mkdir() {}
export async function writeFile(p, body) {
  files.set(key(p), body);
}
export async function readFile(p) {
  const k = key(p);
  if (disk.has(k)) return disk.get(k);
  if (files.has(k)) return String(files.get(k));
  throw Object.assign(new Error("ENOENT " + k), { code: "ENOENT" });
}
export async function readdir(p) {
  if (key(p) === "C:\\proj\\src") return ["main.kite", "checkout.kite"];
  throw Object.assign(new Error("ENOENT " + key(p)), { code: "ENOENT" });
}
export async function stat(p) {
  if (disk.has(key(p))) return {};
  throw Object.assign(new Error("ENOENT " + key(p)), { code: "ENOENT" });
}
"#;

/// Write the stand-ins and a script into `dir`, run it, and return what it
/// printed.
fn run(dir: &Path, script: &str, windows: bool) -> String {
    std::fs::write(dir.join("hooks.mjs"), HOOKS).expect("hooks");
    std::fs::write(dir.join("compiler.mjs"), COMPILER).expect("compiler");
    std::fs::write(dir.join("winpath.mjs"), WINPATH).expect("winpath");
    std::fs::write(dir.join("winfs.mjs"), WINFS).expect("winfs");
    let main = format!(
        "import {{ register }} from \"node:module\";\n\
         register(\"./hooks.mjs\", import.meta.url, {{ data: {{ windows: {} }} }});\n\
         const kite = (await import({})).default;\n\
         {}",
        windows,
        serde_like_string(&url_of(&plugin())),
        script
    );
    std::fs::write(dir.join("run.mjs"), main).expect("script");
    let output = Command::new("node").arg(dir.join("run.mjs")).output().expect("node runs");
    assert!(
        output.status.success(),
        "the script failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

fn url_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{}", text)
    } else {
        format!("file:///{}", text)
    }
}

fn serde_like_string(s: &str) -> String {
    format!("{:?}", s)
}

/// The dev server compiles and serves a `.kite` file only from inside the root
/// or `server.fs.allow`, and an absolute path on disk only when a module names
/// it — never when a request does.
///
/// A request for `/<anywhere on disk>/secret.kite` was compiled and served,
/// and the `api.js` that came back made its `pub fn`s callable from the page,
/// while Vite refused `/@fs/` for the very same file.
#[test]
fn the_dev_server_serves_only_what_vite_would() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let work = Workspace::new("vite-boundary");
    let dir = work.path();
    let proj = dir.join("proj");
    let outside = dir.join("outside");
    std::fs::create_dir_all(proj.join("src")).expect("proj");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(proj.join("src/main.kite"), "pub fn f() {}\n").expect("main");
    std::fs::write(proj.join("src/page.kite"), "// no pub fn\nfn main() {}\n").expect("page");
    std::fs::write(outside.join("secret.kite"), "pub fn token() {}\n").expect("secret");
    let script = format!(
        r#"
const proj = {proj};
const outside = {outside};
const main = proj + "/src/main.kite";
const secret = outside + "/secret.kite";
const ctx = {{ addWatchFile() {{}}, error(m) {{ throw new Error(m); }} }};
const say = (label, value) => console.log(label + ": " + value);
const configure = (command, allow) => {{
  const p = kite();
  p.configResolved({{ root: proj, command, cacheDir: proj + "/node_modules/.vite",
    server: {{ fs: {{ strict: true, allow }} }} }});
  return p;
}};

const dev = configure("serve", [proj]);
say("request for an absolute path", await dev.resolveId.call(ctx, secret, undefined));
// What Vite 7 actually passes for a URL a browser asked for: the page.
say("request attributed to the page", await dev.resolveId.call(ctx, secret, proj + "/index.html"));
say("request climbing out of the root", await dev.resolveId.call(ctx, "/../outside/secret.kite", undefined));
say("request for a root-relative path", (await dev.resolveId.call(ctx, "/src/main.kite", undefined)) === main);
say("the entry stub's own import", (await dev.resolveId.call(ctx, main, main + "?kite-entry")) === main);
say("an id that skipped resolveId", await dev.load.call(ctx, secret));
try {{
  await dev.resolveId.call(ctx, secret, main);
  say("a module reaching outside", "resolved");
}} catch (e) {{
  say("a module reaching outside", e.message.includes("server.fs.allow"));
}}

const widened = configure("serve", [proj, outside]);
say("allowed by server.fs.allow", (await widened.resolveId.call(ctx, secret, main)) === secret);

const build = configure("build", [proj]);
say("a build importing outside", (await build.resolveId.call(ctx, secret, main)) === secret);

const code = await dev.load.call(ctx, main);
say("the module is pointed at Vite's URL", code.includes("__wasm") && !code.includes('new URL("./app.wasm"'));
const page = await dev.load.call(ctx, proj + "/src/page.kite");
say("a program with no pub fn still loads", /export async function load/.test(page) && page.includes("__wasm"));
const glueId = JSON.parse((code.match(/"\\u0000kite-glue:[^"]+"/) || ['""'])[0]);
const glue = await dev.load.call(ctx, glueId);
say("the glue is pointed at it too", glue.includes("__wasm") && !glue.includes('new URL("./app.wasm"'));
"#,
        proj = serde_like_string(&proj.to_string_lossy().replace('\\', "/")),
        outside = serde_like_string(&outside.to_string_lossy().replace('\\', "/")),
    );
    let out = run(dir, &script, false);
    assert_eq!(
        out,
        "request for an absolute path: null\n\
         request attributed to the page: null\n\
         request climbing out of the root: null\n\
         request for a root-relative path: true\n\
         the entry stub's own import: true\n\
         an id that skipped resolveId: null\n\
         a module reaching outside: true\n\
         allowed by server.fs.allow: true\n\
         a build importing outside: true\n\
         the module is pointed at Vite's URL: true\n\
         a program with no pub fn still loads: true\n\
         the glue is pointed at it too: true\n",
        "{}",
        out
    );
}

/// A `.kite` module script is found however its attributes are quoted.
///
/// Only `type="module"` and `src="…"` in double quotes were recognised, so a
/// page written `<script type=module src='/src/main.kite'>` loaded a program
/// that never started, and nothing said so. The pattern that fixed that ended
/// every value at whitespace, quoted or not, and so left a quoted name with a
/// space in it, `src="/src/my file.kite"`, alone in its turn.
#[test]
fn an_entry_is_found_however_it_is_quoted() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let work = Workspace::new("vite-html");
    let script = r#"
const p = kite();
const html = [
  '<script type="module" src="/src/a.kite"></script>',
  "<script type='module' src='/src/b.kite'></script>",
  "<script type=module src=/src/c.kite></script>",
  '<script src="/src/d.kite" type="module"></script>',
  '<script src="/src/e.kite"></script>',
  '<script type="module" src="/src/f.js"></script>',
  '<script type="module" src="/src/my file.kite"></script>',
  "<script type='module' src = '/src/our app.kite'></script>",
  "<script type=\"module\" src='/src/say \"hi\".kite'></script>",
  '<script type="module" src="/src/g.kite.js"></script>',
  "<script type=module src=/src/h.kite/></script>",
].join("\n");
console.log(p.transformIndexHtml.handler(html));
"#;
    let out = run(work.path(), script, false);
    assert_eq!(
        out,
        "<script type=\"module\" src=\"/src/a.kite?kite-entry\"></script>\n\
         <script type='module' src='/src/b.kite?kite-entry'></script>\n\
         <script type=module src=/src/c.kite?kite-entry></script>\n\
         <script src=\"/src/d.kite?kite-entry\" type=\"module\"></script>\n\
         <script src=\"/src/e.kite\"></script>\n\
         <script type=\"module\" src=\"/src/f.js\"></script>\n\
         <script type=\"module\" src=\"/src/my file.kite?kite-entry\"></script>\n\
         <script type='module' src = '/src/our app.kite?kite-entry'></script>\n\
         <script type=\"module\" src='/src/say \"hi\".kite?kite-entry'></script>\n\
         <script type=\"module\" src=\"/src/g.kite.js\"></script>\n\
         <script type=module src=/src/h.kite?kite-entry/></script>\n",
        "{}",
        out
    );
}

/// On Windows, editing a sibling rebuilds the module that reads its
/// directory.
///
/// The plugin recorded each module's directories with `path.join`, which
/// answers with backslashes there, and compared them with the directory of
/// the file Vite reported changed, which Vite spells with forward slashes. The
/// two never matched, so a sibling's edit reloaded nothing.
#[test]
fn a_sibling_edit_rebuilds_on_windows() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let work = Workspace::new("vite-windows");
    let script = r#"
const p = kite();
p.configResolved({ root: "C:/proj", command: "serve", cacheDir: "C:/proj/node_modules/.vite",
  server: { fs: { strict: true, allow: ["C:/proj"] } } });
const id = "C:/proj/src/main.kite";
await p.load.call({ addWatchFile() {} }, id);
const graph = new Map([[id, { id }]]);
const server = { moduleGraph: { getModuleById: (x) => graph.get(x) } };
const result = await p.handleHotUpdate({ file: "C:/proj/src/checkout.kite", server, modules: [] });
console.log(JSON.stringify(result));
"#;
    let out = run(work.path(), script, true);
    assert_eq!(out, "[{\"id\":\"C:/proj/src/main.kite\"}]\n", "{}", out);
}
