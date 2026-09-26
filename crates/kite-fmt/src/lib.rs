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
//! where a value is expected opens a closure. A third, `<`, cannot be settled
//! that way — `Option<int>` and `count < n` are the same three tokens — so
//! the parser, which knows, says which are type brackets. A file that does
//! not parse is read forward from each `<` instead, in
//! `opens_type_arguments`.

use kite_diag::{DiagBag, Severity};
use kite_lexer::{Comment, Token, TokenKind as T};
use kite_span::{FileId, SourceMap, Span};
use std::collections::HashSet;
use std::fmt;

/// Why a file was handed back unformatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// The lexer could not read the whole file. `message` is the first thing
    /// it could not read, at `line`:`col` (both 1-based).
    Lexical { message: String, line: u32, col: u32 },
    /// The layout came out holding different tokens from the ones that went
    /// in. That is a bug in the formatter, and it is caught here rather than
    /// written over someone's file.
    Unfaithful,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::Lexical { message, line, col } => write!(
                f,
                "cannot format a file with lexical errors; the first is at {}:{}: {}",
                line, col, message
            ),
            FormatError::Unfaithful => f.write_str(
                "the formatter could not lay this file out without changing what it says; \
                 it has been left as it was (this is a bug in `kitec fmt`)",
            ),
        }
    }
}

impl std::error::Error for FormatError {}

/// Format a file's text.
///
/// A file that does not *parse* still formats: tokens are all the layout
/// needs, and a half-written file is exactly when someone reaches for this.
/// A file that does not *lex* is refused. The lexer skips what it cannot read
/// — a stray `$`, a `;`, the whole rest of the file after an unterminated
/// `/*` — and a layout rebuilt from the tokens that survived is a layout with
/// those bytes deleted. The formatter used to do exactly that and report
/// success, so `let x = a ?? b` came back as `let x = a b`.
pub fn format(src: &str) -> Result<String, FormatError> {
    let mut diags = DiagBag::new();
    let (tokens, comments) = kite_lexer::tokenize_with_comments(FileId(0), src, &mut diags);
    if let Some(first) = diags.iter().find(|d| d.severity == Severity::Error) {
        let mut map = SourceMap::new();
        let file = map.add("", src);
        let at = first.primary_span().map(|s| map.line_col(s)).unwrap_or_else(|| {
            map.file(file).line_col(0)
        });
        return Err(FormatError::Lexical {
            message: first.message.clone(),
            line: at.line,
            col: at.col,
        });
    }
    // The parser knows which `<` and `>` are type brackets and which `{`
    // opens a struct literal; a file that does not parse falls back to
    // reading the tokens around each one.
    let (brackets, literal_braces) = match kite_parser::layout(FileId(0), src, &tokens) {
        Some(layout) => (
            layout.type_brackets.into_iter().collect(),
            Some(layout.literal_braces.into_iter().collect()),
        ),
        None => (guessed_type_brackets(&tokens), None),
    };
    let mut f = Formatter {
        src,
        out: String::with_capacity(src.len() + src.len() / 8),
        comments,
        next_comment: 0,
        depth: 0,
        line_started: false,
        closure_params: false,
        brackets,
        literal_braces,
        prev: None,
        prev2: None,
        prev_bracket: false,
        prev_text: (0, 0),
        prev_end: 0,
    };
    f.run(&tokens);
    // A byte-order mark is not a token, so the layout does not carry it. It
    // goes back where it was: the formatter moves whitespace, and a mark the
    // author's editor wrote is not its to take away.
    if src.starts_with(kite_lexer::BYTE_ORDER_MARK) {
        f.out.insert(0, kite_lexer::BYTE_ORDER_MARK);
    }
    if !faithful(src, &tokens, &f.out) {
        return Err(FormatError::Unfaithful);
    }
    Ok(f.out)
}

/// Whether formatting would change the file. A file that cannot be formatted
/// answers with the reason instead.
pub fn is_formatted(src: &str) -> Result<bool, FormatError> {
    format(src).map(|out| out == src)
}

/// Whether `out` says what `src` says.
///
/// The formatter moves whitespace and nothing else, so two things have to
/// hold, and both are checked because either alone lets a real mistake
/// through:
///
/// * **With the whitespace taken out, the two texts are identical.** This is
///   what catches a deletion — a comment dropped, a token skipped.
/// * **The output lexes to the same tokens.** Whitespace is what separates
///   tokens, so taking it out cannot see two of them glued into a third:
///   `- ==` written as `-==`, or `t.0 .1` as `t.0.1`, which reads as a float.
///   Line breaks count, since they end statements.
///
/// Any difference is a formatter bug. Refusing costs a file that stays as it
/// was; not refusing costs a file that no longer means what its author wrote.
fn faithful(src: &str, tokens: &[Token], out: &str) -> bool {
    let squeeze = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    if squeeze(src) != squeeze(out) {
        return false;
    }
    let mut diags = DiagBag::new();
    let again = kite_lexer::tokenize(FileId(0), out, &mut diags);
    if diags.has_errors() {
        return false;
    }
    // The one line break the formatter adds on purpose is the file's last,
    // and a newline before the end of input separates nothing.
    let meaningful = |tokens: &[Token]| -> Vec<Token> {
        let mut kept: Vec<Token> = tokens.to_vec();
        if let [.., newline, eof] = kept.as_slice() {
            if newline.kind == T::Newline && eof.kind == T::Eof {
                kept.remove(kept.len() - 2);
            }
        }
        kept
    };
    let (before, after) = (meaningful(tokens), meaningful(&again));
    before.len() == after.len()
        && before.iter().zip(&after).all(|(a, b)| {
            a.kind == b.kind
                && src[a.span.start as usize..a.span.end as usize]
                    == out[b.span.start as usize..b.span.end as usize]
        })
}

/// Whether two tokens written with nothing between them would lex as
/// something else — `-` and `=` as `-=`, `>` and `>` as `>>`, `.` and `..`
/// as `...`.
///
/// Every rule that removes a space is written for code that parses, and a
/// formatter runs on code that does not. Asking the lexer is the one check
/// that holds for every pair, including ones no rule anticipated.
fn glues(left: &str, left_kind: T, right: &str, right_kind: T) -> bool {
    let joined = format!("{}{}", left, right);
    let mut diags = DiagBag::new();
    let tokens = kite_lexer::tokenize(FileId(0), &joined, &mut diags);
    let kinds: Vec<T> = tokens.iter().map(|t| t.kind).filter(|k| *k != T::Eof).collect();
    diags.has_errors() || kinds != [left_kind, right_kind]
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
    /// The byte offset of every `<` and `>` that brackets type arguments or
    /// generic parameters, rather than comparing two values.
    brackets: HashSet<u32>,
    /// The byte offset of every `{` that opens a struct literal or a struct
    /// pattern, when the file parsed and the parser could say.
    literal_braces: Option<HashSet<u32>>,
    prev: Option<T>,
    /// The token before that. `{` needs it: `Point{` is a literal and
    /// `struct Point {` is a declaration, and only the token two back tells
    /// them apart.
    prev2: Option<T>,
    /// Whether the previous token was a type bracket.
    prev_bracket: bool,
    /// Where the previous token's text is, for asking whether the next one
    /// would glue to it.
    prev_text: (u32, u32),
    prev_end: u32,
}

/// The type brackets of a file that does not parse, found by reading forward
/// from each `<` after a name.
///
/// Only a parser really knows — `Option<int>` and `count < n` are the same
/// three tokens — and for a file that parses, one does. This is the fallback
/// for a file half written, where a guess that is right for ordinary code is
/// what there is.
fn guessed_type_brackets(tokens: &[Token]) -> HashSet<u32> {
    let mut brackets = HashSet::new();
    let mut depth = 0usize;
    let mut prev: Option<T> = None;
    for (i, token) in tokens.iter().enumerate() {
        if token.kind == T::Newline {
            continue;
        }
        if token.kind == T::Lt
            && matches!(prev, Some(T::Ident) | Some(T::Impl))
            && opens_type_arguments(&tokens[i + 1..])
        {
            brackets.insert(token.span.start);
            depth += 1;
        } else if depth > 0 {
            match token.kind {
                T::Gt | T::Ge => {
                    brackets.insert(token.span.start);
                    depth -= 1;
                }
                T::Shr => {
                    brackets.insert(token.span.start);
                    brackets.insert(token.span.start + 1);
                    depth = depth.saturating_sub(2);
                }
                _ => {}
            }
        }
        prev = Some(token.kind);
    }
    brackets
}

/// Whether the `<` just reached opens a type argument list rather than
/// beginning a comparison, judging by what follows it: a type argument list
/// closes on a `>` with nothing between but more type.
fn opens_type_arguments(ahead: &[Token]) -> bool {
    // Long enough for any real type, short enough that a `<` with no `>` after
    // it costs nothing. A comparison chain hits a disqualifying token within a
    // few tokens anyway.
    const LIMIT: usize = 96;
    let mut depth = 1usize;
    for token in ahead.iter().take(LIMIT) {
        match token.kind {
            // Nesting. `>>` closes two at once, which is how `Box<Box<int>>`
            // reaches zero without a space in the middle.
            T::Lt => depth += 1,
            // `>=` is a `>` against an `=`: `Option<int>= nil`.
            T::Gt | T::Ge => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            // `>>` is two closers. Either of them can be ours: at depth 1 the
            // first one closes this list and the second belongs to whatever
            // encloses it, and at depth 2 they close both. Only deeper than
            // that does the scan carry on.
            T::Shr => {
                if depth <= 2 {
                    return true;
                }
                depth -= 2;
            }
            // Everything a type is made of: names and paths, the brackets of
            // `[T]`, `(A, B)` and `{K: V}`, the `,` between arguments, the
            // pieces of `fn(A) -> B`, and the `+` between a generic
            // parameter's bounds. `dyn` is in here too — the lexer hands it
            // over as an identifier rather than a keyword of its own.
            T::Ident
            | T::Comma
            | T::Dot
            | T::Colon
            | T::Plus
            | T::LBracket
            | T::RBracket
            | T::LParen
            | T::RParen
            | T::LBrace
            | T::RBrace
            | T::Arrow
            | T::Fn
            | T::SelfKw => {}
            // A statement ended before the bracket did, so there was no
            // bracket — the `<` was a comparison. Anything else (a literal, an
            // operator, a keyword that starts an expression) cannot appear in
            // a type and says the same thing.
            _ => return false,
        }
    }
    false
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

            // Comments first: each keeps the blank lines around it, and the
            // gap this token sees is then measured from the last one rather
            // than from before them all — which is what moved a blank line
            // from above a doc comment to below it.
            self.comments_before(token.span.start);
            let gap = self.gap_before(token.span.start);

            let closing = matches!(token.kind, T::RBrace | T::RBracket | T::RParen);
            if closing {
                self.depth = self.depth.saturating_sub(1);
            }
            if gap.breaks > 0 {
                self.end_line();
                // One blank line survives; more collapse. None is kept against
                // a closing bracket, where it is padding rather than
                // separation, and none at the top of the file, where it would
                // be gone the second time round.
                if gap.breaks > 1 && !closing && !self.out.is_empty() {
                    self.out.push('\n');
                }
            }

            self.start_line();
            let bracket = self.brackets.contains(&token.span.start);
            let text = &self.src[token.span.start as usize..token.span.end as usize];
            let space = self.needs_space(token.kind, bracket, token.span.start);
            if space || self.would_glue(text, token.kind) {
                self.out.push(' ');
            }
            self.out.push_str(text);

            if matches!(token.kind, T::LBrace | T::LBracket | T::LParen) {
                self.depth += 1;
            }
            if token.kind == T::Pipe {
                self.closure_params = !self.closure_params && self.is_value_position();
            }
            self.prev2 = self.prev;
            self.prev = Some(token.kind);
            self.prev_bracket = bracket;
            self.prev_text = (token.span.start, token.span.end);
            self.prev_end = token.span.end;
        }
        while self.out.ends_with('\n') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    /// Whether the token about to be written would run into the one before it
    /// with no space between.
    fn would_glue(&self, text: &str, kind: T) -> bool {
        let Some(prev) = self.prev else { return false };
        if !self.line_started || self.out.ends_with(' ') {
            return false;
        }
        let (start, end) = self.prev_text;
        glues(&self.src[start as usize..end as usize], prev, text, kind)
    }

    // ---- spacing ------------------------------------------------------------

    /// `bracket` says the token is a `<` or `>` of a type argument list, and
    /// `at` is where it starts.
    fn needs_space(&self, kind: T, bracket: bool, at: u32) -> bool {
        let Some(prev) = self.prev else { return false };
        if !self.line_started {
            return false;
        }
        // The `>` closing type arguments or generic parameters, and the `(`
        // after `fn f<T>`, are part of the name they follow.
        let after_type = self.prev_bracket && matches!(prev, T::Gt | T::Shr);
        // Nothing between a name and its argument list, its index, or a dot.
        if matches!(kind, T::LParen | T::LBracket)
            && (matches!(prev, T::Ident | T::RParen | T::RBracket | T::SelfKw) || after_type)
        {
            return false;
        }
        // `fn(T) -> U`, the type. A declaration is `fn name(`, so a `(`
        // straight after `fn` is always a function type.
        if kind == T::LParen && prev == T::Fn {
            return false;
        }
        // `..` in a struct literal's base, `P{ ..p }`, and a struct pattern's
        // rest, `P{ x, .. }`, stands apart like the fields around it. Only a
        // range hugs its ends.
        if kind == T::DotDot && matches!(prev, T::LBrace | T::Comma) {
            return true;
        }
        if kind == T::RBrace && prev == T::DotDot {
            return true;
        }
        if matches!(kind, T::Comma | T::Colon | T::Dot | T::DotDot | T::DotDotEq) {
            return false;
        }
        // `use std/task` is a path, not a division.
        if self.current_line().trim_start().starts_with("use ")
            && matches!(kind, T::Slash)
        {
            return false;
        }
        if prev == T::Slash && self.current_line().trim_start().starts_with("use ") {
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
        // Type arguments are tight: `Option<int>`, `Box<Box<int>>`, and the
        // parameter list of `impl<T>` or `fn f<T>`. Which `<` and `>` those
        // are is the parser's answer, or failing one, the forward scan's.
        if bracket || (self.prev_bracket && prev == T::Lt) {
            return false;
        }
        // `Point{ … }` is a literal and `struct Point {` is a declaration.
        // The parser says which is which when the file parses — including a
        // struct pattern opening a match arm, which has nothing before it on
        // its line to go by. Otherwise what precedes the name decides: a
        // literal appears where a value does.
        if kind == T::LBrace && matches!(prev, T::Ident | T::Gt) {
            return match &self.literal_braces {
                Some(literals) => !literals.contains(&at),
                None => !self.is_literal_head(),
            };
        }
        true
    }

    /// Whether a value could begin here, which is what makes a `-` a negation
    /// and a `|` a closure.
    ///
    /// `nil` and `_` are values too, in a pattern: `nil | _` is two
    /// alternatives, and read as a closure opening it came out `nil |_`.
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
                | Some(T::Nil)
                | Some(T::Underscore)
        )
    }

    /// Whether the `-` just written was a negation.
    ///
    /// A `}` ends a value as well as a block — `if a { 1 } else { 2 } - 3` —
    /// and a `-` after one on the same line is a subtraction.
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
                | Some(T::RBrace)
                | Some(T::SelfKw)
                | Some(T::True)
                | Some(T::False)
        )
    }

    /// Whether the name before a `{` is a struct literal's rather than a
    /// declaration's.
    ///
    /// `ui.Style{ … }` is a literal and `-> ui.Node {` is a return type, and
    /// the token immediately before the name is a `.` in both. So the name is
    /// stripped off the line and what precedes *it* decides — and the test is
    /// for a *value* position, because those are a short list and everything
    /// else is a declaration or a block.
    fn is_literal_head(&self) -> bool {
        let line = self.current_line();
        let head = line
            .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '.')
            .trim_end();
        if head.is_empty() {
            return false;
        }
        // `=` ends an assignment, but `<=`, `>=`, `==` and `!=` end a
        // comparison — and `if a <= b {` opens a block. A struct literal in a
        // comparison has to be parenthesised anyway, which the specification
        // already tells people for an `if` condition.
        if ["<=", ">=", "==", "!="].iter().any(|op| head.ends_with(op)) {
            return false;
        }
        // Arithmetic is *not* in this list, and that is the point of the list
        // being written out. `+ - * /` look like value positions and are not
        // reachable ones: Kite has no operator overloading, so nothing may be
        // added to or multiplied by a struct, and a literal can never follow
        // one. Including them meant `if a < 0.5 * b{` was read as a literal
        // head and kept whatever spacing it was written with — two spellings
        // of the same code, both of which `fmt --check` called formatted.
        ["=", "(", "[", ",", ":", "=>", "return", "check", "await"]
            .iter()
            .any(|token| head.ends_with(token))
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
    fn comments_before(&mut self, at: u32) {
        while self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            if c.span.start >= at {
                break;
            }
            self.next_comment += 1;
            self.write_comment(c);
        }
    }

    fn trailing_comments(&mut self) {
        while self.next_comment < self.comments.len() {
            let c = self.comments[self.next_comment];
            self.next_comment += 1;
            self.write_comment(c);
        }
    }

    /// A comment, where the author put it: at the end of the line it was on,
    /// or on a line of its own with the blank lines around it kept.
    fn write_comment(&mut self, c: Comment) {
        let text = self.text(c.span).trim_end().to_string();
        let gap = self.gap_before(c.span.start);
        if gap.breaks == 0 && self.line_started {
            self.out.push(' ');
        } else {
            self.end_line();
            // Blank lines above a comment are kept, except above the first
            // thing in the file, where the second pass would not see them.
            if gap.breaks > 1 && !self.out.is_empty() {
                self.out.push('\n');
            }
            self.start_line();
        }
        self.out.push_str(&text);
        self.prev_end = c.span.end;
        self.end_line();
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
