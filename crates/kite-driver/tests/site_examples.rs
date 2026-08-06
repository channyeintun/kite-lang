//! Every Kite example on the site compiles.
//!
//! A documentation example that stops working should fail the build rather
//! than sit there wrong. The site's pages hold their examples in
//! `<code class="kite">` blocks and the playground holds its samples in a
//! table, so both are read out of the files themselves — an example added to a
//! page is an example this checks, with nothing to remember.

use kite_driver::{compile, Emit};
use std::path::{Path, PathBuf};

fn site() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site")
}

/// A page, with its line endings settled. Git hands Windows a checkout with
/// CRLF endings, and the readers below match on newlines — a sample ends at
/// "`,\n" — so a page read raw there parses as no samples at all rather than
/// as a failure anyone can read.
fn page(name: &str) -> String {
    let path = site().join(name);
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!("no {} at {}", name, path.display());
    };
    text.replace("\r\n", "\n")
}

/// The Kite in a page: every `<code class="kite">…</code>` block.
fn code_blocks(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    // `class="kite sketch"` marks a block that stands for code in a program
    // that does not exist here — the one in the pitch that calls a `config`
    // module nobody wrote. Everything else has to compile.
    while let Some(start) = rest.find("<code class=\"kite\">") {
        rest = &rest[start + "<code class=\"kite\">".len()..];
        let Some(end) = rest.find("</code>") else { break };
        let block = &rest[..end];
        rest = &rest[end..];
        out.push(
            block
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        );
    }
    out
}

/// The playground's samples, which are JavaScript template literals in an
/// object keyed by name.
fn playground_samples(js: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = js;
    while let Some(start) = rest.find("\": `") {
        let name_start = rest[..start].rfind('"').unwrap_or(0);
        let name = rest[name_start + 1..start].to_string();
        rest = &rest[start + 4..];
        let Some(end) = rest.find("`,\n") else { break };
        let body = &rest[..end];
        rest = &rest[end..];
        // The samples are template literals, so the escapes JavaScript needed
        // come back out: `\\(` is Kite's interpolation and `\``  a backtick.
        out.push((name, body.replace("\\\\(", "\\(").replace("\\`", "`")));
    }
    out
}

#[test]
fn every_example_on_the_site_compiles() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for name in ["index.html", "playground.html"] {
        let html = page(name);
        for (i, block) in code_blocks(&html).into_iter().enumerate() {
            // A fragment rather than a program — an expression, or a couple of
            // lines out of a function — is wrapped so it can be checked at
            // all.
            let src = if block.contains("fn ") { block.clone() } else {
                format!("fn main() {{\n{}\n}}\n", block)
            };
            let compiled = compile("site.kite", &src, Emit::Check);
            if compiled.failed() {
                failures.push(format!(
                    "{} block {}:\n{}\n{}",
                    name,
                    i,
                    block,
                    compiled.render_diagnostics()
                ));
            }
            checked += 1;
        }
    }

    let js = page("playground.html");
    let samples = playground_samples(&js);
    assert!(samples.len() >= 5, "only {} samples found", samples.len());
    for (name, src) in samples {
        let compiled = compile("sample.kite", &src, Emit::Check);
        if compiled.failed() {
            failures.push(format!(
                "playground sample {:?}:\n{}",
                name,
                compiled.render_diagnostics()
            ));
        }
        checked += 1;
    }

    assert!(checked >= 6, "only {} examples were found on the site", checked);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The samples are what a first-time visitor runs, so they must also *run* —
/// a sample that compiles and then traps is worse than none.
#[test]
fn every_playground_sample_runs() {
    let samples = playground_samples(&page("playground.html"));
    // Reading no samples is a silent pass for a loop, which is how this test
    // stayed green on Windows while checking nothing at all.
    assert!(samples.len() >= 5, "only {} samples found", samples.len());
    for (name, src) in samples {
        let compiled = compile("sample.kite", &src, Emit::Check);
        assert!(
            !compiled.failed(),
            "{:?} does not compile:\n{}",
            name,
            compiled.render_diagnostics()
        );
        let mut out = Vec::new();
        compiled
            .run(&mut out)
            .unwrap_or_else(|t| panic!("{:?} trapped: {}", name, t));
        assert!(
            !String::from_utf8_lossy(&out).is_empty(),
            "{:?} printed nothing",
            name
        );
    }
}

/// The site's own program compiles.
///
/// `site/src/` is the site: the Markdown rendering, the syntax colouring, the
/// navigation and the fetching, compiled to the `app.wasm` every page
/// instantiates. It is the largest Kite program in the repository that is not
/// the standard library, and it is written against the same `std/dom` and
/// `std/js` a user has — which is the claim it exists to test.
///
/// Checked for the **web** target rather than with `Emit::Check`, because
/// `std/js` is web-only and a checker that never lowered it would not have
/// said whether the thing a browser runs is the thing that compiles.
#[test]
fn the_sites_own_program_compiles_for_the_web() {
    let entry = site().join("src/main.kite");
    let src = std::fs::read_to_string(&entry)
        .unwrap_or_else(|e| panic!("read {}: {}", entry.display(), e));
    let compiled = kite_driver::compile(&entry, &src, Emit::Wasm);
    assert!(
        !compiled.failed(),
        "the site does not compile:\n{}",
        compiled.render_diagnostics()
    );
    let module = compiled.wasm.as_ref().expect("a module");
    // The budget the pages are held to. Every document on the site goes
    // through this module, so it is downloaded before anything is read.
    assert!(
        module.bytes.len() < 49152,
        "the site's program is {} bytes, budget 49152",
        module.bytes.len()
    );
}

/// The Vite starter's Kite compiles, and every export crosses the boundary.
///
/// The starter is the adoption story made concrete: a normal web project that
/// imports a `.kite` file. It only holds up if two things are true — the Kite
/// compiles for the web, and the generated wrapper can actually *describe*
/// every `pub fn` in it.
///
/// The second is the one that bites. Only `int`, `float`, `bool` and `str`
/// cross; a slice, an `Option<T>` or a `(T, error)` pair is exported by the
/// module and left out of `api.js`. My first draft of this starter took a
/// `[int]` and returned a `(int, error)`, and the build failed with
/// `"total" is not exported` — after the plugin had done everything right.
/// A starter whose functions are invisible to its own caller teaches the
/// wrong lesson twice.
#[test]
fn the_vite_starter_compiles_and_every_export_crosses() {
    let entry = site()
        .join("../examples/vite-starter/src/checkout.kite")
        .canonicalize()
        .expect("the starter's Kite exists");
    let src = std::fs::read_to_string(&entry).expect("read the starter");
    let compiled = kite_driver::compile(&entry, &src, Emit::Wasm);
    assert!(
        !compiled.failed(),
        "the Vite starter does not compile:\n{}",
        compiled.render_diagnostics()
    );

    let module = compiled.wasm.as_ref().expect("a module");
    let (api_js, _) = kite_driver::generate_api(&module.api, "app.wasm");
    assert!(
        !api_js.contains("Left out"),
        "the starter has a `pub fn` the wrapper cannot describe, so a caller \
         cannot reach it:\n{}",
        api_js.lines().filter(|l| l.starts_with("//")).collect::<Vec<_>>().join("\n")
    );
    for wanted in ["line_total", "tax", "discount", "money", "card_looks_valid", "parse_price"] {
        assert!(
            api_js.contains(&format!("export function {}(", wanted)),
            "`{}` is missing from the wrapper",
            wanted
        );
    }
}

/// The starter's copy of the Vite plugin matches the package.
///
/// The starter vendors the plugin instead of depending on it, because the
/// package is not published and a starter has to work when it is copied out of
/// this repository — which is the only thing a starter is for. Depending on it
/// by `file:` looked fine and was not: npm makes a symlink, `npm install`
/// succeeds and reports no problems, and the failure arrives later as
/// `ERR_MODULE_NOT_FOUND` pointing at a generated temp file. Silent at the
/// step that should catch it, cryptic at the step that does.
///
/// A copy needs a check, which is what this is — the same arrangement the
/// brand mark has, for the same reason.
#[test]
fn the_starters_copy_of_the_plugin_has_not_drifted() {
    let root = site().join("..");
    let package = std::fs::read_to_string(root.join("packages/vite-plugin-kite/index.js"))
        .expect("the plugin package");
    let vendored = std::fs::read_to_string(
        root.join("examples/vite-starter/plugin/vite-plugin-kite.js"),
    )
    .expect("the starter's copy");
    assert_eq!(
        package, vendored,
        "examples/vite-starter/plugin/vite-plugin-kite.js has drifted from \
         packages/vite-plugin-kite/index.js — copy it across"
    );

    // And the starter must not reach for the unpublished package by name.
    let config = std::fs::read_to_string(root.join("examples/vite-starter/vite.config.js"))
        .expect("the starter's config");
    assert!(
        config.contains("./plugin/vite-plugin-kite.js"),
        "the starter imports the plugin by package name, which does not resolve \
         once it is copied out of this repository"
    );
}
