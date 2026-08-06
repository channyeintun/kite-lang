//! The Kite tokeniser.
//!
//! Two properties worth knowing before reading:
//!
//! 1. **Tokens carry only a kind and a span.** Literal values are extracted
//!    from source on demand, which keeps [`Token`] `Copy` and small.
//!
//! 2. **Newline termination is decided here**, not in the parser. A newline
//!    becomes a [`TokenKind::Newline`] only when it actually separates two
//!    statements. See [`should_separate`].

use kite_diag::{codes, DiagBag, Diagnostic};
use kite_span::{FileId, Span};

mod kinds;
pub use kinds::{keyword, TokenKind, KEYWORDS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// A comment, kept out of the token stream but not thrown away.
///
/// The parser never sees one — a comment cannot change what a program means —
/// but `kitec fmt` has to put them back and `kitec doc` is made of them, and
/// recovering them by re-scanning the text later is how a formatter starts
/// moving them to the wrong line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Comment {
    pub span: Span,
    /// `///` — attaches to the declaration that follows.
    pub doc: bool,
}

/// Tokenise `text`. Always returns a stream ending in `Eof`, even when
/// diagnostics were produced — later passes can then still make progress and
/// report their own findings rather than the run stopping at the first bad byte.
pub fn tokenize(file: FileId, text: &str, diags: &mut DiagBag) -> Vec<Token> {
    tokenize_with_comments(file, text, diags).0
}

/// Tokenise, keeping the comments.
pub fn tokenize_with_comments(
    file: FileId,
    text: &str,
    diags: &mut DiagBag,
) -> (Vec<Token>, Vec<Comment>) {
    Lexer {
        file,
        src: text,
        bytes: text.as_bytes(),
        pos: 0,
        limit: text.len(),
        out: Vec::new(),
        comments: Vec::new(),
        delims: Vec::new(),
        pending_newline: false,
        diags,
    }
    .run_with_comments()
}

/// Tokenize one byte range of a file.
///
/// The whole text is still passed, so every span is an absolute position in the
/// file rather than an offset into a fragment. That is what lets an
/// interpolated `\(expr)` be parsed as an ordinary expression and still point
/// at the right place in a diagnostic.
pub fn tokenize_range(
    file: FileId,
    text: &str,
    start: usize,
    end: usize,
    diags: &mut DiagBag,
) -> Vec<Token> {
    Lexer {
        file,
        src: text,
        bytes: text.as_bytes(),
        pos: start,
        limit: end.min(text.len()),
        out: Vec::new(),
        comments: Vec::new(),
        delims: Vec::new(),
        pending_newline: false,
        diags,
    }
    .run()
}

struct Lexer<'a> {
    file: FileId,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// One past the last byte this lexer may read. Everything at or after it is
    /// end-of-input as far as scanning is concerned.
    limit: usize,
    out: Vec<Token>,
    comments: Vec<Comment>,
    /// Stack of open delimiters. Newlines inside `(` or `[` are never
    /// significant, so multi-line argument lists work without trailing commas.
    delims: Vec<TokenKind>,
    pending_newline: bool,
    diags: &'a mut DiagBag,
}

impl<'a> Lexer<'a> {
    fn run(self) -> Vec<Token> {
        self.run_with_comments().0
    }

    fn run_with_comments(mut self) -> (Vec<Token>, Vec<Comment>) {
        loop {
            self.skip_trivia();

            if self.pos >= self.limit {
                if self.pending_newline && self.last_kind().is_some() {
                    // A trailing newline still terminates the final statement.
                    let p = self.pos as u32;
                    self.out.push(Token {
                        kind: TokenKind::Newline,
                        span: Span::empty_at(self.file, p),
                    });
                }
                let p = self.pos as u32;
                self.out.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::empty_at(self.file, p),
                });
                return (self.out, self.comments);
            }

            let start = self.pos;
            let Some(kind) = self.scan_token() else {
                continue;
            };
            let span = Span::new(self.file, start as u32, self.pos as u32);

            if self.pending_newline {
                self.pending_newline = false;
                if self.newline_is_significant(kind) {
                    self.out.push(Token {
                        kind: TokenKind::Newline,
                        span: Span::empty_at(self.file, start as u32),
                    });
                }
            }

            match kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    self.delims.push(kind)
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    self.delims.pop();
                }
                _ => {}
            }

            self.out.push(Token { kind, span });
        }
    }

    // ---- newline policy ---------------------------------------------------

    fn last_kind(&self) -> Option<TokenKind> {
        self.out.last().map(|t| t.kind)
    }

    fn newline_is_significant(&self, next: TokenKind) -> bool {
        // Inside ( or [ a newline is always insignificant. Inside { it is not,
        // because blocks need statement separation.
        if matches!(
            self.delims.last(),
            Some(TokenKind::LParen) | Some(TokenKind::LBracket)
        ) {
            return false;
        }
        match self.last_kind() {
            None => false,
            Some(prev) => should_separate(prev, next),
        }
    }

    // ---- trivia -----------------------------------------------------------

    /// Consume spaces, tabs, carriage returns, comments, and newlines, setting
    /// `pending_newline` if any newline was crossed.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') => self.pos += 1,
                Some(b'\n') => {
                    self.pos += 1;
                    self.pending_newline = true;
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    let start = self.pos;
                    let doc = self.peek_at(2) == Some(b'/');
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                    self.comments.push(Comment {
                        span: Span::new(self.file, start as u32, self.pos as u32),
                        doc,
                    });
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    // Consume to a plausible end so one stray `/*` does not
                    // cascade into dozens of downstream errors.
                    while self.pos < self.limit {
                        if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                    let span = Span::new(self.file, start as u32, (start + 2) as u32);
                    self.diags.push(
                        Diagnostic::error(codes::E0005, "block comments are not supported")
                            .with_primary(span, "`/*` is not a comment in Kite")
                            .with_note("use `//` for line comments and `///` for documentation"),
                    );
                }
                _ => return,
            }
        }
    }

    // ---- scanning ---------------------------------------------------------

    fn peek(&self) -> Option<u8> {
        (self.pos < self.limit).then(|| self.bytes[self.pos])
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        let at = self.pos + n;
        (at < self.limit).then(|| self.bytes[at])
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn char_at(&self, byte: usize) -> Option<char> {
        if byte >= self.limit {
            return None;
        }
        self.src[byte..].chars().next()
    }

    /// Returns `None` when input was consumed without producing a token
    /// (an error was reported instead).
    fn scan_token(&mut self) -> Option<TokenKind> {
        let c = self.peek()?;

        if c == b'"' {
            return Some(self.scan_string());
        }
        if c == b'\'' {
            return Some(self.scan_char());
        }
        if c.is_ascii_digit() {
            return Some(self.scan_number());
        }
        if is_ident_start(self.char_at(self.pos)) {
            return Some(self.scan_ident());
        }
        self.scan_punct()
    }

    fn scan_ident(&mut self) -> TokenKind {
        let start = self.pos;
        while let Some(ch) = self.char_at(self.pos) {
            if is_ident_continue(Some(ch)) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let text = &self.src[start..self.pos];
        if text == "_" {
            return TokenKind::Underscore;
        }
        kinds::keyword(text).unwrap_or(TokenKind::Ident)
    }

    fn scan_number(&mut self) -> TokenKind {
        let start = self.pos;

        if self.peek() == Some(b'0') {
            match self.peek_at(1) {
                Some(b'x') | Some(b'X') => return self.scan_radix(start, 16),
                Some(b'o') | Some(b'O') => return self.scan_radix(start, 8),
                Some(b'b') | Some(b'B') => return self.scan_radix(start, 2),
                _ => {}
            }
        }

        self.eat_digits(10);
        let mut is_float = false;

        // A `.` begins a fraction only when followed by a digit, so `0..10`
        // lexes as Int DotDot Int and `x.field` is left alone.
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.pos += 1;
            self.eat_digits(10);
        }

        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                self.eat_digits(10);
            } else {
                self.pos = save;
            }
        }

        let before_suffix = self.pos;
        self.eat_suffix();
        let suffix = &self.src[before_suffix..self.pos];
        let digits = &self.src[start..before_suffix];

        if digits.ends_with('_') || digits.contains("__") {
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                "digit separators must appear between digits",
            );
        }

        if is_float || suffix == "f32" || suffix == "f64" {
            TokenKind::Float
        } else {
            TokenKind::Int
        }
    }

    fn scan_radix(&mut self, start: usize, radix: u32) -> TokenKind {
        self.pos += 2;
        let digits_start = self.pos;
        self.eat_digits(radix);
        if self.pos == digits_start {
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                "expected at least one digit after the radix prefix",
            );
        }
        self.eat_suffix();
        TokenKind::Int
    }

    fn eat_digits(&mut self, radix: u32) {
        while let Some(c) = self.peek() {
            if c == b'_' || (c as char).is_digit(radix) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Reject a trailing type suffix such as `i32` or `f64`.
    ///
    /// Kite has one integer type and one float, so none of these name anything.
    /// They were once consumed and thrown away, which made `300i8` read as a
    /// width the compiler was checking — when there was no `i8` for the value
    /// to overflow, and 300 came out as 300.
    fn eat_suffix(&mut self) {
        const SUFFIXES: [&str; 10] = [
            "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64",
        ];
        let start = self.pos;
        let rest = &self.src[self.pos..self.limit];
        for s in SUFFIXES {
            if rest.starts_with(s) {
                let after = self.pos + s.len();
                // Do not eat the start of a longer identifier.
                if !is_ident_continue(self.char_at(after)) {
                    self.pos = after;
                    self.error_here(
                        codes::E0004,
                        start,
                        &format!("`{}` is not a Kite type", s),
                        "Kite has one integer, `int`, and one float, `float`; write the number \
                         on its own",
                    );
                    return;
                }
            }
        }
    }

    /// Whether the delimiter starts at `self.pos`.
    ///
    /// Asked of the bytes rather than the `str`: a block string walks its
    /// contents one byte at a time, so `self.pos` sits inside a multi-byte
    /// character for as long as it takes to pass over one, and slicing the
    /// `str` there would panic. `"""` is ASCII, so a byte match is exact.
    fn at_block_delim(&self) -> bool {
        self.bytes[self.pos..self.limit].starts_with(b"\"\"\"")
    }

    fn scan_string(&mut self) -> TokenKind {
        let start = self.pos;

        if self.at_block_delim() {
            self.pos += 3;
            loop {
                if self.pos >= self.limit {
                    self.unterminated_string(start);
                    return TokenKind::Str;
                }
                if self.at_block_delim() {
                    self.pos += 3;
                    return TokenKind::Str;
                }
                self.pos += 1;
            }
        }

        self.pos += 1; // opening quote
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    self.unterminated_string(start);
                    return TokenKind::Str;
                }
                Some(b'"') => {
                    self.pos += 1;
                    return TokenKind::Str;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    // `\(` opens an interpolation; skip to its matching paren so
                    // quotes nested inside do not end the literal.
                    if self.peek() == Some(b'(') {
                        self.skip_interpolation();
                    } else {
                        self.bump();
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    fn skip_interpolation(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None | Some(b'\n') => return,
                Some(b'(') => {
                    depth += 1;
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Some(b'"') => {
                    self.scan_string();
                }
                _ => self.pos += 1,
            }
        }
    }

    fn unterminated_string(&mut self, start: usize) {
        let end = (start + 1).min(self.limit);
        let span = Span::new(self.file, start as u32, end as u32);
        self.diags.push(
            Diagnostic::error(codes::E0001, "unterminated string literal")
                .with_primary(span, "this string is never closed")
                .with_note("a `\"` string may not span lines; use a `\"\"\"` block string"),
        );
    }

    fn scan_char(&mut self) -> TokenKind {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    let span = Span::new(self.file, start as u32, self.pos as u32);
                    self.diags.push(
                        Diagnostic::error(codes::E0001, "unterminated character literal")
                            .with_primary(span, "this literal is never closed"),
                    );
                    return TokenKind::Char;
                }
                Some(b'\'') => {
                    self.pos += 1;
                    return TokenKind::Char;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.bump();
                }
                _ => self.pos += 1,
            }
        }
    }

    fn scan_punct(&mut self) -> Option<TokenKind> {
        let start = self.pos;
        let rest = &self.src[start..];

        // PUNCT is ordered longest-first, so this is maximal munch.
        for (text, kind) in kinds::PUNCT {
            if rest.starts_with(text) {
                self.pos += text.len();
                return Some(*kind);
            }
        }

        let ch = self.char_at(start).unwrap_or('\u{fffd}');
        self.pos += ch.len_utf8();
        let span = Span::new(self.file, start as u32, self.pos as u32);
        self.diags.push(
            Diagnostic::error(codes::E0002, format!("invalid character `{}` in source", ch))
                .with_primary(span, "not part of any Kite token"),
        );
        None
    }

    fn error_here(&mut self, code: codes::Code, start: usize, msg: &str, note: &str) {
        let span = Span::new(self.file, start as u32, self.pos as u32);
        self.diags.push(
            Diagnostic::error(code, msg.to_string())
                .with_primary(span, "")
                .with_note(note.to_string()),
        );
    }
}

/// Whether a newline between `prev` and `next` separates two statements.
///
/// The specification's rule is that a statement continues when the line ends in
/// an operator, an open delimiter, or a comma. Two additions make ordinary code
/// read naturally: a line *starting* with `.` continues the previous one, so
/// method chains work, and `else` is never separated from its `}`.
pub fn should_separate(prev: TokenKind, next: TokenKind) -> bool {
    if prev.continues_line() {
        return false;
    }
    // Leading-dot method chaining.
    if next == TokenKind::Dot {
        return false;
    }
    // `}` newline `else` is one statement.
    if next == TokenKind::Else {
        return false;
    }
    // A closing delimiter never needs a separator before it.
    if matches!(next, TokenKind::RParen | TokenKind::RBracket) {
        return false;
    }
    true
}

// Identifiers follow UAX #31 (XID_Start / XID_Continue) rather than an
// `is_alphanumeric` approximation. The difference is not cosmetic: XID_Continue
// includes combining marks, without which scripts such as Burmese, Devanagari,
// Thai, and Hebrew cannot spell ordinary words. `နာမည်` ends in U+103A, a
// nonspacing mark that `is_alphanumeric` rejects.
fn is_ident_start(c: Option<char>) -> bool {
    matches!(c, Some(c) if c == '_' || unicode_ident::is_xid_start(c))
}

fn is_ident_continue(c: Option<char>) -> bool {
    matches!(c, Some(c) if c == '_' || unicode_ident::is_xid_continue(c))
}

#[cfg(test)]
mod tests;
