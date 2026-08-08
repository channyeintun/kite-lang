//! Documentation examples, extracted and compiled.
//!
//! §2.3 of the specification says a doc comment's code fences "are extracted
//! and compiled as tests", and for a long time nothing did it — so the
//! examples on the reference site were the one part of the standard library
//! that could rot without anything going red.
//!
//! What makes this cheap is that a fence does not need a program built around
//! it. The module it was written in is already a compilable unit, so a fence
//! is appended to that module as a function of its own and run by name — the
//! same machinery `kitec test` already uses for `test_…`. Everything the
//! comment documents is therefore in scope exactly as a reader would expect,
//! with no `use` line to write and no import path to get wrong.

/// One `kite` fence out of one doc comment.
pub struct DocTest {
    /// What `kitec test` prints, and the name of the generated function.
    pub name: String,
    /// Where the fence opened, for a message that can be clicked.
    pub line: u32,
    /// The fence's contents, with the `///` prefix removed.
    pub code: String,
    /// Whether the fence declares things rather than doing them.
    ///
    /// A fence of statements is wrapped in a function and **run**. A fence
    /// that declares a `fn` or a `struct` is appended as-is and only
    /// **compiled**, because there is nothing in it to call — and wrapping it
    /// would put a declaration inside a function body, which is not where one
    /// goes.
    pub declares: bool,
}

/// Every `kite` fence in every doc comment, in source order.
///
/// Fences tagged anything else are left alone. `text` is what the reference
/// generator uses for a rendered diagnostic, and compiling one of those would
/// fail for a reason that has nothing to do with the example being wrong.
///
/// **` ```kite ignore ` is an illustration rather than an example**, and is
/// skipped. The distinction is real and worth a marker: a module header
/// showing `html.mount(body, map(rows, row))` is teaching a shape, and
/// inventing a `Row` and a `body` to make it compile would make it a worse
/// explanation of the thing it is there to explain. Everything without the
/// marker is a claim that the code runs, and is held to it.
///
/// The marker is a second word rather than a different tag so that a renderer
/// still highlights the block — an info string's first word is the language,
/// and every markdown renderer reads it that way.
pub fn extract(src: &str, module: &str) -> Vec<DocTest> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut code = String::new();
    let mut opened_at = 0u32;

    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim_start();
        // A fence only counts inside a doc comment. `//` and ordinary code are
        // not documentation, and a `//!` module header is — both spellings
        // reach the same reader.
        let Some(body) = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//!"))
        else {
            // A doc comment that stops mid-fence is a malformed comment rather
            // than a test. Drop it rather than compiling half an example.
            in_fence = false;
            code.clear();
            continue;
        };
        let body = body.strip_prefix(' ').unwrap_or(body);
        let trimmed = body.trim();

        if in_fence {
            if trimmed == "```" {
                in_fence = false;
                let taken = std::mem::take(&mut code);
                // `opened_at` of zero marks the ignored fence above.
                if opened_at != 0 && !taken.trim().is_empty() {
                    out.push(DocTest {
                        name: format!("{}:{}", module, opened_at),
                        line: opened_at,
                        declares: declares_items(&taken),
                        code: taken,
                    });
                }
            } else {
                code.push_str(body);
                code.push('\n');
            }
            continue;
        }

        if trimmed == "```kite" {
            in_fence = true;
            code.clear();
            opened_at = i as u32 + 1;
        } else if trimmed == "```kite ignore" {
            // Consumed to its close so that the lines inside are not scanned
            // for a fence of their own, but nothing is collected.
            in_fence = true;
            code.clear();
            opened_at = 0;
        }
    }
    out
}

/// Whether a fence introduces top-level items.
///
/// Read from the left margin, because that is where a declaration in a fence
/// is written and an indented `fn` is a closure or a nested example rather
/// than something to append at file scope.
fn declares_items(code: &str) -> bool {
    code.lines().any(|l| {
        [
            "fn ", "pub fn ", "struct ", "pub struct ", "enum ", "pub enum ", "trait ",
            "pub trait ", "impl ", "use ", "type ", "pub type ", "@derive",
        ]
        .iter()
        .any(|kw| l.starts_with(kw))
    })
}

/// The module source with every runnable fence appended as a function.
///
/// Returns the augmented source and the names to run, in order. A fence that
/// declares items is appended without a wrapper and contributes no name: it is
/// checked by the fact that the whole thing compiles.
pub fn augment(src: &str, tests: &[DocTest]) -> (String, Vec<String>) {
    let mut out = String::from(src);
    let mut names = Vec::new();
    out.push('\n');
    for (i, t) in tests.iter().enumerate() {
        out.push('\n');
        if t.declares {
            out.push_str(&t.code);
            continue;
        }
        // `pub`, because dead-code elimination is right about a private
        // function nothing calls and this one is called by name from outside
        // the program.
        let name = format!("doc_example_{}", i);
        out.push_str(&format!("pub fn {}() {{\n", name));
        for line in t.code.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("}\n");
        names.push(name);
    }
    (out, names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kite_fence_is_found_and_a_text_one_is_not() {
        let src = "\
/// Doubles.
///
/// ```kite
/// io.print(double(2))
/// ```
pub fn double(n: int) -> int {
    return n * 2
}

/// ```text
/// error[E0001]
/// ```
pub fn other() {}
";
        let found = extract(src, "m");
        assert_eq!(found.len(), 1, "only the `kite` fence is a test");
        assert_eq!(found[0].code, "io.print(double(2))\n");
        assert!(!found[0].declares);
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn a_fence_that_declares_is_not_wrapped() {
        let src = "\
//! ```kite
//! struct P {
//!     x: int
//! }
//! ```
";
        let found = extract(src, "m");
        assert_eq!(found.len(), 1);
        assert!(found[0].declares, "a `struct` is a declaration");
        let (out, names) = augment("", &found);
        assert!(names.is_empty(), "nothing to run in a declaration fence");
        assert!(out.contains("struct P {"), "{}", out);
    }

    #[test]
    fn a_statement_fence_becomes_a_function() {
        let src = "/// ```kite\n/// io.print(1)\n/// ```\n";
        let found = extract(src, "m");
        let (out, names) = augment("fn main() {}\n", &found);
        assert_eq!(names, vec!["doc_example_0".to_string()]);
        assert!(
            out.contains("pub fn doc_example_0() {\n    io.print(1)\n}"),
            "{}",
            out
        );
    }

    #[test]
    fn an_unterminated_fence_is_dropped() {
        let src = "/// ```kite\n/// io.print(1)\nfn real() {}\n";
        assert!(extract(src, "m").is_empty());
    }

    #[test]
    fn an_ignored_fence_is_not_a_test() {
        let src = "\
//! ```kite ignore
//! html.mount(body, map(rows, row))
//! ```
//!
//! ```kite
//! io.print(1)
//! ```
";
        let found = extract(src, "m");
        assert_eq!(found.len(), 1, "only the unmarked fence is a test");
        assert_eq!(found[0].code, "io.print(1)\n");
    }
}
