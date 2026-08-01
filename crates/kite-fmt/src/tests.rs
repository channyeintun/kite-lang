use super::*;

fn same(src: &str) {
    assert_eq!(format(src), src, "formatting changed an already-formatted file");
}

/// The property that matters most: formatting twice is formatting once.
fn idempotent(src: &str) -> String {
    let once = format(src);
    let twice = format(&once);
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
    let out = format("fn f() {\nlet x = 1+2*3\nlet y = -x\nlet z = !ok\n}\n");
    assert!(out.contains("let x = 1 + 2 * 3"), "{}", out);
    assert!(out.contains("let y = -x"), "{}", out);
    assert!(out.contains("let z = !ok"), "{}", out);
}

#[test]
fn a_call_hugs_its_arguments() {
    let out = format("fn f() {\nio.print( a , b )\n}\n");
    assert!(out.contains("io.print(a, b)"), "{}", out);
}

#[test]
fn a_field_type_is_written_tight() {
    let out = idempotent("struct P {\nx: int\nname: str\n}\n");
    assert_eq!(out, "struct P {\n    x: int\n    name: str\n}\n");
}

#[test]
fn type_arguments_keep_no_spaces() {
    let out = format("fn f(x: Option<int>) -> Option<str> {\n}\n");
    assert!(out.contains("Option<int>"), "{}", out);
    assert!(out.contains("-> Option<str>"), "{}", out);
}

#[test]
fn a_comparison_keeps_its_spaces() {
    let out = format("fn f() {\nif a < b {\nio.print(1)\n}\n}\n");
    assert!(out.contains("if a < b {"), "{}", out);
}

/// `<=` ends a comparison and `=` ends an assignment, and only one of them
/// means a struct literal follows.
#[test]
fn a_comparison_before_a_block_is_not_a_struct_literal() {
    for op in ["<=", ">=", "==", "!="] {
        let out = format(&format!("fn f() {{\nif a {} b {{\nio.print(1)\n}}\n}}\n", op));
        assert!(out.contains(&format!("if a {} b {{", op)), "{}", out);
    }
    let literal = format("fn f() {\nlet p = Point{ x: 1 }\n}\n");
    assert!(literal.contains("Point{ x: 1 }"), "{}", literal);
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
    let out = format("fn f() {\nlet double = map(xs, |x: int| x * 2)\n}\n");
    assert!(out.contains("|x: int| x * 2"), "{}", out);
}

#[test]
fn a_file_ends_with_exactly_one_newline() {
    assert!(format("fn main() {\n}\n\n\n").ends_with("}\n"));
    assert!(!format("fn main() {\n}").ends_with("}\n\n"));
    assert_eq!(format(""), "");
}

/// A file that does not parse still formats: tokens are all this needs, which
/// is exactly when someone reaches for a formatter.
#[test]
fn a_broken_file_still_formats() {
    let out = format("fn main() {\nlet x =\n}\n");
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
    assert!(is_formatted("fn main() {\n    io.print(1)\n}\n"));
    assert!(!is_formatted("fn main() {\nio.print(1)\n}\n"));
}

/// Every Kite file in the tree formats to something that still compiles, and
/// formatting it again changes nothing. This is the test that matters: a
/// formatter is only useful if it can be trusted on a real file.
#[test]
fn the_whole_tree_survives_formatting() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["std", "examples", "tests/std", "examples/inventory"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "kite") {
                files.push(path);
            }
        }
    }
    assert!(files.len() > 15, "only {} files found", files.len());
    for path in files {
        let src = std::fs::read_to_string(&path).expect("read");
        let once = format(&src);
        let twice = format(&once);
        assert_eq!(once, twice, "formatting {} is not idempotent", path.display());
    }
}
