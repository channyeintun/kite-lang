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
    use kite_lexer::fence::{Line, Scanner};
    let mut out = Vec::new();
    // Which fences count is the Markdown reader's rule, shared with `kitec
    // doc`: a fence closes on a fence of its own character and at least its
    // own length, so a ```` ````text ```` block that holds a ` ```kite `
    // example is text, as the page that shows it says.
    let mut scan = Scanner::new();
    let mut collecting = false;
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
            scan = Scanner::new();
            collecting = false;
            code.clear();
            continue;
        };
        let body = body.strip_prefix(' ').unwrap_or(body);

        match scan.next(body) {
            Line::Open("kite") => {
                collecting = true;
                code.clear();
                opened_at = i as u32 + 1;
            }
            Line::Open("kite ignore") => {
                // Consumed to its close so that the lines inside are not scanned
                // for a fence of their own, but nothing is collected. `opened_at`
                // of zero marks it.
                collecting = true;
                code.clear();
                opened_at = 0;
            }
            Line::Open(_) => collecting = false,
            Line::Inside if collecting => {
                code.push_str(body);
                code.push('\n');
            }
            Line::Close if collecting => {
                collecting = false;
                let taken = std::mem::take(&mut code);
                if opened_at != 0 && !taken.trim().is_empty() {
                    out.push(DocTest {
                        name: format!("{}:{}", module, opened_at),
                        line: opened_at,
                        declares: declares_items(&without_uses(&taken)),
                        code: taken,
                    });
                }
            }
            _ => {}
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
            "pub trait ", "impl ", "type ", "pub type ", "@derive",
        ]
        .iter()
        .any(|kw| l.starts_with(kw))
    })
}

/// Whether a fence line is a `use`, written where one is written.
fn is_use(line: &str) -> bool {
    line.starts_with("use ")
}

/// A fence without its `use` lines, which go to the top of the file.
fn without_uses(code: &str) -> String {
    code.lines().filter(|l| !is_use(l)).map(|l| format!("{}\n", l)).collect()
}

/// The module source with every runnable fence appended as a function.
///
/// Returns the augmented source and the names to run, in order. A fence that
/// declares items is appended without a wrapper and contributes no name: it is
/// checked by the fact that the whole thing compiles.
///
/// **A fence's `use` lines go to the top of the file**, once each, and only
/// when the file does not already have them. They used to stay in the fence,
/// which a `use` made a declaring one — so it was appended at file scope, after
/// the file's own items, where neither the `use` nor the statements under it
/// are allowed, and the example failed with a syntax error it did not contain.
pub fn augment(src: &str, tests: &[DocTest]) -> (String, Vec<String>) {
    let (out, names, _) = augment_mapped(src, tests);
    (out, names)
}

/// Where a line of the augmented source came from: a 1-based line of the real
/// file, and how many columns further left the text sits in the augmented
/// line than in the real one (`///` and the wrapper's indent make the two
/// differ).
type Origin = (u32, i32);

/// Which place in the real file each place in the augmented source stands for.
///
/// A diagnostic is found in the augmented source, whose lines are the file's
/// only until the first fence. The fence's own lines come from a `///`
/// comment, and the `use` lines of a fence are hoisted to the top, so a line
/// number out of the augmented text names a line that is past the end of the
/// file or in the wrong place in it. Compiling needs the augmented text;
/// *reporting* wants the real file, so an editor or a terminal can jump to
/// the line the mistake is on.
pub struct Remap {
    /// For each line of the augmented source, its origin.
    origins: Vec<Origin>,
    /// Byte offset of the start of each line of the augmented source.
    augmented_starts: Vec<u32>,
    /// Byte offset of the start of each line of the real file, and its length.
    real_lines: Vec<(u32, u32)>,
}

impl Remap {
    /// The offset in the real file that `at`, an offset in the augmented
    /// source, stands for. Lines the compiler made up — a wrapper's header, a
    /// closing brace — stand for the line of the fence that caused them.
    pub fn offset(&self, at: u32) -> u32 {
        let line = self.augmented_starts.partition_point(|&s| s <= at).saturating_sub(1);
        let Some(&(real, shift)) = self.origins.get(line).or(self.origins.last()) else {
            return at;
        };
        let column = at as i64 - self.augmented_starts.get(line).copied().unwrap_or(0) as i64;
        let (start, len) = self.real_lines[(real as usize - 1).min(self.real_lines.len() - 1)];
        start + (column + shift as i64).clamp(0, len as i64) as u32
    }

    /// The 1-based line of the real file that `at`, an offset in the
    /// augmented source, stands for — what tells which fence a diagnostic
    /// is in.
    pub fn line(&self, at: u32) -> u32 {
        let line = self.augmented_starts.partition_point(|&s| s <= at).saturating_sub(1);
        self.origins.get(line).or(self.origins.last()).map_or(0, |o| o.0)
    }
}

impl DocTest {
    /// The lines of the real file the fence occupies, its opening and closing
    /// lines included.
    pub fn lines(&self) -> std::ops::RangeInclusive<u32> {
        self.line..=self.line + self.code.lines().count() as u32 + 1
    }
}

/// [`augment`], and the map back from the augmented source to the real file.
pub fn augment_mapped(src: &str, tests: &[DocTest]) -> (String, Vec<String>, Remap) {
    let mut real_lines = Vec::new();
    let mut start = 0u32;
    for line in src.split('\n') {
        real_lines.push((start, line.trim_end_matches('\r').len() as u32));
        start += line.len() as u32 + 1;
    }
    // Where fence line `k` of a test is in the real file: the line after the
    // fence's opening, and past the `///` and the one space after it that
    // `extract` took off.
    let real_line = |t: &DocTest, k: usize| -> Origin {
        let line = t.line + 1 + k as u32;
        let text = src.lines().nth(line as usize - 1).unwrap_or("");
        let marker = text.find("///").or_else(|| text.find("//!")).map_or(0, |i| i + 3);
        let space = text[marker.min(text.len())..].starts_with(' ') as usize;
        (line, (marker + space) as i32)
    };
    let mut hoisted: Vec<(&str, Origin)> = Vec::new();
    for t in tests {
        for (k, line) in t.code.lines().enumerate().filter(|(_, l)| is_use(l)) {
            let line = line.trim_end();
            let already = src.lines().any(|l| l.trim_end() == line)
                || hoisted.iter().any(|(h, _)| *h == line);
            if !already {
                hoisted.push((line, real_line(t, k)));
            }
        }
    }
    let mut out = String::new();
    let mut origins: Vec<Origin> = Vec::new();
    for (line, origin) in &hoisted {
        out.push_str(line);
        out.push('\n');
        origins.push(*origin);
    }
    out.push_str(src);
    origins.extend((1..=src.split('\n').count() as u32).map(|n| (n, 0)));
    let mut names = Vec::new();
    out.push('\n');
    for (i, t) in tests.iter().enumerate() {
        out.push('\n');
        origins.push((t.line, 0));
        let code = without_uses(&t.code);
        if t.declares {
            out.push_str(&code);
            for (k, _) in t.code.lines().enumerate().filter(|(_, l)| !is_use(l)) {
                origins.push(real_line(t, k));
            }
            continue;
        }
        // `pub`, because dead-code elimination is right about a private
        // function nothing calls and this one is called by name from outside
        // the program.
        let name = format!("doc_example_{}", i);
        out.push_str(&format!("pub fn {}() {{\n", name));
        origins.push((t.line, 0));
        for (k, l) in t.code.lines().enumerate().filter(|(_, l)| !is_use(l)) {
            out.push_str("    ");
            out.push_str(l);
            out.push('\n');
            let (line, shift) = real_line(t, k);
            origins.push((line, shift - 4));
        }
        out.push_str("}\n");
        origins.push((t.line, 0));
        names.push(name);
    }
    let mut augmented_starts = vec![0u32];
    for (i, b) in out.bytes().enumerate() {
        if b == b'\n' {
            augmented_starts.push(i as u32 + 1);
        }
    }
    (out, names, Remap { origins, augmented_starts, real_lines })
}

/// The diagnostics of a compiled, augmented source, rendered against the real
/// file: every span that points into it is moved to where it was written.
pub fn render_diagnostics(
    compiled: &crate::Compilation,
    path: &str,
    src: &str,
    remap: &Remap,
) -> String {
    render_diagnostics_where(compiled, path, src, remap, |_| true)
}

/// The real line of the entry file a diagnostic's primary span points at, or
/// `None` when it points into another file (a module the program uses).
pub fn diagnostic_line(
    compiled: &crate::Compilation,
    path: &str,
    remap: &Remap,
    d: &kite_diag::Diagnostic,
) -> Option<u32> {
    let span = d.primary_span()?;
    let entry = compiled.sources.iter().find(|(_, name)| name == path)?.0;
    (span.file == entry).then(|| remap.line(span.start))
}

/// [`render_diagnostics`], for only the diagnostics `keep` accepts.
pub fn render_diagnostics_where(
    compiled: &crate::Compilation,
    path: &str,
    src: &str,
    remap: &Remap,
    keep: impl Fn(&kite_diag::Diagnostic) -> bool,
) -> String {
    use kite_span::{SourceMap, Span};
    let mut sources = SourceMap::new();
    let mut entry = None;
    for (id, name) in compiled.sources.iter() {
        let file = compiled.sources.file(id);
        if name == path {
            entry = Some(id);
            sources.add(&file.name, src);
        } else {
            sources.add(&file.name, file.text.clone());
        }
    }
    let moved = |span: Span| -> Span {
        if Some(span.file) != entry {
            return span;
        }
        let (start, end) = (remap.offset(span.start), remap.offset(span.end));
        Span { file: span.file, start, end: end.max(start) }
    };
    let mut diags = kite_diag::DiagBag::new();
    for d in compiled.diags.iter().filter(|d| keep(d)) {
        let mut d = d.clone();
        for label in &mut d.labels {
            label.span = moved(label.span);
        }
        for edit in d.fixes.iter_mut().flat_map(|f| f.edits.iter_mut()) {
            edit.span = moved(edit.span);
        }
        diags.push(d);
    }
    diags.render_all(&sources)
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

    /// A `use` in a fence is not a declaration of the fence's: it goes to the
    /// top of the file, where a `use` is allowed, and the rest of the fence is
    /// run as statements.
    #[test]
    fn a_use_in_a_fence_goes_to_the_top_of_the_file() {
        let src = "/// ```kite\n/// use std/json\n/// let (d, e) = json.parse(\"1\")\n\
                   /// assert(e == nil, \"parse\")\n/// ```\npub fn nothing() -> int {\n    \
                   return 0\n}\n";
        let found = extract(src, "m");
        assert_eq!(found.len(), 1);
        assert!(!found[0].declares, "a `use` alone does not make a fence a declaration");
        let (out, names) = augment(src, &found);
        assert!(out.starts_with("use std/json\n"), "{}", out);
        assert_eq!(out.matches("use std/json").count(), 2, "hoisted once, still in the comment: {}", out);
        assert_eq!(names, vec!["doc_example_0".to_string()]);

        let c = crate::compile("l3.kite", &out, crate::Emit::Kbc);
        assert!(!c.failed(), "{}", c.render_diagnostics());
    }

    /// A `use` the file already has is not written twice.
    #[test]
    fn a_use_the_file_already_has_is_not_repeated() {
        let src = "use std/json\n\n/// ```kite\n/// use std/json\n/// io.print(1)\n/// ```\n\
                   pub fn f() {\n}\n";
        let (out, _) = augment(src, &extract(src, "m"));
        assert!(out.starts_with("use std/json\n\n///"), "{}", out);
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

    /// A ` ```kite ` line inside a ```` ````text ```` fence is text, however
    /// the page that shows it was written: the fence closes only on a fence of
    /// its own length, so the example inside is not the end of it.
    #[test]
    fn a_kite_fence_inside_a_longer_fence_is_not_an_example() {
        let src = "/// ````text\n/// ```kite\n/// this is an illustration\n/// ```\n/// ````\n///\n/// ```kite\n/// io.print(1)\n/// ```\n";
        let found = extract(src, "m");
        assert_eq!(found.len(), 1, "{:?}", found.iter().map(|f| &f.code).collect::<Vec<_>>());
        assert_eq!(found[0].code, "io.print(1)\n");
        // And `~~~` fences and indented blocks hold none either.
        let src = "/// ~~~\n/// ```kite\n/// x\n/// ```\n/// ~~~\n///\n///     ```kite\n///     y\n///     ```\n";
        assert!(extract(src, "m").is_empty());
    }
}
