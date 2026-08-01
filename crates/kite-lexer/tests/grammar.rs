//! The editor grammar has to know every keyword the lexer does.
//!
//! Highlighting drifting out of step with the language is the normal failure
//! here — a keyword gets added, and for months it reads as an ordinary
//! identifier in every editor that uses the grammar. That is worth a test
//! rather than a habit.

const GRAMMAR: &str = include_str!("../../../editors/vscode/syntaxes/kite.tmLanguage.json");

/// Whether a keyword appears as a whole word inside one of the grammar's
/// alternations. Parsing the JSON properly would need a dependency and would
/// not be more certain: the patterns are alternations of bare words, and a
/// keyword is covered exactly when it appears in one bounded by `|`, `(`, `)`
/// or `\b`.
fn covered(keyword: &str) -> bool {
    let bounds = |c: Option<char>| !matches!(c, Some(c) if c.is_alphanumeric() || c == '_');
    GRAMMAR.match_indices(keyword).any(|(i, _)| {
        let before = GRAMMAR[..i].chars().next_back();
        let after = GRAMMAR[i + keyword.len()..].chars().next();
        bounds(before) && bounds(after)
    })
}

#[test]
fn the_grammar_colours_every_keyword() {
    let missing: Vec<&str> = kite_lexer::KEYWORDS
        .iter()
        .copied()
        .filter(|k| !covered(k))
        .collect();
    assert!(
        missing.is_empty(),
        "not coloured by editors/vscode/syntaxes/kite.tmLanguage.json: {:?}",
        missing
    );
}

#[test]
fn the_grammar_has_balanced_brackets_and_the_right_scope() {
    // Enough of a parse to catch an unbalanced brace, which is how a grammar
    // usually breaks. Strings are skipped so a brace inside a regex does not
    // count.
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for c in GRAMMAR.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0, "unbalanced brackets in the grammar");
    }
    assert_eq!(depth, 0, "unbalanced brackets in the grammar");
    assert!(GRAMMAR.contains(r#""scopeName": "source.kite""#));
    assert!(GRAMMAR.contains(r#""fileTypes": ["kite"]"#));
}
