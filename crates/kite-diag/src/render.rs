//! Diagnostic rendering.
//!
//! Output format is pinned by snapshot tests. Changing a message requires
//! updating a snapshot, which makes message quality a reviewable part of every
//! change.

use crate::{Diagnostic, LabelStyle};
use kite_span::{SourceMap, Span};
use std::fmt::Write as _;

/// A label resolved to concrete line/column coordinates.
struct Placed<'a> {
    line: u32,
    /// 0-based character offset within the line.
    col: usize,
    /// Width in characters, at least 1 so zero-width spans still render.
    width: usize,
    style: LabelStyle,
    message: &'a str,
}

/// Render a diagnostic for a terminal.
///
/// The whole result goes through [`tame`] on the way out, rather than each
/// place that quotes source doing it individually — a diagnostic quotes the
/// line it is about, names the character it choked on, and echoes identifiers,
/// and every one of those came from a file. Sanitising at the one boundary is
/// what makes that exhaustive instead of a list to keep up to date.
pub fn render(d: &Diagnostic, sources: &SourceMap) -> String {
    tame(&render_raw(d, sources))
}

/// Control characters, made visible.
///
/// The bytes of a source line are chosen by whoever wrote the file — a
/// vendored dependency, an attached repro, a contributor's branch — and this
/// output goes to a terminal. Written verbatim, `ESC [` is not text: it moves
/// the cursor, erases what was already printed and repaints it, so the author
/// of a file decides what the developer compiling it sees. A build that failed
/// can be made to look like one that passed. On terminals honouring OSC 52 the
/// same bytes reach the clipboard.
///
/// Newline and tab are kept: the layout is built from them, and neither can
/// overwrite what is already on screen. Everything else in C0, DEL and C1
/// becomes a visible escape — the same treatment the language server's JSON
/// writer has always applied on its side of the wire.
///
/// The bidirectional formatting characters go too, and they are the reason
/// this is not simply `char::is_control`. That predicate is the Unicode `Cc`
/// category, so `U+202E RIGHT-TO-LEFT OVERRIDE` is not a control character by
/// it and was written out verbatim. It does not move the cursor; it reorders
/// what is already there, which reaches the same end by a quieter route — an
/// identifier quoted back in a diagnostic can be made to read as a different
/// identifier, and the reader has no way to see it. Escaping only what can
/// reorder, rather than every invisible character, keeps ordinary text
/// ordinary: a zero-width space is unhelpful but it cannot rewrite a line.
fn tame(text: &str) -> String {
    let needs = text.chars().any(|c| c != '\n' && c != '\t' && suspicious(c));
    if !needs {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\n' || c == '\t' {
            out.push(c);
        } else if suspicious(c) {
            let _ = write!(out, "\\u{{{:02x}}}", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether a character decides what the terminal shows rather than being shown.
fn suspicious(c: char) -> bool {
    c.is_control()
        // C1, which a terminal reads as escape sequences of its own.
        || ('\u{80}'..='\u{9f}').contains(&c)
        // Bidirectional embeddings and overrides, and the pop that ends them.
        || ('\u{202a}'..='\u{202e}').contains(&c)
        // Bidirectional isolates, and their pop.
        || ('\u{2066}'..='\u{2069}').contains(&c)
        // The marks, which set direction without an explicit scope.
        || c == '\u{200e}'
        || c == '\u{200f}'
        || c == '\u{061c}'
}

fn render_raw(d: &Diagnostic, sources: &SourceMap) -> String {
    let mut out = String::new();

    // ---- header -----------------------------------------------------------
    match d.code {
        Some(c) => {
            let _ = writeln!(out, "{}[{}]: {}", d.severity.label(), c, d.message);
        }
        None => {
            let _ = writeln!(out, "{}: {}", d.severity.label(), d.message);
        }
    }

    let anchor = d.primary_span().or_else(|| d.labels.first().map(|l| l.span));
    let Some(anchor) = anchor else {
        for note in &d.notes {
            let _ = writeln!(out, "  = note: {}", note);
        }
        return out;
    };

    // Labels grouped by file, the anchor's file first and the rest in the
    // order they were attached, each group in source order.
    //
    // A label in another file is the reason for the error as often as not —
    // the parameter in a module the call reached, whose type the argument
    // does not match — so it gets a block of its own under its own file's
    // header. It used to be dropped, which is the one place the "why" of a
    // cross-module mistake was.
    let mut files = vec![anchor.file];
    for l in &d.labels {
        if !files.contains(&l.span.file) {
            files.push(l.span.file);
        }
    }
    let groups: Vec<(Span, Vec<Placed>)> = files
        .iter()
        .map(|&file| {
            let mut placed: Vec<Placed> = d
                .labels
                .iter()
                .filter(|l| l.span.file == file)
                .map(|l| place(sources, l.span, l.style, &l.message))
                .collect();
            placed.sort_by_key(|p| (p.line, p.col));
            // The anchor's own block is headed by the anchor; another file's
            // by the first thing in it.
            let head = if file == anchor.file {
                anchor
            } else {
                d.labels
                    .iter()
                    .filter(|l| l.span.file == file)
                    .map(|l| l.span)
                    .min_by_key(|s| s.start)
                    .unwrap_or(anchor)
            };
            (head, placed)
        })
        .collect();

    let max_line = groups
        .iter()
        .flat_map(|(_, placed)| placed.iter().map(|p| p.line))
        .max()
        .unwrap_or(1);
    let gutter = digits(max_line);

    // ---- location and snippet, per file ------------------------------------
    for (head, placed) in &groups {
        let file = sources.file(head.file);
        let lc = file.line_col(head.start);
        let _ = writeln!(
            out,
            "{:>w$}┌─ {}:{}:{}",
            "",
            file.name.display(),
            lc.line,
            lc.col,
            w = gutter + 1
        );
        render_snippet(&mut out, sources, *head, placed, gutter);
    }

    // ---- notes ------------------------------------------------------------
    if !d.notes.is_empty() {
        for note in &d.notes {
            let _ = writeln!(out, "{:>w$}= note: {}", "", note, w = gutter + 1);
        }
    }

    // ---- fixes ------------------------------------------------------------
    for fix in &d.fixes {
        let _ = writeln!(out, "help: {}", fix.message);
        render_fix(&mut out, sources, fix, gutter);
    }

    out
}

fn render_snippet(
    out: &mut String,
    sources: &SourceMap,
    anchor: Span,
    placed: &[Placed],
    gutter: usize,
) {
    let file = sources.file(anchor.file);
    let bar = format!("{:>w$}│", "", w = gutter + 1);
    let _ = writeln!(out, "{}", bar);

    let mut prev_line: Option<u32> = None;
    for p in placed {
        match prev_line {
            // Same line as the previous label: the source line is already
            // printed, so only add the underline.
            Some(prev) if prev == p.line => {}
            Some(prev) if p.line == prev + 1 => {
                print_source_line(out, file, p.line, gutter);
            }
            Some(prev) if p.line > prev + 1 => {
                let _ = writeln!(out, "{:>w$}⋮", "", w = gutter + 1);
                print_source_line(out, file, p.line, gutter);
            }
            _ => {
                print_source_line(out, file, p.line, gutter);
            }
        }

        let mark = match p.style {
            LabelStyle::Primary => '^',
            LabelStyle::Secondary => '-',
        };
        let _ = write!(out, "{:>w$}│ ", "", w = gutter + 1);
        let _ = write!(out, "{}{}", " ".repeat(p.col), mark.to_string().repeat(p.width));
        if p.message.is_empty() {
            let _ = writeln!(out);
        } else {
            let _ = writeln!(out, " {}", p.message);
        }

        prev_line = Some(p.line);
    }

    let _ = writeln!(out, "{}", bar);
}

fn print_source_line(out: &mut String, file: &kite_span::SourceFile, line: u32, gutter: usize) {
    let _ = writeln!(
        out,
        "{:>w$} │ {}",
        line,
        expand_tabs(file.line_text(line)),
        w = gutter
    );
}

/// How wide a tab is drawn. A tab is printed as this many spaces rather than
/// left for the terminal, because the underline beneath it is made of spaces
/// and carets: the two have to agree about how wide a tab is, and only one of
/// them is under the renderer's control otherwise.
const TAB_WIDTH: usize = 4;

/// A source line as it is drawn.
fn expand_tabs(line: &str) -> String {
    line.replace('\t', &" ".repeat(TAB_WIDTH))
}

/// How many columns `text` takes once drawn.
fn drawn_width(text: &str) -> usize {
    text.chars().map(column_width).sum()
}

/// How many terminal columns a character takes: a tab as `TAB_WIDTH`, the
/// wide East Asian characters and emoji as two, combining marks and joiners
/// as none, everything else as one.
///
/// The underline is made of carets, and a caret per *character* put three
/// under the six columns of `日本語`. This is the common ranges rather than the
/// whole of Unicode's width tables — a diagnostic is not worth a dependency —
/// and a terminal may disagree about the rarest, where the underline is a
/// column out and still on the right word.
fn column_width(c: char) -> usize {
    const NONE: &[(u32, u32)] = &[
        (0x0300, 0x036F), (0x0483, 0x0489), (0x0591, 0x05BD), (0x05BF, 0x05BF),
        (0x05C1, 0x05C2), (0x05C4, 0x05C5), (0x05C7, 0x05C7), (0x0610, 0x061A),
        (0x064B, 0x065F), (0x0670, 0x0670), (0x06D6, 0x06DC), (0x06DF, 0x06E4),
        (0x0E31, 0x0E31), (0x0E34, 0x0E3A), (0x0E47, 0x0E4E), (0x200B, 0x200F),
        (0x202A, 0x202E), (0x2060, 0x2064), (0x20D0, 0x20F0), (0x3099, 0x309A),
        (0xFE00, 0xFE0F), (0xFE20, 0xFE2F), (0xFEFF, 0xFEFF), (0xE0100, 0xE01EF),
    ];
    const TWO: &[(u32, u32)] = &[
        (0x1100, 0x115F), (0x231A, 0x231B), (0x2329, 0x232A), (0x23E9, 0x23EC),
        (0x23F0, 0x23F0), (0x23F3, 0x23F3), (0x25FD, 0x25FE), (0x2614, 0x2615),
        (0x2648, 0x2653), (0x267F, 0x267F), (0x2693, 0x2693), (0x26A1, 0x26A1),
        (0x26AA, 0x26AB), (0x26BD, 0x26BE), (0x26C4, 0x26C5), (0x26CE, 0x26CE),
        (0x26D4, 0x26D4), (0x26EA, 0x26EA), (0x26F2, 0x26F3), (0x26F5, 0x26F5),
        (0x26FA, 0x26FA), (0x26FD, 0x26FD), (0x2705, 0x2705), (0x270A, 0x270B),
        (0x2728, 0x2728), (0x274C, 0x274C), (0x274E, 0x274E), (0x2753, 0x2755),
        (0x2757, 0x2757), (0x2795, 0x2797), (0x27B0, 0x27B0), (0x27BF, 0x27BF),
        (0x2B1B, 0x2B1C), (0x2B50, 0x2B50), (0x2B55, 0x2B55), (0x2E80, 0x303E),
        (0x3041, 0x33FF), (0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xA000, 0xA4CF),
        (0xA960, 0xA97F), (0xAC00, 0xD7A3), (0xF900, 0xFAFF), (0xFE10, 0xFE19),
        (0xFE30, 0xFE6F), (0xFF00, 0xFF60), (0xFFE0, 0xFFE6), (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF), (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A),
        (0x1F200, 0x1F251), (0x1F300, 0x1F64F), (0x1F680, 0x1F6FF),
        (0x1F900, 0x1F9FF), (0x1FA70, 0x1FAFF), (0x20000, 0x3FFFD),
    ];
    let n = c as u32;
    if c == '\t' {
        TAB_WIDTH
    } else if NONE.iter().any(|&(lo, hi)| (lo..=hi).contains(&n)) {
        0
    } else if TWO.iter().any(|&(lo, hi)| (lo..=hi).contains(&n)) {
        2
    } else {
        1
    }
}

fn render_fix(out: &mut String, sources: &SourceMap, fix: &crate::Fix, gutter: usize) {
    // Only single-edit, single-line fixes get a preview. Anything larger is
    // still machine-applicable via `kite fix`; it just is not drawn here.
    if fix.edits.len() != 1 {
        return;
    }
    let edit = &fix.edits[0];
    let file = sources.file(edit.span.file);
    let start = file.line_col(edit.span.start);
    let end = file.line_col(edit.span.end);
    if start.line != end.line {
        return;
    }

    let line_text = file.line_text(start.line);
    let col = (start.col - 1) as usize;
    let old_width = (end.col - start.col) as usize;

    let chars: Vec<char> = line_text.chars().collect();
    let before: String = chars[..col.min(chars.len())].iter().collect();
    let mut patched = before.clone();
    patched.push_str(&edit.replacement);
    if col + old_width < chars.len() {
        patched.extend(&chars[col + old_width..]);
    }

    let _ = writeln!(out, "{:>w$}│", "", w = gutter + 1);
    let _ = writeln!(out, "{:>w$} │ {}", start.line, expand_tabs(&patched), w = gutter);
    let new_width = drawn_width(&edit.replacement).max(1);
    let _ = writeln!(
        out,
        "{:>w$}│ {}{}",
        "",
        " ".repeat(drawn_width(&before)),
        "~".repeat(new_width),
        w = gutter + 1
    );
}

fn place<'a>(
    sources: &SourceMap,
    span: Span,
    style: LabelStyle,
    message: &'a str,
) -> Placed<'a> {
    let file = sources.file(span.file);
    let start = file.line_col(span.start);
    let end = file.line_col(span.end);

    // Columns are counted as the line is drawn, a tab as `TAB_WIDTH`, so the
    // caret lands under the character it means on a tab-indented line.
    let line: Vec<char> = file.line_text(start.line).chars().collect();
    let from = ((start.col - 1) as usize).min(line.len());
    let to = if end.line == start.line {
        ((end.col - 1) as usize).clamp(from, line.len())
    } else {
        // Multi-line span: underline to the end of the first line.
        line.len()
    };
    let drawn = |chars: &[char]| drawn_width(&chars.iter().collect::<String>());

    Placed {
        line: start.line,
        col: drawn(&line[..from]),
        width: drawn(&line[from..to]).max(1),
        style,
        message,
    }
}

fn digits(n: u32) -> usize {
    let mut n = n;
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

#[cfg(test)]
mod tests {
    use crate::{codes, Diagnostic, Fix};
    use kite_span::{SourceMap, Span};

    const SRC: &str = "fn total(items: [Item]) -> int {\n\
                       \x20   let total = 0\n\
                       \x20   for item in items {\n\
                       \x20       total = total + item.price\n\
                       \x20   }\n\
                       \x20   return total\n\
                       }\n";

    fn setup() -> (SourceMap, kite_span::FileId) {
        let mut m = SourceMap::new();
        let f = m.add("cart.kite", SRC);
        (m, f)
    }

    /// Byte offset of the nth (0-based) occurrence of `needle`.
    fn find_nth(hay: &str, needle: &str, n: usize) -> u32 {
        hay.match_indices(needle).nth(n).unwrap().0 as u32
    }

    #[test]
    fn renders_primary_secondary_and_fix() {
        let (m, f) = setup();
        let decl = find_nth(SRC, "total", 1); // `let total`
        let asgn = find_nth(SRC, "total", 2); // `total = total + ...`

        let d = Diagnostic::error(codes::E0114, "cannot assign to immutable binding `total`")
            .with_secondary(Span::new(f, decl, decl + 5), "declared immutable here")
            .with_primary(Span::new(f, asgn, asgn + 5), "cannot assign")
            .with_fix(Fix::replace(
                "make the binding mutable",
                Span::new(f, decl - 4, decl - 1),
                "var",
            ));

        let out = d.render(&m);
        let expected = "\
error[E0114]: cannot assign to immutable binding `total`
  ┌─ cart.kite:4:9
  │
2 │     let total = 0
  │         ----- declared immutable here
  ⋮
4 │         total = total + item.price
  │         ^^^^^ cannot assign
  │
help: make the binding mutable
  │
2 │     var total = 0
  │     ~~~
";
        assert_eq!(out, expected, "\n--- got ---\n{}", out);
    }

    #[test]
    fn adjacent_lines_render_without_ellipsis() {
        let (m, f) = setup();
        let a = find_nth(SRC, "for", 0);
        let b = find_nth(SRC, "total", 2);
        let d = Diagnostic::error(codes::E0100, "example")
            .with_secondary(Span::new(f, a, a + 3), "loop here")
            .with_primary(Span::new(f, b, b + 5), "and here");
        let out = d.render(&m);
        assert!(!out.contains('⋮'), "\n{}", out);
        assert!(out.contains("3 │"), "\n{}", out);
        assert!(out.contains("4 │"), "\n{}", out);
    }

    #[test]
    fn gutter_widens_for_three_digit_lines() {
        let mut m = SourceMap::new();
        let text = "x\n".repeat(120);
        let f = m.add("big.kite", text);
        let off = 2 * 110; // line 111
        let d = Diagnostic::error(codes::E0100, "example")
            .with_primary(Span::new(f, off, off + 1), "here");
        let out = d.render(&m);
        assert!(out.contains("111 │ x"), "\n{}", out);
        assert!(out.contains("    ┌─ big.kite:111:1"), "\n{}", out);
    }

    #[test]
    fn notes_are_rendered() {
        let (m, f) = setup();
        let d = Diagnostic::error(codes::E0202, "condition must be `bool`")
            .with_primary(Span::new(f, 0, 2), "found `int`")
            .with_note("Kite has no truthiness");
        let out = d.render(&m);
        assert!(out.contains("= note: Kite has no truthiness"), "\n{}", out);
    }

    /// A label in another file is drawn under that file's own header. It was
    /// dropped: calling `util.double("x")` from `main` lost the label saying
    /// where `int` was required, which was the half of the message that
    /// explained it.
    #[test]
    fn a_label_in_another_file_gets_a_block_of_its_own() {
        let mut m = SourceMap::new();
        let main = m.add("main.kite", "fn main() {\n    io.print(util.double(\"x\"))\n}\n");
        let util = m.add("util.kite", "pub fn double(n: int) -> int {\n    return n * 2\n}\n");
        let arg = find_nth(m.text(main), "\"x\"", 0);
        let param = find_nth(m.text(util), "int", 0);
        let d = Diagnostic::error(codes::E0200, "mismatched types")
            .with_primary(Span::new(main, arg, arg + 3), "found `str`")
            .with_secondary(Span::new(util, param, param + 3), "`int` required here");
        let out = d.render(&m);
        let expected = "\
error[E0200]: mismatched types
  ┌─ main.kite:2:26
  │
2 │     io.print(util.double(\"x\"))
  │                          ^^^ found `str`
  │
  ┌─ util.kite:1:18
  │
1 │ pub fn double(n: int) -> int {
  │                  --- `int` required here
  │
";
        assert_eq!(out, expected, "\n--- got ---\n{}", out);
    }

    /// A tab is drawn as four spaces, in the line and in the underline alike,
    /// so the caret lands under what it means. The line used to be printed
    /// with its tab for the terminal to expand while the caret counted it as
    /// one column.
    #[test]
    fn a_tab_indented_line_keeps_its_caret_in_place() {
        let mut m = SourceMap::new();
        let src = "fn main() {\n\tlet x: int = \"s\"\n}\n";
        let f = m.add("tab.kite", src);
        let at = find_nth(src, "\"s\"", 0);
        let d = Diagnostic::error(codes::E0200, "mismatched types")
            .with_primary(Span::new(f, at, at + 3), "found `str`");
        let out = d.render(&m);
        assert!(out.contains("2 │     let x: int = \"s\"\n"), "\n{}", out);
        assert!(out.contains("  │                  ^^^ found `str`\n"), "\n{}", out);
        assert!(!out.contains('\t'), "\n{}", out);
    }

    /// Wide characters take two columns, so the underline under `日本語` is six
    /// carets, not three; a combining accent takes none. The underline counted
    /// characters.
    #[test]
    fn a_wide_character_is_underlined_across_its_columns() {
        let mut m = SourceMap::new();
        let src = "fn main() {\n    let x = \"日本語e\u{301}😀\" + 1\n}\n";
        let f = m.add("wide.kite", src);
        let at = find_nth(src, "\"日", 0);
        let len = "\"日本語e\u{301}😀\"".len() as u32;
        let d = Diagnostic::error(codes::E0200, "mismatched types")
            .with_primary(Span::new(f, at, at + len), "found `str`");
        let out = d.render(&m);
        // Quote, six columns of CJK, `e` (the accent is none), two for the
        // emoji, quote.
        assert!(out.contains(&format!("  │ {}{} found `str`\n", " ".repeat(12), "^".repeat(11))), "\n{}", out);
    }

    #[test]
    fn every_code_has_an_explanation() {
        for (code, _) in codes::all() {
            assert!(
                codes::explain(code).is_some(),
                "{} has no --explain text",
                code
            );
        }
    }

    /// §2.3 has three comment forms, and E0005's explanation named two of
    /// them as the only ones there are: `//!` module documentation was left
    /// out after the lexer learned it.
    #[test]
    fn e0005_names_every_comment_form() {
        let (_, text) = codes::explain("E0005").expect("E0005 is explained");
        for form in ["`//`", "`///`", "`//!`"] {
            assert!(text.contains(form), "{} is missing from: {}", form, text);
        }
    }
}
