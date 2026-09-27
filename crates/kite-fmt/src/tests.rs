use super::*;

/// Format a file the test expects to be formattable.
fn fmt(src: &str) -> String {
    match format(src) {
        Ok(out) => out,
        Err(e) => panic!("{}\n--- in ---\n{}", e, src),
    }
}

fn same(src: &str) {
    assert_eq!(fmt(src), src, "formatting changed an already-formatted file");
}

/// The property that matters most: formatting twice is formatting once.
fn idempotent(src: &str) -> String {
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(once, twice, "formatting is not idempotent:\n{}", once);
    once
}

#[test]
fn indentation_is_four_spaces_per_bracket() {
    let out = idempotent("fn main() {\nio.print(1)\n}\n");
    assert_eq!(out, "fn main() {\n    io.print(1)\n}\n");
}

/// Where the lines end is the author's decision; everything around them is
/// the formatter's.
#[test]
fn line_breaks_are_kept() {
    let out = idempotent("fn f() {\n    let xs = [\n        1,\n        2,\n    ]\n}\n");
    assert_eq!(out, "fn f() {\n    let xs = [\n        1,\n        2,\n    ]\n}\n");
    let inline = idempotent("fn f() {\n    let xs = [1, 2]\n}\n");
    assert_eq!(inline, "fn f() {\n    let xs = [1, 2]\n}\n");
}

#[test]
fn a_struct_literal_keeps_its_shape() {
    let out = idempotent("fn f() {\n    let p = Point{ x: 1.0, y: 2.0 }\n}\n");
    assert!(out.contains("Point{ x: 1.0, y: 2.0 }"), "{}", out);
}

#[test]
fn nesting_compounds() {
    let out = idempotent("fn f() {\nif a {\nfor i in 0..3 {\nio.print(i)\n}\n}\n}\n");
    assert_eq!(
        out,
        "fn f() {\n    if a {\n        for i in 0..3 {\n            io.print(i)\n        }\n    }\n}\n"
    );
}

#[test]
fn binary_operators_are_spaced_and_prefixes_are_not() {
    let out = fmt("fn f() {\nlet x = 1+2*3\nlet y = -x\nlet z = !ok\n}\n");
    assert!(out.contains("let x = 1 + 2 * 3"), "{}", out);
    assert!(out.contains("let y = -x"), "{}", out);
    assert!(out.contains("let z = !ok"), "{}", out);
}

#[test]
fn a_call_hugs_its_arguments() {
    let out = fmt("fn f() {\nio.print( a , b )\n}\n");
    assert!(out.contains("io.print(a, b)"), "{}", out);
}

#[test]
fn a_field_type_is_written_tight() {
    let out = idempotent("struct P {\nx: int\nname: str\n}\n");
    assert_eq!(out, "struct P {\n    x: int\n    name: str\n}\n");
}

#[test]
fn type_arguments_keep_no_spaces() {
    let out = fmt("fn f(x: Option<int>) -> Option<str> {\n}\n");
    assert!(out.contains("Option<int>"), "{}", out);
    assert!(out.contains("-> Option<str>"), "{}", out);
}

#[test]
fn a_comparison_keeps_its_spaces() {
    let out = fmt("fn f() {\nif a < b {\nio.print(1)\n}\n}\n");
    assert!(out.contains("if a < b {"), "{}", out);
}

/// `<=` ends a comparison and `=` ends an assignment, and only one of them
/// means a struct literal follows.
#[test]
fn a_comparison_before_a_block_is_not_a_struct_literal() {
    for op in ["<=", ">=", "==", "!="] {
        let out = fmt(&format!("fn f() {{\nif a {} b {{\nio.print(1)\n}}\n}}\n", op));
        assert!(out.contains(&format!("if a {} b {{", op)), "{}", out);
    }
    let literal = fmt("fn f() {\nlet p = Point{ x: 1 }\n}\n");
    assert!(literal.contains("Point{ x: 1 }"), "{}", literal);
}

/// Arithmetic before a block is a block, not a struct literal.
///
/// `+ - * /` look like positions a value could start in, and are not reachable
/// ones: Kite has no operator overloading, so nothing can be added to or
/// multiplied by a struct. Treating them as literal heads meant a condition
/// ending in an identifier kept whatever spacing it was written with — and
/// `std/math` had one, which `fmt --check` and the CI job that runs it both
/// called formatted.
#[test]
fn arithmetic_before_a_block_is_not_a_struct_literal() {
    for op in ["+", "-", "*", "/"] {
        let out = fmt(&format!(
            "fn f(a: float, b: float) {{\nif a < 0.5 {} b{{\nio.print(1)\n}}\n}}\n",
            op
        ));
        assert!(out.contains(&format!("0.5 {} b {{", op)), "{}", out);
    }
    // And what the exclusion could have cost, which is nothing: a literal in
    // any of the positions one really does appear in still hugs its brace.
    for head in ["let p = ", "return ", "check ", "f(", "[", "x: "] {
        let src = format!("fn f() {{\n{}Point{{ x: 1 }}\n}}\n", head);
        assert!(fmt(&src).contains("Point{ x: 1 }"), "{}", src);
    }
}

#[test]
fn a_comment_on_its_own_line_stays_there() {
    let out = idempotent("// a note\nfn main() {\n    io.print(1)\n}\n");
    assert!(out.starts_with("// a note\nfn main()"), "{}", out);
}

#[test]
fn a_trailing_comment_stays_on_its_line() {
    let out = idempotent("fn main() {\n    io.print(1) // why\n}\n");
    assert!(out.contains("io.print(1) // why"), "{}", out);
}

#[test]
fn a_comment_inside_a_block_is_indented_with_it() {
    let out = idempotent("fn main() {\n    // inside\n    io.print(1)\n}\n");
    assert!(out.contains("\n    // inside\n"), "{}", out);
}

#[test]
fn one_blank_line_survives_and_more_collapse() {
    let out = idempotent("fn a() {\n}\n\n\n\nfn b() {\n}\n");
    assert_eq!(out, "fn a() {\n}\n\nfn b() {\n}\n");
}

#[test]
fn a_blank_line_before_a_closing_brace_goes() {
    let out = idempotent("fn a() {\n    io.print(1)\n\n}\n");
    assert_eq!(out, "fn a() {\n    io.print(1)\n}\n");
}

/// A closure's parameters have colons in them, and the body after them is a
/// value: `|a: int, b: int| a < b` is a comparison, not a type argument list.
#[test]
fn a_comparison_in_a_closure_body_keeps_its_spaces() {
    let out = idempotent("fn f() {\n    let s = sorted(xs, |a: int, b: int| a < b)\n}\n");
    assert!(out.contains("|a: int, b: int| a < b"), "{}", out);
}

/// A closure that declares a return type is still writing a type after its
/// parameters.
#[test]
fn a_closures_return_type_stays_tight() {
    let out = idempotent("fn f() {\n    let g = |n: int| -> Option<int> {\n        return n\n    }\n}\n");
    assert!(out.contains("-> Option<int>"), "{}", out);
}

#[test]
fn a_closure_keeps_its_pipes_tight() {
    let out = fmt("fn f() {\nlet double = map(xs, |x: int| x * 2)\n}\n");
    assert!(out.contains("|x: int| x * 2"), "{}", out);
}

#[test]
fn a_file_ends_with_exactly_one_newline() {
    assert!(fmt("fn main() {\n}\n\n\n").ends_with("}\n"));
    assert!(!fmt("fn main() {\n}").ends_with("}\n\n"));
    assert_eq!(fmt(""), "");
}

/// A file that does not parse still formats: tokens are all this needs, which
/// is exactly when someone reaches for a formatter.
#[test]
fn a_broken_file_still_formats() {
    let out = fmt("fn main() {\nlet x =\n}\n");
    assert!(out.contains("let x ="), "{}", out);
}

#[test]
fn already_formatted_files_are_left_alone() {
    same("fn add(a: int, b: int) -> int {\n    return a + b\n}\n");
    same("struct Point {\n    x: float\n    y: float\n}\n");
    same("// A note.\nfn main() {\n    let xs = [1, 2, 3]\n    io.print(xs.len())\n}\n");
}

#[test]
fn is_formatted_agrees_with_format() {
    assert_eq!(is_formatted("fn main() {\n    io.print(1)\n}\n"), Ok(true));
    assert_eq!(is_formatted("fn main() {\nio.print(1)\n}\n"), Ok(false));
}

/// Every Kite file in the tree formats to something that still compiles, and
/// formatting it again changes nothing. This is the test that matters: a
/// formatter is only useful if it can be trusted on a real file.
/// The one declaration whose `=` does not begin a value. Both sides of an
/// alias are types, and reading the right-hand one as a value spaced its
/// brackets like arithmetic — `Option<json.Json>` came out as
/// `Option < json.Json >`, and stayed there, because the mangled form was a
/// fixed point of the same rule that produced it.
#[test]
fn a_type_alias_is_a_type_on_both_sides() {
    same("pub type Doc = Option<json.Json>\n");
    same("type Pair = Map<str, int>\n");
    same("type Arr = [Option<int>]\n");
    let out = idempotent("type Nested = Box<Box<int>>\n");
    assert_eq!(out, "type Nested = Box<Box<int>>\n");
    // And it repairs a file the old formatter already spaced out.
    assert_eq!(
        fmt("pub type Doc = Option < json.Json >\n"),
        "pub type Doc = Option<json.Json>\n"
    );
}

/// The other half of the same ambiguity. A struct literal's field separator
/// reads exactly like a type annotation's, so the rule that looked for a `:`
/// on the line tightened every comparison written in a field value —
/// `tone: if low > 0` came out as `low> 0`, one-sided and wrong.
#[test]
fn a_comparison_in_a_field_value_is_not_a_type_argument() {
    same("let x = Flag{ on: count > 0, n: 1 }\n");
    same("let y = Flag{ on: a < b, n: 2 }\n");
    same("let z = Flag{ on: sum(a, b) > 0, n: 3 }\n");
    same("let t = Stat{ tone: if low > 0 { \"warn\" } else { \"\" } }\n");
    // Still tight where it really is a type, on a line that also has a colon.
    same("let m: Option<int> = nil\n");
    same("fn f(a: Map<str, int>) -> Option<Box<int>> {\n}\n");
    assert_eq!(
        fmt("let t = Stat{ tone: if low> 0 { \"w\" } else { \"\" } }\n"),
        "let t = Stat{ tone: if low > 0 { \"w\" } else { \"\" } }\n"
    );
}

/// Every `.kite` file in the tree is formatted already, and formatting it
/// again changes nothing — what CI's `kitec fmt --check` asks of each file,
/// asked here too so that `cargo test` sees it. The recovery corpus is in
/// here: its files are broken on purpose, and formatting has to leave them
/// broken the same way.
///
/// This walked four directories, not the tree, and only asked that
/// formatting be idempotent — so a file could drift from the formatter's
/// layout, and the corpus, the site and two examples were never looked at.
#[test]
fn the_whole_tree_survives_formatting() {
    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            // What a build or a package manager put there is not the tree's.
            if ["node_modules", "dist", "target", ".kite"].iter().any(|n| name == *n) {
                continue;
            }
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "kite") {
                files.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["std", "examples", "tests", "site"] {
        walk(&root.join(dir), &mut files);
    }
    assert!(files.len() > 150, "only {} files found", files.len());
    for path in files {
        let src = std::fs::read_to_string(&path).expect("read");
        let once = fmt(&src);
        assert!(once == src, "{} is not formatted", path.display());
        let twice = fmt(&once);
        assert_eq!(once, twice, "formatting {} is not idempotent", path.display());
    }
}

/// A file the lexer cannot read is refused, not laid out from the tokens that
/// survived. Each of these used to come back shorter than it went in, with
/// `kitec fmt` reporting success.
#[test]
fn a_file_with_lexical_errors_is_refused() {
    // An unterminated `/*` swallowed the rest of the file.
    let err = format("fn main() {\n    let x = 1 /* not a comment\n    io.print(x)\n}\n")
        .expect_err("a `/*` must be refused");
    assert!(
        matches!(&err, FormatError::Lexical { line: 2, col: 15, message } if message.contains("block comments")),
        "{:?}",
        err
    );
    assert!(err.to_string().contains("cannot format a file with lexical errors"), "{}", err);
    // Characters that begin no token vanished from between the ones that did:
    // `a ?? b` became `a b`, and two statements on one line ran together.
    for src in [
        "fn main() {\n    let x = a ?? b\n}\n",
        "fn main() {\n    let x = 1; let y = 2\n}\n",
        "fn main() {\n    let s = $x\n}\n",
        "fn main() {\n    let s = #x\n}\n",
        "fn main() {\n    let s = `x`\n}\n",
        "fn main() {\n    let s = a\u{2026}b\n}\n",
        "fn main() {\n    let s = \"never closed\n}\n",
    ] {
        match format(src) {
            Err(FormatError::Lexical { line: 2, .. }) => {}
            other => panic!("expected a refusal at line 2 for {:?}, got {:?}", src, other),
        }
        assert!(is_formatted(src).is_err(), "{:?}", src);
    }
}

/// The last line of defence: an output that does not say what the input said
/// is thrown away. Whitespace carries meaning in exactly one way — it keeps
/// tokens apart — so gluing two together is as much a change as deleting one.
#[test]
fn an_output_that_changes_the_tokens_is_refused() {
    let lex = |src: &str| kite_lexer::tokenize(FileId(0), src, &mut DiagBag::new());
    let src = "let x = a - == b\n";
    assert!(faithful(src, &lex(src), "let x = a - == b\n"));
    // `-` and `==` glued into `-=` and `=`.
    assert!(!faithful(src, &lex(src), "let x = a -== b\n"));
    // A deleted token.
    assert!(!faithful(src, &lex(src), "let x = a == b\n"));
    // A line break removed, which joins two statements.
    let src = "let a = 1\nlet b = 2\n";
    assert!(!faithful(src, &lex(src), "let a = 1 let b = 2\n"));
}

/// Blank lines at the top of a file go on the first pass, not the second.
/// They used to be collapsed to one there, and the second pass took that one
/// away — `fmt --check` could not pass on its own output.
#[test]
fn leading_blank_lines_go_in_one_pass() {
    assert_eq!(idempotent("\n\n\nfn main() {\n}\n"), "fn main() {\n}\n");
    assert_eq!(idempotent("\n\n// a note\n\nfn main() {\n}\n"), "// a note\n\nfn main() {\n}\n");
    assert_eq!(idempotent("// a note\nfn main() {\n}\n"), "// a note\nfn main() {\n}\n");
}

/// Which `<` and `>` are type brackets is the parser's answer, and a
/// comparison keeps its spaces however much it looks like a type.
#[test]
fn a_comparison_that_looks_like_a_type_keeps_its_spaces() {
    same("fn f() {\n    g(a < b, c > d)\n}\n");
    same("fn f() {\n    let x = a > (b - 1)\n}\n");
    same("fn f() {\n    if n as int < limit {\n    }\n}\n");
    assert_eq!(fmt("fn f() {\n    g(a<b, c> d)\n}\n"), "fn f() {\n    g(a < b, c > d)\n}\n");
    assert_eq!(fmt("fn f() {\n    let x = a >(b - 1)\n}\n"), "fn f() {\n    let x = a > (b - 1)\n}\n");
    // And a type is tight wherever it is.
    same("fn f<T: Show + Eq>(x: T) -> Option<T> {\n}\n");
    assert_eq!(fmt("fn f < T: Show + Eq >(x: T) {\n}\n"), "fn f<T: Show + Eq>(x: T) {\n}\n");
    same("fn f() {\n    let a: Option<int>= nil\n}\n");
    same("type B = Box<Box<int>>\n");
}

/// A file that does not parse falls back to reading forward from each `<`,
/// which knows a generic bound's `+` now.
#[test]
fn a_broken_file_still_spaces_its_types() {
    let out = fmt("fn f<T: Show + Eq>(x: T) {\n    let y =\n}\n");
    assert!(out.starts_with("fn f<T: Show + Eq>(x: T) {"), "{}", out);
}

/// `}` ends a value as well as a block: a `-` after one is a subtraction.
#[test]
fn a_minus_after_a_brace_is_a_subtraction() {
    same("fn f() {\n    let x = if a { 1 } else { 2 } - 3\n}\n");
    assert_eq!(
        fmt("fn f() {\n    let x = if a { 1 } else { 2 } -3\n}\n"),
        "fn f() {\n    let x = if a { 1 } else { 2 } - 3\n}\n"
    );
}

/// `nil` and `_` are values in a pattern, so the `|` after one separates
/// alternatives rather than opening a closure.
#[test]
fn a_pattern_alternative_after_nil_is_spaced() {
    same("fn f() {\n    match x {\n        nil | _ => 1,\n    }\n}\n");
    assert_eq!(
        fmt("fn f() {\n    match x {\n        nil |_ => 1,\n    }\n}\n"),
        "fn f() {\n    match x {\n        nil | _ => 1,\n    }\n}\n"
    );
}

/// `..` in a struct literal's base and a struct pattern's rest stands apart
/// like the fields around it, as §5.3 writes it. It hugged the brace.
#[test]
fn a_struct_base_and_rest_stand_apart() {
    same("fn f() {\n    let q = Point{ ..p, y: 5 }\n}\n");
    assert_eq!(
        fmt("fn f() {\n    let q = Point{..p, y: 5 }\n}\n"),
        "fn f() {\n    let q = Point{ ..p, y: 5 }\n}\n"
    );
    same("fn f() {\n    match p {\n        Point{ x, .. } => x,\n    }\n}\n");
    // A range still hugs its ends, open or not.
    same("fn f() {\n    let a = xs[1..3]\n    let b = xs[1..]\n    let c = xs[..2]\n}\n");
}

/// A tuple index after a tuple index stays two tokens: `t.0.1` lexes as the
/// two it is, so writing `t.0 .1` tight no longer turns it into a float.
#[test]
fn a_tuple_index_chain_is_written_tight() {
    assert_eq!(fmt("let y = t.0 .1\n"), "let y = t.0.1\n");
    same("let y = t.0.1\n");
}

/// An index or a call after a tuple's element is written against it, as
/// after any other name. `t.0[0]` came out `t.0 [0]`.
#[test]
fn an_index_after_a_tuple_element_is_tight() {
    same("let x = t.0[0]\n");
    same("let x = t.0.1[0][1]\n");
    same("let x = t.1(2)\n");
    assert_eq!(fmt("let x = t.0 [0]\n"), "let x = t.0[0]\n");
    assert_eq!(fmt("let x = t.0.1 [0] [1]\n"), "let x = t.0.1[0][1]\n");
    // A number that is not an element is not a name: nothing indexes `1`,
    // and a slice literal after one is on a line of its own anyway.
    same("let x = [1, 2]\n");
}

/// A declaration that does not parse is laid out by the fallback, and
/// nothing else is. One broken declaration at the end of a file — the state
/// format-on-save sees mid-edit — cost every comparison above it its spacing:
/// `io.print(a < b, b > a)` was written `io.print(a<b, b> a)`.
#[test]
fn a_broken_declaration_costs_only_itself_its_layout() {
    let good = "fn f(a: int, b: int) -> Option<int> {\n    io.print(a < b, b > a)\n\
                \x20   let e = g(a < b, b >= a)\n    let p = P{ x: 1 }\n    return nil\n}\n";
    same(good);
    let broken = format!("{}\nfn broken( {{\n}}\n", good);
    let out = fmt(&broken);
    assert!(out.starts_with(good), "{}", out);
    // And one broken in the middle, with good ones on either side. (Its
    // brackets balance: brackets left open indent what follows, which is
    // the formatter's one rule for indentation, broken file or not.)
    let after = good.replace("fn f(", "fn h(");
    let middle = format!("{}\nfn broken() {{\n    let = a < b\n}}\n\n{}", good, after);
    let out = fmt(&middle);
    assert!(out.ends_with(&after), "{}", out);
}

/// Wherever a rule would write two tokens with nothing between them, and they
/// would lex as something else, a space stays. Rules are written for code
/// that parses; this holds for every pair.
#[test]
fn no_two_tokens_are_glued_into_a_third() {
    // Two type closers written apart are not a shift.
    same("let x: Option<Option<int> > = nil\n");
    // A closure with a space between its pipes is not `||`.
    same("let f = | | 1\n");
    // Nonsense, but the formatter runs on nonsense, and this was `-=`.
    same("let x = - = 1\n");
}

/// A byte-order mark is kept: it is not the formatter's to take away.
#[test]
fn a_byte_order_mark_is_kept() {
    assert_eq!(idempotent("\u{feff}fn main() {\nio.print(1)\n}\n"), "\u{feff}fn main() {\n    io.print(1)\n}\n");
}

/// A struct literal that begins its line is written against its name whether
/// or not the file parses, and a condition split across lines still has its
/// block spaced. The fallback for a broken file used to space every literal
/// that began a line, so format-on-save flipped them each time a typo
/// elsewhere came and went.
#[test]
fn a_literal_starting_a_line_is_tight_in_a_broken_file_too() {
    // A continuation line is not indented further: indentation is bracket
    // depth, and nothing else.
    let good = "fn f() {\n    let xs = [\n        Item{ n: 1 },\n    ]\n    if a &&\n    b {\n    }\n}\n";
    same(good);
    let broken = format!("{}fn g( {{\n", good);
    assert!(fmt(&broken).starts_with(good), "{}", fmt(&broken));
}
