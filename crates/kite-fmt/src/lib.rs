//! `kitec fmt` — one way to lay Kite out.
//!
//! A formatter from the first release means no formatting discussion ever
//! happens, which is worth more than any particular choice it makes.
//!
//! It works on **tokens**, not on the syntax tree. The tree has dropped the
//! things a formatter must keep — a comment, a blank line someone put between
//! two groups of fields — and a formatter that rebuilds a program from a tree
//! is a formatter that deletes them. Working on tokens also means a file that
//! does not parse still formats, which is exactly when someone reaches for it.
//!
//! What it decides:
//!
//! * indentation — four spaces per open bracket, and a closing bracket lines
//!   up with whatever opened it;
//! * spacing — one space around a binary operator, none around a prefix one,
//!   none before `,` or `:` and one after, nothing between a name and its
//!   argument list;
//! * blank lines — at most one, and none against a closing brace.
//!
//! What it leaves alone: **where the lines end**. A formatter that reflows
//! decides how an argument list is grouped, and that is a decision the author
//! made for reasons the formatter cannot see — a matrix laid out in rows, a
//! chain of transformations one per line. Keeping the author's breaks and
//! fixing everything around them is the same bargain `gofmt` makes.
//!
//! Two tokens mean two things, and both are decided by what came immediately
//! before: `-` after an operator or an open bracket is a negation, and `|`
//! where a value is expected opens a closure.

use kite_diag::DiagBag;
use kite_lexer::{Comment, Token, TokenKind as T};
use kite_span::{FileId, Span};

/// Format a file's text. Never fails: a file that does not parse still has
/// tokens, and a token stream is all this needs.
pub fn format(src: &str) -> String {
    let mut diags = DiagBag::new();
    let (tokens, comments) = kite_lexer::tokenize_with_comments(FileId(0), src, &mut diags);
    let mut f = Formatter {
        src,
        out: String::with_capacity(src.len() + src.len() / 8),
        comments,
        next_comment: 0,
        depth: 0,
        line_started: false,
        closure_params: false,
        prev: None,
        prev2: None,
        prev_end: 0,
    };
    f.run(&tokens);
    f.out
}

/// Whether formatting would change the file.
pub fn is_formatted(src: &str) -> bool {
    format(src) == src
}

struct Formatter<'a> {
    src: &'a str,
    out: String,
    comments: Vec<Comment>,
    next_comment: usize,
    depth: usize,
    line_started: bool,
    /// Between the two `|` of a closure's parameter list.
    closure_params: bool,
    prev: Option<T>,
    /// The token before that. `{` needs it: `Point{` is a literal and
    /// `struct Point {` is a declaration, and only the token two back tells
    /// them apart.
    prev2: Option<T>,
    prev_end: u32,
}

impl Formatter<'_> {
    fn run(&mut self, tokens: &[Token]) {
        for token in tokens {
            // The lexer emits a newline only where one separates statements.
            // The formatter wants every line break the author wrote — inside
            // an argument list too — so it reads them from the source gap
            // rather than from the token stream.
            if matches!(token.kind, T::Newline) {
                continue;
            }
            if token.kind == T::Eof {
                self.trailing_comments();
                break;
            }

            let gap = self.gap_before(token.span.start);
            self.comments_before(token.span.start, gap.breaks > 0);

            let closing = matches!(token.kind, T::RBrace | T::RBracket | T::RParen);
            if closing {
                self.depth = self.depth.saturating_sub(1);
            }
            if gap.breaks > 0 {
                self.end_line();
                // One blank line survives; more collapse. None is kept against
                // a closing bracket, where it is padding rather than
                // separation.
                if gap.breaks > 1 && !closing {
                    self.out.push('\n');
                }
            }

            self.start_line();
            if self.needs_space(token.kind) {
                self.out.push(' ');
            }
            let text = &self.src[token.span.start as usize..token.span.end as usize];
            self.out.push_str(text);

            if matches!(token.kind, T::LBrace | T::LBracket | T::LParen) {
                self.depth += 1;
            }
            if token.kind == T::Pipe {
                self.closure_params = !self.closure_params && self.is_value_position();
            }
            self.prev2 = self.prev;
            self.prev = Some(token.kind);
            self.prev_end = token.span.end;
        }
        while self.out.ends_with('\n') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    // ---- spacing ------------------------------------------------------------

    fn needs_space(&self, kind: T) -> bool {
        let Some(prev) = self.prev else { return false };
        if !self.line_started {
            return false;
        }
        // Nothing between a name and its argument list, its index, or a dot.
        if matches!(kind, T::LParen | T::LBracket)
            && matches!(prev, T::Ident | T::RParen | T::RBracket | T::SelfKw)
        {
            return false;
        }
        if matches!(kind, T::Comma | T::Colon | T::Dot | T::DotDot | T::DotDotEq) {
            return false;
        }
        if matches!(prev, T::Dot | T::DotDot | T::DotDotEq | T::At | T::Bang | T::LParen | T::LBracket)
        {
            return false;
        }
        // `[1, 2]` and `f(a)`: nothing hugs the inside of a bracket.
        if matches!(kind, T::RParen | T::RBracket) {
            return false;
        }
        // A closure's parameters: `|x, y|`, not `| x, y |`.
        if self.closure_params && (kind == T::Pipe || prev == T::Pipe) {
            return false;
        }
        // A negation hugs its operand; a subtraction does not.
        if prev == T::Minus && self.minus_was_prefix() {
            return false;
        }
        // Type arguments are tight: `Option<int>`, `Box<Box<int>>`.
        if kind == T::Lt && prev == T::Ident && self.in_type_position() {
            return false;
        }
        if matches!(prev, T::Lt) && self.in_type_position() {
            return false;
        }
        if matches!(kind, T::Gt | T::Shr) && self.in_type_position() {
            return false;
        }
        // `Point{ … }` is a literal and `struct Point {` is a declaration. The
        // token two back is what tells them apart: a literal appears where a
        // value does.
        if kind == T::LBrace && matches!(prev, T::Ident | T::Gt) {
            return !self.is_literal_head();
        }
        true
    }

    /// Whether a value could begin here, which is what makes a `-` a negation
    /// and a `|` a closure.
    fn is_value_position(&self) -> bool {
        !matches!(
            self.prev,
            Some(T::Ident)
                | Some(T::Int)
                | Some(T::Float)
                | Some(T::Str)
                | Some(T::Char)
                | Some(T::RParen)
                | Some(T::RBracket)
                | Some(T::RBrace)
                | Some(T::SelfKw)
                | Some(T::True)
                | Some(T::False)
        )
    }

    /// Whether the `-` just written was a negation.
    fn minus_was_prefix(&self) -> bool {
        !matches!(
            self.prev2,
            Some(T::Ident)
                | Some(T::Int)
                | Some(T::Float)
                | Some(T::Str)
                | Some(T::Char)
                | Some(T::RParen)
                | Some(T::RBracket)
                | Some(T::SelfKw)
                | Some(T::True)
                | Some(T::False)
        )
    }

    /// Whether the name before a `{` is a struct literal's rather than a
    /// declaration's.
    fn is_literal_head(&self) -> bool {
        matches!(
            self.prev2,
            Some(T::Eq)
                | Some(T::LParen)
                | Some(T::LBracket)
                | Some(T::Comma)
                | Some(T::Return)
                | Some(T::Dot)
                | Some(T::Colon)
                | Some(T::FatArrow)
                | Some(T::Plus)
                | Some(T::Minus)
                | Some(T::Star)
                | Some(T::Slash)
                | Some(T::EqEq)
                | Some(T::Ne)
                | Some(T::AmpAmp)
                | Some(T::PipePipe)
                | Some(T::Check)
                | Some(T::Await)
        )
    }

    /// Whether the tail of this line reads as a type rather than a value.
    ///
    /// Everything after a `:` or a `->` is a type until an `=` begins a value,
    /// and so is the head of a declaration. Enough to keep `Option<int>` tight
    /// while leaving `a < b` spaced.
    fn in_type_position(&self) -> bool {
        let line = self.current_line();
        let value_from = line.rfind('=').map(|i| i + 1).unwrap_or(0);
        let tail = &line[value_from..];
        if tail.contains(':') || tail.contains("->") {
            return true;
        }
        let head = line.trim_start();
        value_from == 0
            && ["struct ", "enum ", "impl ", "trait ", "fn ", "pub ", "type ", "var ", "let "]
                .iter()
                .any(|k| head.starts_with(k))
    }

    fn current_line(&self) -> &str {
        match self.out.rfind('\n') {
            Some(i) => &self.out[i + 1..],
            None => &self.out,
        }
    }

    // ---- lines and comments --------------------------------------------------

    fn start_line(&mut self) {
        if self.line_started {
            return;
        }
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.line_started = true;
        self.prev = None;
        self.prev2 = None;
    }

    fn end_line(&mut self) {
        if self.line_started {
            self.out.push('\n');
            self.line_started = false;
            self.prev = None;
            self.prev2 = None;
        }
    }

    /// Comments between the last token and this one.
    ///
    /// One that sat at the end of a line stays at the end of that line; one on
    /// a line of its own gets a line of its own, indented with the code it
    /// belongs to.
    fn comments_before(&mut self, at: u32, breaks_after: bool) {
        while self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            if c.span.start >= at {
                break;
            }
            self.next_comment += 1;
            let text = self.text(c.span).trim_end().to_string();
            let own_line = self.gap_before(c.span.start).breaks > 0 || !self.line_started;
            if own_line {
                self.end_line();
                self.start_line();
            } else {
                self.out.push(' ');
            }
            self.out.push_str(&text);
            self.prev_end = c.span.end;
            self.end_line();
            let _ = breaks_after;
        }
    }

    fn trailing_comments(&mut self) {
        while self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            self.next_comment += 1;
            let text = self.text(c.span).trim_end().to_string();
            let own_line = self.gap_before(c.span.start).breaks > 0 || !self.line_started;
            if own_line {
                self.end_line();
                self.start_line();
            } else {
                self.out.push(' ');
            }
            self.out.push_str(&text);
            self.prev_end = c.span.end;
            self.end_line();
        }
    }

    /// What the source held between the last token and `at`.
    fn gap_before(&self, at: u32) -> Gap {
        let from = self.prev_end.min(at) as usize;
        let text = &self.src[from..at as usize];
        Gap { breaks: text.matches('\n').count() }
    }

    fn text(&self, span: Span) -> &str {
        &self.src[span.start as usize..span.end as usize]
    }
}

struct Gap {
    breaks: usize,
}

#[cfg(test)]
mod tests;
