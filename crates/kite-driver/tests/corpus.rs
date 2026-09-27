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

/// A `}` missing before the next declaration is one diagnostic, from the
/// parser through the type checker, at the `{` that lost it.
///
/// These live here rather than in `tests/corpus` because the corpus is
/// formatted like every other `.kite` file, and `kitec fmt` indents what
/// follows an unclosed brace one level in — which is the very layout the
/// parser reads to find where the brace went missing.
#[test]
fn a_brace_missing_before_a_declaration_is_one_diagnostic() {
    let cases = [
        // The `}` after `return 0` closes the `if`, but it is indented to
        // close the function, and that is where the missing one was.
        (
            "fn a(x: int) -> int {\n    if x > 0 {\n        return 1\n    return 0\n}\n\n\
             fn b() -> int {\n    return 2\n}\n\n\
             fn main() {\n    io.print(a(1) + b())\n}\n",
            2,
        ),
        ("struct P {\n    x: int\n\nfn main() {\n    let p = P{ x: 1 }\n    io.print(p.x)\n}\n", 1),
        (
            "enum Light {\n    Red\n    Green\n\n\
             fn main() {\n    let l = Red\n    io.print(match l {\n        Red => 1,\n        Green => 2,\n    })\n}\n",
            1,
        ),
        (
            "struct P {\n    n: int\n}\n\nimpl P {\n    fn get(self) -> int {\n        return self.n\n    }\n\n\
             fn main() {\n    io.print(P{ n: 1 }.get())\n}\n",
            5,
        ),
        // A method's body: the methods after it are still the `impl`'s, so
        // calling one is not a second error.
        (
            "struct Counter {\n    n: int\n}\n\nimpl Counter {\n    fn get(self) -> int {\n\
             \x20       if self.n > 0 {\n            return self.n\n        }\n        return 0\n\n\
             \x20   fn twice(self) -> int {\n        return self.get() * 2\n    }\n}\n\n\
             fn main() {\n    let c = Counter{ n: 2 }\n    io.print(c.twice())\n}\n",
            6,
        ),
        // A method's, before a function at the margin: that function is not
        // one more method of the `impl`, and neither is `main`.
        (
            "struct P {\n    n: int\n}\n\nimpl P {\n    fn get(self) -> int {\n        return self.n\n\n}\n\n\
             fn helper() -> int {\n    return 2\n}\n\n\
             fn main() {\n    io.print(P{ n: 1 }.get() + helper())\n}\n",
            6,
        ),
    ];
    for (src, line) in cases {
        let result = compile(Path::new("t.kite"), src, Emit::Check);
        let errors: Vec<(u32, String)> = actual(&result)
            .into_iter()
            .flat_map(|(line, codes)| codes.into_iter().map(move |c| (line, c)))
            .collect();
        assert_eq!(
            errors,
            vec![(line, "E0101".to_string())],
            "{}\n{}",
            src,
            result.render_diagnostics()
        );
    }
}

/// A list missing its closing bracket, or a comma between two arguments, is
/// one diagnostic from the parser through the type checker.
///
/// Here rather than in `tests/corpus` for the reason above: `kitec fmt`
/// indents what follows an unclosed bracket.
#[test]
fn one_mistake_in_a_list_is_one_diagnostic() {
    let cases = [
        // A `)` missing at the end of a line: the lines after it are not more
        // arguments, and the statements on them are read.
        (
            "fn main() {\n    var xs = [1]\n    xs.push(2\n    xs.push(3)\n    io.print(xs.len())\n}\n",
            4,
        ),
        // A struct literal's `}`: the binding stays, and so does the
        // function's own `}`.
        (
            "struct P {\n    x: int\n}\n\nfn main() {\n    let p = P{ x: 1\n    io.print(p.x)\n}\n\n\
             fn other() -> int {\n    return 2\n}\n",
            7,
        ),
        // A map literal's.
        ("fn main() {\n    let m = {\"a\": 1\n    io.print(m)\n}\n", 3),
        // A comma between two arguments is a guess, and the call it would
        // guess is not checked against the function: `f` takes one.
        (
            "fn f(x: int) -> int {\n    return x\n}\n\nfn main() {\n    let a = 1\n    let b = 2\n\
             \x20   io.print(f(a b))\n}\n",
            8,
        ),
        ("fn main() {\n    let name = \"x\"\n    io.print(\"hello \" name)\n}\n", 3),
    ];
    for (src, line) in cases {
        let result = compile(Path::new("t.kite"), src, Emit::Check);
        let errors: Vec<(u32, String)> = actual(&result)
            .into_iter()
            .flat_map(|(line, codes)| codes.into_iter().map(move |c| (line, c)))
            .collect();
        assert_eq!(
            errors,
            vec![(line, "E0100".to_string())],
            "{}\n{}",
            src,
            result.render_diagnostics()
        );
    }
}

/// What the parser has already refused, the checker does not find fault
/// with a second time, on the same line.
///
/// The corpus cannot say this: it asks for a code on a line and for nothing
/// on a line that did not ask, and each of these was a second code on the
/// line that did.
#[test]
fn a_construct_the_parser_refused_is_not_checked_as_well() {
    let cases = [
        // A chained range as an index: indexing with it was typed as an
        // element, and `.len()` on that was `int` has no methods; on a string
        // it was a `str` that cannot be indexed, and in a loop an `int` that
        // cannot be iterated.
        ("fn main() {\n    let xs = [1, 2, 3]\n    io.print(xs[1..2..3].len())\n}\n", vec!["E0100"]),
        ("fn main() {\n    let s = \"hello\"\n    io.print(s[0..1..2].len())\n}\n", vec!["E0100"]),
        (
            "fn main() {\n    let xs = [1, 2, 3]\n    for x in xs[0..1..2] {\n        io.print(x)\n    }\n}\n",
            vec!["E0100"],
        ),
        // Braces an interpolation ends inside: what was written of the
        // literal, the `match` or the `if` was checked as the whole of it —
        // `P` with no text form, a match with no arms, a block with no value.
        ("struct P {\n    x: int\n}\n\nfn main() {\n    io.print(\"\\(P{ x: 1)\")\n}\n", vec!["E0101"]),
        ("fn main() {\n    let a = true\n    io.print(\"\\(match a {)\")\n}\n", vec!["E0101"]),
        (
            "fn main() {\n    let a = true\n    io.print(\"\\(if a { 1 } else { 2)\")\n}\n",
            vec!["E0101"],
        ),
        (
            "fn main() {\n    let a = true\n    io.print(\"\\(match a { true => 1, false => )\")\n}\n",
            vec!["E0100"],
        ),
        // And the same braces cut short by the end of a file: the arms not
        // yet written were not missing, and nor was a closure's value.
        ("fn main() {\n    let a = true\n    match a {\n        true => io.print(1)\n", vec!["E0101"]),
        (
            "fn main() {\n    let f = |x: int| {\n        return x\n\nfn other() {\n}\n",
            vec!["E0101"],
        ),
    ];
    for (src, want) in cases {
        let result = compile(Path::new("t.kite"), src, Emit::Check);
        let codes: Vec<&str> = result
            .diags
            .iter()
            .filter(|d| d.severity == kite_diag::Severity::Error)
            .filter_map(|d| d.code.map(|c| c.0))
            .collect();
        assert_eq!(codes, want, "{}\n{}", src, result.render_diagnostics());
    }
}

/// A body cut short by a missing `}` ends in the parser's error statement,
/// and after a `return` that is not unreachable code: nobody wrote it. It was
/// reported as E0116 beside the E0101, at the next method's `pub`.
#[test]
fn a_body_cut_short_after_a_return_is_not_unreachable_code() {
    let src = "struct B {\n    v: int\n}\n\nimpl B {\n    pub fn get(self) -> int {\n        return self.v\n\n\
               \x20   pub fn put(self, v: int) -> B {\n        return B{ v: v }\n    }\n}\n\n\
               fn main() {\n    io.print(B{ v: 1 }.put(2).get())\n}\n";
    let result = compile(Path::new("t.kite"), src, Emit::Check);
    let codes: Vec<&str> = result.diags.iter().filter_map(|d| d.code.map(|c| c.0)).collect();
    assert_eq!(codes, vec!["E0101"], "{}", result.render_diagnostics());
}
