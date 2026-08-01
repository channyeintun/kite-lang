//! The annotated compile-fail corpus.
//!
//! Each `tests/corpus/*.kite` file marks the diagnostics it expects with a
//! rustc-style trailing comment:
//!
//! ```text
//!     total = 1        //~ ERROR E0114
//! ```
//!
//! The harness asserts that the expected code is reported **on that line**, and
//! that no diagnostic appears on a line that did not ask for one. Catching
//! *extra* diagnostics is the point: it is how the "one diagnostic per cause"
//! requirement stays true as the compiler grows.

use kite_driver::{compile, Emit};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .canonicalize()
        .expect("corpus directory exists")
}

/// Expected code per 1-indexed line.
fn expectations(src: &str) -> BTreeMap<u32, Vec<String>> {
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (i, line) in src.lines().enumerate() {
        let Some(rest) = line.split("//~").nth(1) else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(kind) = words.next() else { continue };
        assert_eq!(kind, "ERROR", "only `//~ ERROR CODE` is supported");
        let code = words.next().expect("`//~ ERROR` needs a code").to_string();
        out.entry(i as u32 + 1).or_default().push(code);
    }
    out
}

/// Actual codes per 1-indexed line of each diagnostic's primary span.
fn actual(c: &kite_driver::Compilation) -> BTreeMap<u32, Vec<String>> {
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for d in c.diags.iter() {
        if d.severity != kite_diag::Severity::Error {
            continue;
        }
        let Some(span) = d.primary_span() else { continue };
        let line = c.sources.file(span.file).line_col(span.start).line;
        out.entry(line)
            .or_default()
            .push(d.code.map(|x| x.0.to_string()).unwrap_or_default());
    }
    out
}

#[test]
fn corpus_diagnostics_match_annotations() {
    let dir = corpus_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kite"))
        .collect();
    files.sort();

    assert!(!files.is_empty(), "corpus is empty at {}", dir.display());

    let mut failures = Vec::new();

    for path in &files {
        let src = std::fs::read_to_string(path).expect("corpus file is readable");
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let want = expectations(&src);
        assert!(
            !want.is_empty(),
            "{} has no `//~ ERROR` annotations; every corpus file must expect at least one",
            name
        );

        let result = compile(path, &src, Emit::Check);
        let got = actual(&result);

        for (line, codes) in &want {
            match got.get(line) {
                None => failures.push(format!(
                    "{}:{}: expected {} but no diagnostic was reported there\n{}",
                    name,
                    line,
                    codes.join(", "),
                    result.render_diagnostics()
                )),
                Some(actual_codes) => {
                    for c in codes {
                        if !actual_codes.contains(c) {
                            failures.push(format!(
                                "{}:{}: expected {} but got {}\n{}",
                                name,
                                line,
                                c,
                                actual_codes.join(", "),
                                result.render_diagnostics()
                            ));
                        }
                    }
                }
            }
        }

        // Any diagnostic on an unannotated line is a cascade or a regression.
        for (line, codes) in &got {
            if !want.contains_key(line) {
                failures.push(format!(
                    "{}:{}: unexpected {}\n{}",
                    name,
                    line,
                    codes.join(", "),
                    result.render_diagnostics()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} corpus mismatch(es):\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

/// Every corpus file must fail to compile. A file that starts passing has
/// silently stopped testing anything.
#[test]
fn every_corpus_file_fails_to_compile() {
    for entry in std::fs::read_dir(corpus_dir()).expect("corpus is readable") {
        let path = entry.expect("entry is readable").path();
        if path.extension().is_none_or(|x| x != "kite") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("file is readable");
        let result = compile(&path, &src, Emit::Check);
        assert!(
            result.failed(),
            "{} compiles cleanly but is in the compile-fail corpus",
            path.display()
        );
    }
}
