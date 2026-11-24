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

pub mod fence;
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
    /// `//!` — documents the file it is in rather than anything in it: the
    /// module's own overview, wherever in the header it is written.
    pub module: bool,
}

/// The byte-order mark some editors write at the start of a UTF-8 file.
///
/// It is an encoding signature rather than text — UTF-8 has no byte order to
/// mark — so it is not read as a token. It was once reported as an invalid
/// character, which made a file saved by a Windows editor fail on its first
/// byte with an error pointing at nothing visible.
pub const BYTE_ORDER_MARK: char = '\u{feff}';

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
    let start = if text.starts_with(BYTE_ORDER_MARK) { BYTE_ORDER_MARK.len_utf8() } else { 0 };
    Lexer {
        file,
        src: text,
        bytes: text.as_bytes(),
        pos: start,
        limit: text.len(),
        out: Vec::new(),
        comments: Vec::new(),
        delims: Vec::new(),
        pending_newline: false,
        skipped: false,
        break_after_skip: false,
        nesting: 0,
        nesting_reported: false,
        ran_to_end: false,
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
///
/// `strings` is how many string literals enclose the range — one for the
/// hole of a literal at the top level. It continues the whole-file scan's
/// count of nested interpolations rather than starting a fresh one, so the
/// ceiling on them falls at the same depth here as it did there. The
/// whole-file scan has already reported reaching it, so this one does not
/// report it again: a hole is re-scanned once per level it is nested, and a
/// file seventy levels deep used to hear about the ceiling once per level
/// past it.
pub fn tokenize_range(
    file: FileId,
    text: &str,
    start: usize,
    end: usize,
    strings: u32,
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
        skipped: false,
        break_after_skip: false,
        nesting: strings,
        nesting_reported: strings > 0,
        ran_to_end: false,
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
    /// Whether characters were skipped (an `E0002`) since the last token,
    /// and whether a line break was crossed after them.
    ///
    /// **Text that is skipped to the end of its line ends that line.** `let
    /// y = ???` lost its `???`, and the line break after `=` continued the
    /// statement, so the next line became the value: `let y = io.print(y)`,
    /// an E0200 and a `cannot find` beside the one report the `???` had. The
    /// skipped text stood where the value was, so the line ends there.
    skipped: bool,
    break_after_skip: bool,
    /// How many `"…\(…"` layers deep the scanner currently is.
    ///
    /// `scan_string` and `skip_interpolation` call each other, so the nesting
    /// in the *file* is native stack depth in this process — and three bytes
    /// of source buy a frame pair. Running out of stack is an abort, not a
    /// diagnostic, which means a file could kill the language server just by
    /// being opened.
    nesting: u32,
    /// Whether the ceiling has already been reported. One diagnostic answers
    /// the question; a file built to hit the limit would otherwise hit it on
    /// every quote for the rest of the line.
    nesting_reported: bool,
    /// Whether a block string has already been reported running to the end
    /// of the input. A string around it — one whose hole it is in — ran to
    /// the end because it did, so the first report, the innermost, is the
    /// one mistake.
    ran_to_end: bool,
    diags: &'a mut DiagBag,
}

/// How deeply a string may nest interpolations holding further strings.
///
/// Far past anything written on purpose — the deepest in the standard library
/// is one — and far short of the stack. The parser carries its own ceiling for
/// the same reason; this is the lexer's.
const MAX_STRING_NESTING: u32 = 64;

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
                if self.newline_is_significant(kind) || (self.break_after_skip && self.ends_a_skipped_line(kind)) {
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

            self.skipped = false;
            self.break_after_skip = false;
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

    /// Whether the line break after skipped text separates, whatever the
    /// token before the text: everything [`Lexer::newline_is_significant`]
    /// asks but whether that token continues its line.
    fn ends_a_skipped_line(&self, next: TokenKind) -> bool {
        if matches!(self.delims.last(), Some(TokenKind::LParen) | Some(TokenKind::LBracket)) {
            return false;
        }
        self.last_kind().is_some()
            && !matches!(next, TokenKind::Dot | TokenKind::Else | TokenKind::RParen | TokenKind::RBracket)
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
                    self.break_after_skip |= self.skipped;
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    let start = self.pos;
                    let doc = self.peek_at(2) == Some(b'/');
                    let module = self.peek_at(2) == Some(b'!');
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                    self.comments.push(Comment {
                        span: Span::new(self.file, start as u32, self.pos as u32),
                        doc,
                        module,
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
                            .with_note(
                                "use `//` for a comment, `///` to document a declaration and \
                                 `//!` to document the module",
                            ),
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

        // `t.0.1` is two tuple indexes, not `t` followed by the float `0.1`.
        // After a `.` a number is a member name, so it is digits and nothing
        // more: no fraction, no exponent, no radix. Without this the only
        // spelling was `t.0 .1`, which the formatter then tightened back into
        // the one that does not lex.
        if self.last_kind() == Some(TokenKind::Dot) {
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
            return TokenKind::Int;
        }

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
        let suffixed = self.eat_suffix();
        let suffix = &self.src[before_suffix..self.pos];
        let digits = &self.src[start..before_suffix];

        if !separators_between_digits(digits, 10) {
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                "digit separators must appear between digits",
            );
        } else if is_float && !suffixed && float_overflows(digits) {
            // An `int` too large for 64 bits is refused, so a `float` too
            // large for 64 bits is refused too. It used to become infinity
            // without a word, which no one writing `1e999` meant.
            self.error_here(
                codes::E0004,
                start,
                "float literal is out of range",
                "the largest finite `float` is about 1.8e308",
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
        // A digit or letter outside the radix is part of the literal, not
        // the start of the next token: `0b102` stopped at the `2` and the
        // parser saw two numbers side by side, which it reported as a
        // missing line break pointing at the `2`. The whole run is one bad
        // literal, reported once where it is.
        let stray = self.char_at(self.pos).filter(|_| self.suffix_here().is_none());
        if let Some(c) = stray.filter(|&c| is_ident_continue(Some(c))) {
            while is_ident_continue(self.char_at(self.pos)) {
                self.pos += self.char_at(self.pos).map_or(1, char::len_utf8);
            }
            let (a, name, digits) = match radix {
                2 => ("a", "binary", "0 and 1"),
                8 => ("an", "octal", "0 to 7"),
                _ => ("a", "hexadecimal", "0 to 9 and A to F"),
            };
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                &format!("`{}` is not {} {} digit; {} {} literal's digits are {}", c, a, name, a, name, digits),
            );
            return TokenKind::Int;
        }
        if self.pos == digits_start {
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                "expected at least one digit after the radix prefix",
            );
        } else if !separators_between_digits(&self.src[digits_start..self.pos], radix) {
            // `0x_F`, `0xFF_` and `0b__1` were accepted while `1_` was not.
            // One rule for every radix: a separator sits between two digits.
            self.error_here(
                codes::E0004,
                start,
                "invalid number literal",
                "digit separators must appear between digits",
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
    /// Returns whether a suffix was found (and reported).
    fn eat_suffix(&mut self) -> bool {
        let start = self.pos;
        let Some(s) = self.suffix_here() else { return false };
        self.pos += s.len();
        self.error_here(
            codes::E0004,
            start,
            &format!("`{}` is not a Kite type", s),
            "Kite has one integer, `int`, and one float, `float`; write the number on its own",
        );
        true
    }

    /// The type suffix (`i32`, `f64`, …) that starts at `self.pos`, if one
    /// does and is not the start of a longer identifier.
    fn suffix_here(&self) -> Option<&'static str> {
        const SUFFIXES: [&str; 10] = [
            "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64",
        ];
        let rest = &self.src[self.pos..self.limit];
        SUFFIXES
            .into_iter()
            .find(|s| rest.starts_with(s) && !is_ident_continue(self.char_at(self.pos + s.len())))
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
                    self.unterminated_block_string(start);
                    return TokenKind::Str;
                }
                if self.at_block_delim() {
                    self.pos += 3;
                    return TokenKind::Str;
                }
                // A hole is skipped as a `"` string's is, so a string inside
                // it — a block string included — is read as one: its `"""`
                // is its own, not the end of this one (§2.4: a hole is an
                // ordinary expression). `\\` is stepped over whole, so the
                // `(` after an escaped backslash opens nothing, and so is
                // `\"`: an escaped quote is text, so it cannot be the first
                // quote of the closing delimiter — `\""""` is a quote and
                // then the end. Any other escape is left as it was read
                // before holes were.
                if self.bytes[self.pos] == b'\\' && self.pos + 1 < self.limit {
                    match self.bytes[self.pos + 1] {
                        b'(' => {
                            self.pos += 1;
                            self.skip_interpolation();
                            continue;
                        }
                        b'\\' | b'"' => self.pos += 1,
                        _ => {}
                    }
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
                        // A `\` ending a line is stepped over with the whole
                        // line break, `\r\n` as `\n`: it is one mistake, which
                        // the checker names, and the string goes on as it does
                        // after a `\` and a `\n`, rather than ending at the
                        // `\n` and taking the next line for code.
                        if self.src[self.pos..].starts_with("\r\n") {
                            self.pos += 1;
                        }
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
                    // At the ceiling, stop recursing and read the quote as an
                    // ordinary byte. The file is already malformed by any
                    // reasonable measure; the alternative is not a better
                    // parse but an aborted process.
                    if self.nesting >= MAX_STRING_NESTING {
                        self.string_too_deep();
                        self.pos += 1;
                    } else {
                        self.nesting += 1;
                        self.scan_string();
                        self.nesting -= 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Report the nesting ceiling, once.
    fn string_too_deep(&mut self) {
        if self.nesting_reported {
            return;
        }
        self.nesting_reported = true;
        let at = self.pos.min(self.limit);
        let span = Span::new(self.file, at as u32, (at + 1).min(self.limit) as u32);
        self.diags.push(
            Diagnostic::error(codes::E0006, "string interpolation nested too deeply")
                .with_primary(span, "this string opens another interpolation")
                .with_note(format!(
                    "at most {MAX_STRING_NESTING} levels of `\\(\"…\")` may nest"
                )),
        );
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

    /// A `"""` with no `"""` after it. It has read everything to the end
    /// of the file as its text, so this is the one error the file has: the
    /// parser does not go on to report the braces it swallowed as unclosed.
    fn unterminated_block_string(&mut self, start: usize) {
        if std::mem::replace(&mut self.ran_to_end, true) {
            return;
        }
        let end = (start + 3).min(self.limit);
        let span = Span::new(self.file, start as u32, end as u32);
        self.diags.push(
            Diagnostic::error(codes::E0001, "unterminated block string")
                .with_primary(span, "this block string is never closed")
                .with_note(
                    "a block string ends at the next `\"\"\"`, so everything after this one \
                     was read as its text",
                ),
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
        // A `;` is read as the line break it was meant to be, so what follows
        // it on the line is the next statement, as the note says, and is not
        // reported again for standing where a line break was wanted.
        if ch == ';' {
            let span = Span::new(self.file, start as u32, self.pos as u32);
            // Where no statement can end it is skipped instead: before a
            // `{` on the same line, where it stands between a header and the
            // body the header is waiting for — `fn one() -> int ; {`,
            // `struct P ; {`, `if ok ; {` — and after a keyword that only
            // begins a declaration (`fn; checks()`). As a line break it put
            // the body or the name on a line of its own, which dropped the
            // declaration, and every use of it was one more report. A block
            // is not a statement, so the only statement a `{` could begin
            // is a map literal whose value is thrown away.
            let rest = self.src[self.pos..self.limit].trim_start_matches([' ', '\t', ';']);
            let after_keyword = matches!(
                self.last_kind(),
                Some(
                    TokenKind::Fn
                        | TokenKind::Async
                        | TokenKind::Pub
                        | TokenKind::Struct
                        | TokenKind::Enum
                        | TokenKind::Trait
                        | TokenKind::Impl
                )
            );
            if rest.starts_with('{') || after_keyword {
                self.diags.push(
                    Diagnostic::error(codes::E0002, "invalid character `;` in source")
                        .with_primary(span, "Kite has no `;`")
                        .with_note(
                            "a line break ends a statement, and none ends here, so this `;` is \
                             skipped",
                        ),
                );
                return None;
            }
            self.diags.push(
                Diagnostic::error(codes::E0002, "invalid character `;` in source")
                    .with_primary(span, "Kite has no `;`")
                    .with_note(
                        "a line break ends a statement; this `;` is read as one, so what follows \
                         it on the line is read as the next statement",
                    ),
            );
            self.pending_newline = true;
            return None;
        }
        self.skipped = true;
        // `# say hello` is a comment in the reader's other language: one
        // report, and the rest of the line goes with it. Read token by token
        // it was three — the `#`, `say` not found, `hello` where a line break
        // was wanted.
        let line_end = self.src[start..self.limit].find('\n').map_or(self.limit, |i| start + i);
        let to_text_end = |from: usize| self.src[from..line_end].trim_end().len() + from;
        if ch == '#' {
            self.pos = to_text_end(start);
            let span = Span::new(self.file, start as u32, self.pos as u32);
            self.diags.push(
                Diagnostic::error(codes::E0002, "invalid character `#` in source")
                    .with_primary(span, "Kite has no `#` comments")
                    .with_note(
                        "a comment is written `// …`; the rest of this line, from the `#`, is \
                         skipped",
                    ),
            );
            return None;
        }
        // A backtick string, to its closing backtick on the line or else to
        // the end of the line, is one report for the same reason. It stops
        // short of a bracket it did not open: in `f(`hello)` the `)` is the
        // call's, and taking it would make the rest of the file the call.
        if ch == '`' {
            let mut depth = 0u32;
            let mut end = None;
            for (i, b) in self.src[self.pos..line_end].bytes().enumerate() {
                match b {
                    b'`' => {
                        end = Some(self.pos + i + 1);
                        break;
                    }
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' if depth == 0 => {
                        end = Some(self.pos + i);
                        break;
                    }
                    b')' | b']' | b'}' => depth -= 1,
                    _ => {}
                }
            }
            self.pos = end.unwrap_or_else(|| to_text_end(start));
            // Spaces before the bracket it stopped at are not the string's.
            while self.pos > start + 1 && matches!(self.bytes[self.pos - 1], b' ' | b'\t') {
                self.pos -= 1;
            }
            let span = Span::new(self.file, start as u32, self.pos as u32);
            self.diags.push(
                Diagnostic::error(codes::E0002, "a backtick does not start a string")
                    .with_primary(span, "Kite has no backtick strings")
                    .with_note(
                        "a string is written `\"…\"`, and a value goes into one as `\\(…)`: \
                         `\"total: \\(n)\"`",
                    ),
            );
            return None;
        }
        // `??`, `~~`: a run of one character is one mistake, and one report.
        while self.char_at(self.pos) == Some(ch) {
            self.pos += ch.len_utf8();
        }
        let span = Span::new(self.file, start as u32, self.pos as u32);
        let run = &self.src[start..self.pos];
        let title = if run.len() == ch.len_utf8() {
            format!("invalid character `{}` in source", ch)
        } else {
            format!("invalid characters `{}` in source", run)
        };
        self.diags.push(
            Diagnostic::error(codes::E0002, title).with_primary(span, "not part of any Kite token"),
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

/// Whether every `_` in a number sits between two digits of its radix.
///
/// That is the whole rule, in every part of a literal: `1_000`, `0xFF_FF` and
/// `1.5e1_0` are fine; `1_`, `0x_F`, `1__0`, `1_.5` and `1.5_e3` are not. The
/// radix prefix is not part of `text` for a radix literal, so a separator
/// straight after `0x` has nothing before it.
fn separators_between_digits(text: &str, radix: u32) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().all(|(i, &c)| {
        c != '_'
            || (i > 0
                && chars[i - 1].is_digit(radix)
                && chars.get(i + 1).is_some_and(|n| n.is_digit(radix)))
    })
}

/// Whether a well-formed float literal is too large to be a finite `float`.
fn float_overflows(text: &str) -> bool {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    digits.parse::<f64>().is_ok_and(|v| v.is_infinite())
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
