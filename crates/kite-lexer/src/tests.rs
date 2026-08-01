use super::*;
use kite_span::SourceMap;
use TokenKind as T;

/// Tokenise, returning kinds with `Eof` dropped, plus the diagnostic bag.
fn lex(src: &str) -> (Vec<TokenKind>, DiagBag) {
    let mut map = SourceMap::new();
    let f = map.add("t.kite", src);
    let mut diags = DiagBag::new();
    let toks = tokenize(f, src, &mut diags);
    let kinds = toks
        .iter()
        .map(|t| t.kind)
        .filter(|k| *k != T::Eof)
        .collect();
    (kinds, diags)
}

fn kinds(src: &str) -> Vec<TokenKind> {
    let (k, d) = lex(src);
    assert!(!d.has_errors(), "unexpected diagnostics for {:?}", src);
    k
}

/// Kinds with newlines removed, for tests that do not care about separation.
fn bare(src: &str) -> Vec<TokenKind> {
    kinds(src).into_iter().filter(|k| *k != T::Newline).collect()
}

// ---- keywords -------------------------------------------------------------

#[test]
fn keyword_table_matches_census() {
    assert_eq!(kinds::KEYWORDS.len(), TokenKind::KEYWORD_COUNT);
    for kw in kinds::KEYWORDS {
        assert!(kinds::keyword(kw).is_some(), "{} missing from table", kw);
    }
}

#[test]
fn every_keyword_lexes_as_itself() {
    for kw in kinds::KEYWORDS {
        let k = bare(kw);
        assert_eq!(k.len(), 1, "{}", kw);
        assert_ne!(k[0], T::Ident, "{} lexed as an identifier", kw);
        assert_eq!(k[0].text(), kw);
    }
}

#[test]
fn identifiers_that_merely_start_with_a_keyword_are_identifiers() {
    assert_eq!(bare("iffy format returned"), vec![T::Ident, T::Ident, T::Ident]);
}

#[test]
fn non_latin_identifiers_are_accepted() {
    // Kite identifiers are Unicode; this is a Burmese word.
    assert_eq!(bare("let နာမည် = 1"), vec![T::Let, T::Ident, T::Eq, T::Int]);
}

#[test]
fn underscore_alone_is_not_an_identifier() {
    assert_eq!(bare("_"), vec![T::Underscore]);
    assert_eq!(bare("_x"), vec![T::Ident]);
}

// ---- numbers --------------------------------------------------------------

#[test]
fn integer_and_float_forms() {
    assert_eq!(bare("42"), vec![T::Int]);
    assert_eq!(bare("1_000_000"), vec![T::Int]);
    assert_eq!(bare("0xFF"), vec![T::Int]);
    assert_eq!(bare("0o755"), vec![T::Int]);
    assert_eq!(bare("0b1010_1101"), vec![T::Int]);
    assert_eq!(bare("42i32"), vec![T::Int]);
    assert_eq!(bare("3.14"), vec![T::Float]);
    assert_eq!(bare("2.5f32"), vec![T::Float]);
    assert_eq!(bare("1e10"), vec![T::Float]);
    assert_eq!(bare("1.5e-3"), vec![T::Float]);
}

/// The reason `scan_number` refuses to eat `.` unless a digit follows.
#[test]
fn range_after_integer_is_not_a_float() {
    assert_eq!(bare("0..10"), vec![T::Int, T::DotDot, T::Int]);
    assert_eq!(bare("0..=10"), vec![T::Int, T::DotDotEq, T::Int]);
}

#[test]
fn method_call_on_integer_is_not_a_float() {
    assert_eq!(bare("1.max(2)"), vec![T::Int, T::Dot, T::Ident, T::LParen, T::Int, T::RParen]);
}

#[test]
fn malformed_numbers_report_e0004() {
    let (_, d) = lex("0x");
    assert!(d.iter().any(|x| x.code == Some(codes::E0004)));
    let (_, d) = lex("1__0");
    assert!(d.iter().any(|x| x.code == Some(codes::E0004)));
}

// ---- strings --------------------------------------------------------------

#[test]
fn string_forms() {
    assert_eq!(bare(r#""hello""#), vec![T::Str]);
    assert_eq!(bare(r#""with \"escape\"""#), vec![T::Str]);
    assert_eq!(bare("\"\"\"\nblock\n\"\"\""), vec![T::Str]);
}

#[test]
fn interpolation_with_nested_quotes_stays_one_token() {
    // The `"` inside \( ) must not terminate the outer literal.
    assert_eq!(bare(r#""a \(f("x")) b""#), vec![T::Str]);
}

#[test]
fn interpolation_with_nested_parens_stays_one_token() {
    assert_eq!(bare(r#""v=\(f(g(1), h(2)))""#), vec![T::Str]);
}

#[test]
fn unterminated_string_reports_e0001() {
    let (_, d) = lex("\"oops\n");
    assert!(d.iter().any(|x| x.code == Some(codes::E0001)));
}

// ---- comments -------------------------------------------------------------

#[test]
fn line_and_doc_comments_are_trivia() {
    assert_eq!(bare("// nothing\nlet x = 1 // trailing"), vec![T::Let, T::Ident, T::Eq, T::Int]);
    assert_eq!(bare("/// doc\nfn f()"), vec![T::Fn, T::Ident, T::LParen, T::RParen]);
}

#[test]
fn block_comment_reports_e0005_once() {
    let (_, d) = lex("/* a */ let x = 1");
    let hits = d.iter().filter(|x| x.code == Some(codes::E0005)).count();
    assert_eq!(hits, 1, "one diagnostic per cause");
}

// ---- newline termination --------------------------------------------------

#[test]
fn newline_separates_statements() {
    assert_eq!(
        kinds("let a = 1\nlet b = 2"),
        vec![T::Let, T::Ident, T::Eq, T::Int, T::Newline, T::Let, T::Ident, T::Eq, T::Int]
    );
}

#[test]
fn trailing_operator_continues_the_line() {
    assert_eq!(
        kinds("let a = 1 +\n2"),
        vec![T::Let, T::Ident, T::Eq, T::Int, T::Plus, T::Int]
    );
}

#[test]
fn blank_lines_do_not_produce_repeated_separators() {
    assert_eq!(
        kinds("let a = 1\n\n\n\nlet b = 2"),
        vec![T::Let, T::Ident, T::Eq, T::Int, T::Newline, T::Let, T::Ident, T::Eq, T::Int]
    );
}

#[test]
fn leading_newlines_are_dropped() {
    assert_eq!(kinds("\n\nlet a = 1"), vec![T::Let, T::Ident, T::Eq, T::Int]);
}

/// Multi-line argument lists must work *without* a trailing comma. This is why
/// the lexer tracks delimiter depth rather than only inspecting adjacent tokens.
#[test]
fn newlines_inside_parens_are_insignificant() {
    assert_eq!(
        kinds("f(\n  a,\n  b\n)"),
        vec![T::Ident, T::LParen, T::Ident, T::Comma, T::Ident, T::RParen]
    );
}

#[test]
fn newlines_inside_brackets_are_insignificant() {
    assert_eq!(
        kinds("[\n  1,\n  2\n]"),
        vec![T::LBracket, T::Int, T::Comma, T::Int, T::RBracket]
    );
}

/// Braces are the opposite: blocks need statement separation inside them.
#[test]
fn newlines_inside_braces_are_significant() {
    assert_eq!(
        kinds("{\n  a\n  b\n}"),
        vec![T::LBrace, T::Ident, T::Newline, T::Ident, T::Newline, T::RBrace]
    );
}

#[test]
fn else_is_never_separated_from_its_brace() {
    assert_eq!(
        kinds("if a {\n} else {\n}"),
        vec![T::If, T::Ident, T::LBrace, T::RBrace, T::Else, T::LBrace, T::RBrace]
    );
}

#[test]
fn leading_dot_continues_a_method_chain() {
    assert_eq!(
        kinds("items\n  .filter(f)\n  .map(g)"),
        vec![
            T::Ident,
            T::Dot, T::Ident, T::LParen, T::Ident, T::RParen,
            T::Dot, T::Ident, T::LParen, T::Ident, T::RParen,
        ]
    );
}

#[test]
fn final_statement_is_terminated_by_a_trailing_newline() {
    assert_eq!(kinds("let a = 1\n"), vec![T::Let, T::Ident, T::Eq, T::Int, T::Newline]);
}

#[test]
fn file_without_trailing_newline_still_ends_cleanly() {
    assert_eq!(kinds("let a = 1"), vec![T::Let, T::Ident, T::Eq, T::Int]);
}

// ---- punctuation ----------------------------------------------------------

/// `?` is not a token in Kite at all — no optional chaining, no coalescing,
/// no ternary. An inline `if` expression does that work in the open.
#[test]
fn question_mark_is_not_a_token() {
    let (_, d) = lex("a ? b");
    assert!(
        d.iter().any(|x| x.code == Some(codes::E0002)),
        "`?` should not lex"
    );
}

#[test]
fn maximal_munch_on_multi_character_operators() {
    assert_eq!(bare("..= .. ."), vec![T::DotDotEq, T::DotDot, T::Dot]);
    assert_eq!(bare("== = =>"), vec![T::EqEq, T::Eq, T::FatArrow]);
    assert_eq!(bare("<= << <"), vec![T::Le, T::Shl, T::Lt]);
    assert_eq!(bare("-> - -="), vec![T::Arrow, T::Minus, T::MinusEq]);
}

#[test]
fn invalid_character_reports_e0002_and_lexing_continues() {
    let (k, d) = lex("let a = §1");
    assert!(d.iter().any(|x| x.code == Some(codes::E0002)));
    // The `1` after the bad byte is still tokenised.
    assert!(k.contains(&T::Int), "lexer did not recover: {:?}", k);
}

// ---- the Phase 1 program --------------------------------------------------

#[test]
fn phase_one_program_lexes() {
    let src = "\
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    let x = add(2, 3)
    if x > 4 {
        io.print(\"big\")
    }
    for i in 0..x {
        io.print(i)
    }
}
";
    let (k, d) = lex(src);
    assert!(!d.has_errors(), "{:?}", d.iter().next().map(|x| &x.message));
    assert!(k.contains(&T::Fn));
    assert!(k.contains(&T::DotDot));
    assert!(k.contains(&T::Newline));
}

// ---- spans ----------------------------------------------------------------

#[test]
fn spans_cover_exactly_the_token_text() {
    let src = "let total = 42";
    let mut map = SourceMap::new();
    let f = map.add("t.kite", src);
    let mut diags = DiagBag::new();
    let toks = tokenize(f, src, &mut diags);
    assert_eq!(map.snippet(toks[0].span), "let");
    assert_eq!(map.snippet(toks[1].span), "total");
    assert_eq!(map.snippet(toks[2].span), "=");
    assert_eq!(map.snippet(toks[3].span), "42");
}
