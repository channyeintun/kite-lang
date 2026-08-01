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
