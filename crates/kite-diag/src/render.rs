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

pub fn render(d: &Diagnostic, sources: &SourceMap) -> String {
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

    // Labels in the anchor's file, in source order. Cross-file labels are
    // rendered as a separate block after the main one.
    let mut placed: Vec<Placed> = d
        .labels
        .iter()
        .filter(|l| l.span.file == anchor.file)
        .map(|l| place(sources, l.span, l.style, &l.message))
        .collect();
    placed.sort_by_key(|p| (p.line, p.col));

    let max_line = placed.iter().map(|p| p.line).max().unwrap_or(1);
    let gutter = digits(max_line);

    // ---- location ---------------------------------------------------------
    let file = sources.file(anchor.file);
    let lc = file.line_col(anchor.start);
    let _ = writeln!(
        out,
        "{:>w$}┌─ {}:{}:{}",
        "",
        file.name.display(),
        lc.line,
        lc.col,
        w = gutter + 1
    );

    render_snippet(&mut out, sources, anchor, &placed, gutter);

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
        file.line_text(line),
        w = gutter
    );
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
    let mut patched: String = chars[..col.min(chars.len())].iter().collect();
    patched.push_str(&edit.replacement);
    if col + old_width < chars.len() {
        patched.extend(&chars[col + old_width..]);
    }

    let _ = writeln!(out, "{:>w$}│", "", w = gutter + 1);
    let _ = writeln!(out, "{:>w$} │ {}", start.line, patched, w = gutter);
    let new_width = edit.replacement.chars().count().max(1);
    let _ = writeln!(
        out,
        "{:>w$}│ {}{}",
        "",
        " ".repeat(col),
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

    let width = if end.line == start.line {
        (end.col - start.col) as usize
    } else {
        // Multi-line span: underline to the end of the first line.
        file.line_text(start.line).chars().count() - (start.col - 1) as usize
    };

    Placed {
        line: start.line,
        col: (start.col - 1) as usize,
        width: width.max(1),
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
}
