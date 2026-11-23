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

/// Kite has no `char` type: `'a'` lexes, and the checker refuses it (`E0200`).
/// The grammar coloured it as a string literal, which says it is one.
#[test]
fn the_grammar_does_not_colour_a_char_literal() {
    assert!(!GRAMMAR.contains("\"#char\""), "a `char` rule is included");
    assert!(!GRAMMAR.contains("\"char\":"), "a `char` rule is defined");
    assert!(!GRAMMAR.contains("string.quoted.single"));
}

/// The interpolation ended at the first `)`, so in `"\(f(x))"` the last one
/// was coloured as text. A `(` inside an interpolation has to open a group of
/// its own, tried before the general rules, which would otherwise take it as a
/// lone parenthesis and leave the `)` to close the interpolation early.
#[test]
fn an_interpolation_nests_its_parentheses() {
    let at = GRAMMAR.find("\"interpolation\": {").expect("an interpolation rule");
    let rule = &GRAMMAR[at..];
    let rule = &rule[..rule.find("\"escape\"").expect("the rule after it")];
    let group = rule.find("\"#interpolation-parens\"").expect("the interpolation includes a group");
    let general = rule.find("\"$self\"").expect("and the general rules");
    assert!(group < general, "the group must come before the general rules");
    assert!(rule.contains("\"interpolation-parens\": {"), "the group rule is defined");
    // And it recurses, so `\(f(g(x)))` is three levels.
    let defined = &rule[rule.find("\"interpolation-parens\": {").unwrap()..];
    assert!(defined.contains("\"begin\": \"\\\\(\"") && defined.contains("\"end\": \"\\\\)\""));
    assert!(defined.matches("\"#interpolation-parens\"").count() >= 1);
}
