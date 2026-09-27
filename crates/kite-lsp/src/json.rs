//! Just enough JSON for the language-server protocol.
//!
//! Hand-written for the same reason the argument parser is: the compiler's
//! build time is a stated design target, and a serialisation framework is a
//! large dependency to take on for a protocol whose messages are this shallow.
//! Two hundred lines buys the whole of it.

use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Array(Vec<Json>),
    /// Ordered, so a message serialises the same way twice — which matters
    /// when the difference between two runs is what a test is looking at.
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn object(entries: Vec<(&str, Json)>) -> Json {
        Json::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn number(n: impl Into<f64>) -> Json {
        Json::Number(n.into())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// A dotted path: `get_path("params.textDocument.uri")`.
    pub fn path(&self, path: &str) -> Option<&Json> {
        let mut here = self;
        for part in path.split('.') {
            here = here.get(part)?;
        }
        Some(here)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Json::Number(n) => Some(*n as u32),
            _ => None,
        }
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => {
                if n.fract() == 0.0 && n.is_finite() {
                    let _ = write!(out, "{}", *n as i64);
                } else {
                    let _ = write!(out, "{}", n);
                }
            }
            Json::Str(s) => write_string(out, s),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse a JSON document. Returns `None` on anything malformed — a language
/// server that guesses at a broken message is a language server that hangs.
pub fn parse(text: &str) -> Option<Json> {
    let mut p = Parser { chars: text.chars().collect(), at: 0, depth: 0 };
    p.space();
    let value = p.value()?;
    Some(value)
}

/// How deeply arrays and objects may nest. The protocol's own messages are a
/// handful of levels deep; this is far past any of them and far short of the
/// stack.
const MAX_DEPTH: usize = 512;

struct Parser {
    chars: Vec<char>,
    at: usize,
    /// Arrays and objects open around the current value. The parser recurses
    /// once per level, so fifty thousand `[` overflowed the stack and ended
    /// the session on one message; past [`MAX_DEPTH`] the message is refused
    /// like any other that does not read.
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.at += 1;
        }
        c
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\n') | Some('\t') | Some('\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, c: char) -> Option<()> {
        (self.bump() == Some(c)).then_some(())
    }

    fn value(&mut self) -> Option<Json> {
        self.space();
        match self.peek()? {
            '{' | '[' => {
                if self.depth >= MAX_DEPTH {
                    return None;
                }
                self.depth += 1;
                let nested = if self.peek() == Some('{') { self.object() } else { self.array() };
                self.depth -= 1;
                nested
            }
            '"' => self.string().map(Json::Str),
            't' => self.word("true").map(|_| Json::Bool(true)),
            'f' => self.word("false").map(|_| Json::Bool(false)),
            'n' => self.word("null").map(|_| Json::Null),
            _ => self.number(),
        }
    }

    fn word(&mut self, word: &str) -> Option<()> {
        for c in word.chars() {
            self.expect(c)?;
        }
        Some(())
    }

    fn object(&mut self) -> Option<Json> {
        self.expect('{')?;
        let mut map = BTreeMap::new();
        self.space();
        if self.peek() == Some('}') {
            self.at += 1;
            return Some(Json::Object(map));
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.expect(':')?;
            let value = self.value()?;
            map.insert(key, value);
            self.space();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(Json::Object(map)),
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(']') {
            self.at += 1;
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.space();
            match self.bump()? {
                ',' => continue,
                ']' => return Some(Json::Array(items)),
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump()? {
                '"' => return Some(out),
                '\\' => match self.bump()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let code = self.hex4()?;
                        match code {
                            // A surrogate pair arrives as two escapes; the
                            // second completes the first.
                            0xD800..=0xDBFF => {
                                let before = self.at;
                                let low = match (self.bump(), self.bump()) {
                                    (Some('\\'), Some('u')) => self.hex4(),
                                    _ => None,
                                };
                                match low {
                                    Some(low @ 0xDC00..=0xDFFF) => {
                                        let combined =
                                            0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                        out.push(char::from_u32(combined)?);
                                    }
                                    // Half a pair is not a character. JSON
                                    // allows it all the same, so it becomes
                                    // U+FFFD and whatever followed is read
                                    // again on its own — refusing the whole
                                    // message over it would lose the request.
                                    _ => {
                                        out.push('\u{FFFD}');
                                        self.at = before;
                                    }
                                }
                            }
                            0xDC00..=0xDFFF => out.push('\u{FFFD}'),
                            _ => out.push(char::from_u32(code)?),
                        }
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
    }

    /// The four hex digits of a `\u` escape.
    fn hex4(&mut self) -> Option<u32> {
        let mut code = 0u32;
        for _ in 0..4 {
            code = code * 16 + self.bump()?.to_digit(16)?;
        }
        Some(code)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        if self.peek() == Some('-') {
            self.at += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
        {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        text.parse::<f64>().ok().map(Json::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_message() {
        let text = r#"{"id":1,"method":"initialize","params":{"rootUri":null}}"#;
        let value = parse(text).expect("parses");
        assert_eq!(value.path("params.rootUri"), Some(&Json::Null));
        assert_eq!(value.get("method").and_then(|m| m.as_str()), Some("initialize"));
        assert_eq!(value.to_text(), text);
    }

    #[test]
    fn escapes_survive() {
        let value = parse(r#"{"a":"line\nbreak \"quoted\" é"}"#).expect("parses");
        assert_eq!(value.get("a").unwrap().as_str(), Some("line\nbreak \"quoted\" é"));
        assert_eq!(parse(&value.to_text()), Some(value));
    }

    #[test]
    fn a_broken_message_is_rejected_rather_than_guessed_at() {
        assert!(parse("{").is_none());
        assert!(parse(r#"{"a" 1}"#).is_none());
        assert!(parse("").is_none());
    }

    /// Half a surrogate pair is legal JSON and not a character. It used to
    /// fail the parse — and a failed parse ended the session — or, followed by
    /// an escape below U+DC00, underflow computing the pair.
    #[test]
    fn a_lone_surrogate_becomes_the_replacement_character() {
        let value = parse(r#"{"a":"\ud800","b":"x\udc00y","c":"\ud800A","d":"😀"}"#)
            .expect("parses");
        assert_eq!(value.get("a").unwrap().as_str(), Some("\u{FFFD}"));
        assert_eq!(value.get("b").unwrap().as_str(), Some("x\u{FFFD}y"));
        assert_eq!(value.get("c").unwrap().as_str(), Some("\u{FFFD}A"));
        assert_eq!(value.get("d").unwrap().as_str(), Some("😀"));
    }

    /// Nesting past the limit is refused rather than recursed into until the
    /// stack runs out — which ended the session over one message.
    #[test]
    fn nesting_past_the_limit_is_refused_not_overflowed() {
        let deep = format!("{}{}", "[".repeat(50_000), "]".repeat(50_000));
        assert!(parse(&deep).is_none());
        let objects = format!("{}1{}", "{\"a\":".repeat(50_000), "}".repeat(50_000));
        assert!(parse(&objects).is_none());
        let fine = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse(&fine).is_some());
    }

    #[test]
    fn numbers_and_arrays() {
        let value = parse("[1, 2.5, -3, true, null]").expect("parses");
        assert_eq!(value.to_text(), "[1,2.5,-3,true,null]");
    }
}
