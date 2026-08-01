//! Token kinds, the keyword table, and the punctuation table.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TokenKind {
    // ---- literals ---------------------------------------------------------
    Int,
    Float,
    Str,
    Char,
    Ident,

    // ---- the 27 keywords --------------------------------------------------
    Async,
    Await,
    As,
    Break,
    Check,
    Continue,
    Defer,
    Else,
    Enum,
    False,
    Fn,
    For,
    If,
    Impl,
    In,
    Let,
    Match,
    Nil,
    Pub,
    Return,
    SelfKw,
    Struct,
    Trait,
    True,
    Type,
    Use,
    Var,

    // ---- delimiters -------------------------------------------------------
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // ---- punctuation ------------------------------------------------------
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,
    FatArrow,
    At,
    Underscore,
    Question,
    QuestionDot,
    QuestionQuestion,
    Slash,
    Plus,
    Minus,
    Star,
    Percent,
    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Shl,
    Shr,
    Bang,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,

    // ---- structural -------------------------------------------------------
    /// Statement separator, emitted only where a newline is significant.
    Newline,
    Eof,
}

impl TokenKind {
    /// Whether a line ending in this token continues onto the next.
    ///
    /// This is the specification's rule: operators, open delimiters, and
    /// commas continue a statement.
    pub fn continues_line(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            // open delimiters
            LParen | LBracket | LBrace
            // separators that must be followed by something
            | Comma | Colon | Arrow | FatArrow
            // binary and assignment operators
            | Plus | Minus | Star | Slash | Percent
            | Amp | AmpAmp | Pipe | PipePipe | Caret | Shl | Shr
            | Eq | EqEq | Ne | Lt | Le | Gt | Ge
            | PlusEq | MinusEq | StarEq | SlashEq | PercentEq
            | QuestionQuestion | Bang
            | Dot | QuestionDot | DotDot | DotDotEq
            // a keyword that cannot end a statement
            | Return | As | In | Check | Await | Else
        )
    }

    /// Whether this token can begin a declaration. Used by parser recovery to
    /// find a synchronisation point.
    pub fn starts_declaration(self) -> bool {
        use TokenKind::*;
        matches!(self, Pub | Fn | Struct | Enum | Trait | Impl | Type | Use | At)
    }

    /// Whether this token can begin a statement.
    pub fn starts_statement(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Let | Var | Return | If | For | Match | Break | Continue | Check | Defer
                | Ident | SelfKw | LBrace
        ) || self.starts_declaration()
    }

    pub fn describe(self) -> &'static str {
        use TokenKind::*;
        match self {
            Int => "an integer literal",
            Float => "a float literal",
            Str => "a string literal",
            Char => "a character literal",
            Ident => "an identifier",
            Newline => "a line break",
            Eof => "end of file",
            _ => self.text(),
        }
    }

    /// The token's surface text, for messages like "expected `}`".
    pub fn text(self) -> &'static str {
        use TokenKind::*;
        match self {
            Int => "integer",
            Float => "float",
            Str => "string",
            Char => "char",
            Ident => "identifier",
            Async => "async",
            Await => "await",
            As => "as",
            Break => "break",
            Check => "check",
            Continue => "continue",
            Defer => "defer",
            Else => "else",
            Enum => "enum",
            False => "false",
            Fn => "fn",
            For => "for",
            If => "if",
            Impl => "impl",
            In => "in",
            Let => "let",
            Match => "match",
            Nil => "nil",
            Pub => "pub",
            Return => "return",
            SelfKw => "self",
            Struct => "struct",
            Trait => "trait",
            True => "true",
            Type => "type",
            Use => "use",
            Var => "var",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Colon => ":",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            Arrow => "->",
            FatArrow => "=>",
            At => "@",
            Underscore => "_",
            Question => "?",
            QuestionDot => "?.",
            QuestionQuestion => "??",
            Slash => "/",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Percent => "%",
            Amp => "&",
            AmpAmp => "&&",
            Pipe => "|",
            PipePipe => "||",
            Caret => "^",
            Shl => "<<",
            Shr => ">>",
            Bang => "!",
            Eq => "=",
            EqEq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            Newline => "newline",
            Eof => "<eof>",
        }
    }

    /// The language's keyword census. Asserted against [`KEYWORDS`] in tests.
    pub const KEYWORD_COUNT: usize = 27;
}

/// The 27 keywords.
pub fn keyword(text: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match text {
        "async" => Async,
        "await" => Await,
        "as" => As,
        "break" => Break,
        "check" => Check,
        "continue" => Continue,
        "defer" => Defer,
        "else" => Else,
        "enum" => Enum,
        "false" => False,
        "fn" => Fn,
        "for" => For,
        "if" => If,
        "impl" => Impl,
        "in" => In,
        "let" => Let,
        "match" => Match,
        "nil" => Nil,
        "pub" => Pub,
        "return" => Return,
        "self" => SelfKw,
        "struct" => Struct,
        "trait" => Trait,
        "true" => True,
        "type" => Type,
        "use" => Use,
        "var" => Var,
        _ => return None,
    })
}

pub const KEYWORDS: [&str; TokenKind::KEYWORD_COUNT] = [
    "async", "await", "as", "break", "check", "continue", "defer", "else", "enum", "false", "fn",
    "for", "if", "impl", "in", "let", "match", "nil", "pub", "return", "self", "struct", "trait",
    "true", "type", "use", "var",
];

/// Punctuation, ordered longest-first so prefix matching is maximal munch.
pub const PUNCT: &[(&str, TokenKind)] = &[
    ("..=", TokenKind::DotDotEq),
    ("<<", TokenKind::Shl),
    (">>", TokenKind::Shr),
    ("?.", TokenKind::QuestionDot),
    ("??", TokenKind::QuestionQuestion),
    ("->", TokenKind::Arrow),
    ("=>", TokenKind::FatArrow),
    ("==", TokenKind::EqEq),
    ("!=", TokenKind::Ne),
    ("<=", TokenKind::Le),
    (">=", TokenKind::Ge),
    ("&&", TokenKind::AmpAmp),
    ("||", TokenKind::PipePipe),
    ("+=", TokenKind::PlusEq),
    ("-=", TokenKind::MinusEq),
    ("*=", TokenKind::StarEq),
    ("/=", TokenKind::SlashEq),
    ("%=", TokenKind::PercentEq),
    ("..", TokenKind::DotDot),
    ("(", TokenKind::LParen),
    (")", TokenKind::RParen),
    ("{", TokenKind::LBrace),
    ("}", TokenKind::RBrace),
    ("[", TokenKind::LBracket),
    ("]", TokenKind::RBracket),
    (",", TokenKind::Comma),
    (":", TokenKind::Colon),
    (".", TokenKind::Dot),
    ("@", TokenKind::At),
    ("?", TokenKind::Question),
    ("+", TokenKind::Plus),
    ("-", TokenKind::Minus),
    ("*", TokenKind::Star),
    ("/", TokenKind::Slash),
    ("%", TokenKind::Percent),
    ("&", TokenKind::Amp),
    ("|", TokenKind::Pipe),
    ("^", TokenKind::Caret),
    ("!", TokenKind::Bang),
    ("=", TokenKind::Eq),
    ("<", TokenKind::Lt),
    (">", TokenKind::Gt),
];
