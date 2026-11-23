//! Which lines of a doc comment are code, as CommonMark reads them.
//!
//! Doc comments are Markdown, and two tools need to know where the code in one
//! is: `kitec doc` leaves code alone and escapes the prose around it, and
//! `kitec test` runs the code that is tagged `kite`. They used to answer
//! separately — each took any line of three backticks as the end of a fence —
//! so a ```` ````text ```` fence holding a ` ```kite ` example was closed by
//! the example's own fence, and `kitec test` ran what the page showed as text.
//! One scanner answers for both.
//!
//! The rules are CommonMark's:
//!
//! * a fence opens on a run of three or more backticks or tildes (a backtick
//!   fence's info string has no backtick in it) and closes only on a line made
//!   of **the same character, at least as many times**; one still open at the
//!   end of the text runs to the end of it;
//! * a line indented four or more columns past its container, after a blank
//!   line (or at the start), is an indented code block, which goes on through
//!   blank lines and further indented ones — but it cannot interrupt a
//!   paragraph, and the text under a list item is indented by the item's own
//!   content width, so *that* is the margin it is four past.
//!
//! A fence may be indented to sit under a list item, so its indentation is
//! not limited when it is looked for or closed.

/// What one line of a doc comment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// Prose, the line of a paragraph, a heading or a list that Markdown reads.
    Prose,
    /// A line of an indented code block (blank lines inside one included).
    Indented,
    /// The line that opens a fence, with its info string (`kite ignore`),
    /// trimmed.
    Open(&'a str),
    /// A line inside a fence.
    Inside,
    /// The line that closes a fence.
    Close,
}

/// Reads a doc comment's lines one at a time, remembering what block it is in.
#[derive(Clone, Debug)]
pub struct Scanner {
    /// The open fence: its character and how many of it opened it.
    fence: Option<(char, usize)>,
    indented: bool,
    /// Whether the line before was blank (true at the start).
    after_blank: bool,
    /// The column list text is indented to, inside a list.
    list: Option<usize>,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner::new()
    }
}

impl Scanner {
    pub fn new() -> Scanner {
        Scanner { fence: None, indented: false, after_blank: true, list: None }
    }

    /// The line that closes the fence this scanner is inside, if it is inside
    /// one — for a comment that ends before the fence does.
    pub fn closing_line(&self) -> Option<String> {
        self.fence.map(|(c, n)| c.to_string().repeat(n))
    }

    /// Read the next line (without its line break).
    pub fn next<'a>(&mut self, line: &'a str) -> Line<'a> {
        if let Some((c, n)) = self.fence {
            if closes(line, c, n) {
                self.fence = None;
                self.after_blank = false;
                return Line::Close;
            }
            return Line::Inside;
        }
        if line.trim().is_empty() {
            self.after_blank = true;
            return if self.indented { Line::Indented } else { Line::Prose };
        }
        let indent = columns(line);
        let was_blank = std::mem::replace(&mut self.after_blank, false);
        if let Some(content) = list_item(line, indent) {
            self.list = Some(content);
            self.indented = false;
            return Line::Prose;
        }
        // Text indented less than the list's own ends it, once a blank line
        // has said the text is not a lazy continuation of the item.
        if was_blank && self.list.is_some_and(|content| indent < content) {
            self.list = None;
        }
        let margin = self.list.unwrap_or(0);
        let code = indent >= margin + 4;
        if self.indented {
            if code {
                return Line::Indented;
            }
            self.indented = false;
        }
        if code && was_blank {
            self.indented = true;
            return Line::Indented;
        }
        if let Some((c, n, info)) = opening(line) {
            self.fence = Some((c, n));
            return Line::Open(info);
        }
        Line::Prose
    }
}

/// Leading columns of whitespace, a tab as four.
fn columns(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4,
            _ => break,
        }
    }
    n
}

/// A fence's opening line: its character, its length, and its info string.
fn opening(line: &str) -> Option<(char, usize, &str)> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = t.chars().take_while(|&x| x == c).count();
    if run < 3 {
        return None;
    }
    let info = t[run..].trim();
    // `~~~` may carry anything; a backtick fence's info has no backtick, or
    // the line is a code span that happens to start with three of them.
    if c == '`' && info.contains('`') {
        return None;
    }
    Some((c, run, info))
}

/// Whether `line` closes a fence of `n` of `c`: only that character, at least
/// that many times, and nothing after it but space.
fn closes(line: &str, c: char, n: usize) -> bool {
    let t = line.trim();
    t.chars().count() >= n && t.chars().all(|x| x == c)
}

/// The column a list item's text starts at, if `line` begins one: `- `, `* `,
/// `+ ` or a number and `.` or `)`.
fn list_item(line: &str, indent: usize) -> Option<usize> {
    let t = line.trim_start();
    let marker = if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        (1, rest)
    } else {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        let after = &t[digits..];
        if digits == 0 || digits > 9 || !(after.starts_with('.') || after.starts_with(')')) {
            return None;
        }
        (digits + 1, &after[1..])
    };
    let (width, rest) = marker;
    // The marker is followed by space, or ends the line.
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    // A `-` or `*` line that is nothing else but them is a rule, not an item.
    if rest.chars().all(|c| c == ' ' || c == '-' || c == '*') && rest.contains(['-', '*']) {
        return None;
    }
    let gap = rest.chars().take_while(|c| *c == ' ').count();
    let gap = if gap == 0 || gap > 4 { 1 } else { gap };
    Some(indent + width + gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Line<'_>> {
        let mut s = Scanner::new();
        text.lines().map(|l| s.next(l)).collect()
    }

    #[test]
    fn a_fence_closes_only_on_a_fence_as_long_as_its_own() {
        use Line::*;
        let got = kinds("````text\n```kite\nx\n```\n````\nafter");
        assert_eq!(got, vec![Open("text"), Inside, Inside, Inside, Close, Prose]);
        // A different character does not close it, and a longer run does.
        let got = kinds("~~~\n```\n~~~~\nafter");
        assert_eq!(got, vec![Open(""), Inside, Close, Prose]);
        // One still open stays open to the end, and says how to close it.
        let mut s = Scanner::new();
        s.next("````");
        s.next("```");
        assert_eq!(s.closing_line().as_deref(), Some("````"));
    }

    #[test]
    fn three_backticks_with_a_backtick_after_are_a_code_span() {
        assert_eq!(kinds("```a``` b"), vec![Line::Prose]);
    }

    #[test]
    fn indented_code_follows_a_blank_line_and_not_a_paragraph() {
        use Line::*;
        let got = kinds("text\n    not code\n\n    code\n\n      more\nafter");
        assert_eq!(got, vec![Prose, Prose, Prose, Indented, Indented, Indented, Prose]);
        let got = kinds("    first line is code\nthen prose");
        assert_eq!(got, vec![Indented, Prose]);
    }

    #[test]
    fn text_under_a_list_item_is_four_past_the_items_own_margin() {
        use Line::*;
        let got = kinds("- item\n\n  still the item\n\n      code in it\n\nafter");
        assert_eq!(got, vec![Prose, Prose, Prose, Prose, Indented, Indented, Prose]);
        let got = kinds("- item\n\n    four spaces is the item's own text\n");
        assert_eq!(got[2], Prose);
        // A fence under an item is a fence.
        let got = kinds("- item\n\n  ```kite\n  x\n  ```\n");
        assert_eq!(got[2], Open("kite"));
    }
}
