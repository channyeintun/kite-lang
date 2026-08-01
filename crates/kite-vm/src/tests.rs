//! End-to-end execution: Kite source in, program output out.
//!
//! These exercise every pass at once, which is what makes them the most
//! valuable tests in the tree. When the Wasm and native backends arrive, this
//! same corpus becomes the differential-testing oracle.

use super::*;
use kite_span::SourceMap;

/// Compile and run, returning captured output.
fn exec(src: &str) -> Result<String, Trap> {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();

    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let hir = kite_types::check(&ast, &resolved, src, &mut diags);
    assert!(
        !diags.has_errors(),
        "program does not compile:\n{}",
        diags.render_all(&sources)
    );

    let mir = kite_mir::lower(&hir);
    let chunk = kite_codegen_kbc::compile(&mir);

    let mut out = Vec::new();
    run(&chunk, &mut out)?;
    Ok(String::from_utf8(out).expect("output is valid UTF-8"))
}

/// Run, expecting success, and split the output into lines.
fn lines(src: &str) -> Vec<String> {
    exec(src)
        .unwrap_or_else(|t| panic!("unexpected trap: {}", t))
        .lines()
        .map(str::to_string)
        .collect()
}

/// Wrap statements in a `main`.
fn run_main(stmts: &str) -> Vec<String> {
    lines(&format!("fn main() {{\n{}\n}}\n", stmts))
}

// ---- the Phase 1 exit criterion -------------------------------------------

/// The exact program from docs/06-roadmap.md Phase 1.
#[test]
fn the_phase_one_program_runs() {
    let out = lines(
        "\
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
",
    );
    assert_eq!(out, vec!["big", "0", "1", "2", "3", "4"]);
}

// ---- values and printing --------------------------------------------------

#[test]
fn prints_each_primitive_type() {
    assert_eq!(
        run_main("  io.print(42)\n  io.print(1.5)\n  io.print(true)\n  io.print(\"hi\")"),
        vec!["42", "1.5", "true", "hi"]
    );
}

/// A float prints so it reads back as a float.
#[test]
fn whole_floats_keep_their_point() {
    assert_eq!(run_main("  io.print(2.0)\n  io.print(-0.5)"), vec!["2.0", "-0.5"]);
}

#[test]
fn string_escapes_reach_the_output() {
    assert_eq!(run_main("  io.print(\"a\\tb\")"), vec!["a\tb"]);
    assert_eq!(run_main("  io.print(\"x\\ny\")"), vec!["x", "y"]);
}

// ---- arithmetic -----------------------------------------------------------

#[test]
fn integer_arithmetic() {
    assert_eq!(
        run_main(
            "  io.print(2 + 3)\n  io.print(10 - 4)\n  io.print(6 * 7)\n  io.print(20 / 3)\n  io.print(20 % 3)\n  io.print(-5)"
        ),
        vec!["5", "6", "42", "6", "2", "-5"]
    );
}

#[test]
fn float_arithmetic() {
    assert_eq!(
        run_main("  io.print(1.5 + 2.5)\n  io.print(3.0 * 2.0)\n  io.print(1.0 / 4.0)"),
        vec!["4.0", "6.0", "0.25"]
    );
}

#[test]
fn precedence_is_respected_at_runtime() {
    assert_eq!(run_main("  io.print(1 + 2 * 3)"), vec!["7"]);
    assert_eq!(run_main("  io.print((1 + 2) * 3)"), vec!["9"]);
}

/// The documented departure from C, verified end to end. `6 & 3 == 2` groups as
/// `(6 & 3) == 2`, which is true. In C it would be `6 & (3 == 2)`, which is 0.
#[test]
fn bitwise_binds_tighter_than_comparison_at_runtime() {
    assert_eq!(run_main("  io.print(6 & 3 == 2)"), vec!["true"]);
}

#[test]
fn bitwise_and_shift_operators() {
    assert_eq!(
        run_main(
            "  io.print(12 & 10)\n  io.print(12 | 10)\n  io.print(12 ^ 10)\n  io.print(1 << 4)\n  io.print(256 >> 4)"
        ),
        vec!["8", "14", "6", "16", "16"]
    );
}

#[test]
fn string_concatenation() {
    assert_eq!(run_main("  io.print(\"foo\" + \"bar\")"), vec!["foobar"]);
}

// ---- comparison and logic -------------------------------------------------

#[test]
fn comparisons_on_each_type() {
    assert_eq!(
        run_main(
            "  io.print(1 < 2)\n  io.print(2.0 >= 3.0)\n  io.print(\"a\" == \"a\")\n  io.print(true != false)"
        ),
        vec!["true", "false", "true", "true"]
    );
}

#[test]
fn logical_operators() {
    assert_eq!(
        run_main("  io.print(true && false)\n  io.print(true || false)\n  io.print(!true)"),
        vec!["false", "true", "false"]
    );
}

/// `&&` must not evaluate its right side when the left is false. `boom()`
/// divides by zero, so if it ran the program would trap.
#[test]
fn and_short_circuits() {
    let out = lines(
        "\
fn boom() -> bool {
    let a = 1
    let b = 0
    return a / b > 0
}
fn main() {
    io.print(false && boom())
}
",
    );
    assert_eq!(out, vec!["false"]);
}

#[test]
fn or_short_circuits() {
    let out = lines(
        "\
fn boom() -> bool {
    let a = 1
    let b = 0
    return a / b > 0
}
fn main() {
    io.print(true || boom())
}
",
    );
    assert_eq!(out, vec!["true"]);
}

// ---- control flow ---------------------------------------------------------

#[test]
fn if_else_chains_pick_one_branch() {
    let src = "\
fn classify(n: int) -> str {
    if n > 10 {
        return \"big\"
    } else if n > 5 {
        return \"medium\"
    } else {
        return \"small\"
    }
}
fn main() {
    io.print(classify(20))
    io.print(classify(7))
    io.print(classify(1))
}
";
    assert_eq!(lines(src), vec!["big", "medium", "small"]);
}

#[test]
fn if_as_a_value() {
    assert_eq!(
        run_main("  let label = if 12 > 10 { \"big\" } else { \"small\" }\n  io.print(label)"),
        vec!["big"]
    );
}

#[test]
fn exclusive_and_inclusive_ranges() {
    assert_eq!(run_main("  for i in 0..3 {\n    io.print(i)\n  }"), vec!["0", "1", "2"]);
    assert_eq!(
        run_main("  for i in 0..=3 {\n    io.print(i)\n  }"),
        vec!["0", "1", "2", "3"]
    );
}

#[test]
fn an_empty_range_runs_zero_times() {
    assert!(run_main("  for i in 5..5 {\n    io.print(i)\n  }").is_empty());
    assert!(run_main("  for i in 5..0 {\n    io.print(i)\n  }").is_empty());
}

#[test]
fn conditional_loop() {
    assert_eq!(
        run_main("  var n = 0\n  for n < 3 {\n    io.print(n)\n    n += 1\n  }"),
        vec!["0", "1", "2"]
    );
}

#[test]
fn unconditional_loop_with_break() {
    assert_eq!(
        run_main(
            "  var n = 0\n  for {\n    if n == 3 {\n      break\n    }\n    io.print(n)\n    n += 1\n  }"
        ),
        vec!["0", "1", "2"]
    );
}

/// The behaviour that justifies keeping `for` intact through HIR. If `continue`
/// jumped to the loop header instead of the increment, this would not
/// terminate.
#[test]
fn continue_in_a_range_loop_still_advances() {
    assert_eq!(
        run_main("  for i in 0..5 {\n    if i == 2 {\n      continue\n    }\n    io.print(i)\n  }"),
        vec!["0", "1", "3", "4"]
    );
}

#[test]
fn continue_in_a_conditional_loop_still_advances() {
    assert_eq!(
        run_main(
            "  var n = 0\n  for n < 5 {\n    n += 1\n    if n == 2 {\n      continue\n    }\n    io.print(n)\n  }"
        ),
        vec!["1", "3", "4", "5"]
    );
}

#[test]
fn break_leaves_only_the_innermost_loop() {
    assert_eq!(
        run_main(
            "  for i in 0..2 {\n    for j in 0..5 {\n      if j == 1 {\n        break\n      }\n      io.print(j)\n    }\n    io.print(i)\n  }"
        ),
        vec!["0", "0", "0", "1"]
    );
}

#[test]
fn a_labelled_continue_advances_the_outer_loop() {
    assert_eq!(
        run_main(
            "  outer: for i in 0..3 {\n    for j in 0..3 {\n      if j == 1 {\n        continue outer\n      }\n      io.print(i * 10 + j)\n    }\n  }"
        ),
        vec!["0", "10", "20"]
    );
}

#[test]
fn a_labelled_break_leaves_the_outer_loop() {
    assert_eq!(
        run_main(
            "  outer: for i in 0..3 {\n    for j in 0..3 {\n      if i == 1 {\n        break outer\n      }\n      io.print(i * 10 + j)\n    }\n  }"
        ),
        vec!["0", "1", "2"]
    );
}

#[test]
fn nested_loops_run_the_full_product() {
    assert_eq!(
        run_main("  for i in 0..3 {\n    for j in 0..4 {\n      io.print(1)\n    }\n  }").len(),
        12
    );
}

/// The bound is evaluated once. If it were re-evaluated each iteration this
/// would print `bound` repeatedly.
#[test]
fn the_range_bound_is_evaluated_once() {
    let out = lines(
        "\
fn bound() -> int {
    io.print(\"bound\")
    return 3
}
fn main() {
    for i in 0..bound() {
        io.print(i)
    }
}
",
    );
    assert_eq!(out, vec!["bound", "0", "1", "2"]);
}

// ---- functions ------------------------------------------------------------

#[test]
fn functions_return_values() {
    assert_eq!(
        lines(
            "fn square(n: int) -> int {\n  return n * n\n}\nfn main() {\n  io.print(square(7))\n}\n"
        ),
        vec!["49"]
    );
}

#[test]
fn a_call_may_precede_its_declaration() {
    assert_eq!(
        lines("fn main() {\n  io.print(later())\n}\nfn later() -> int {\n  return 9\n}\n"),
        vec!["9"]
    );
}

#[test]
fn recursion_works() {
    let src = "\
fn fact(n: int) -> int {
    if n <= 1 {
        return 1
    }
    return n * fact(n - 1)
}
fn main() {
    io.print(fact(10))
}
";
    assert_eq!(lines(src), vec!["3628800"]);
}

#[test]
fn mutual_recursion_works() {
    let src = "\
fn is_even(n: int) -> bool {
    if n == 0 {
        return true
    }
    return is_odd(n - 1)
}
fn is_odd(n: int) -> bool {
    if n == 0 {
        return false
    }
    return is_even(n - 1)
}
fn main() {
    io.print(is_even(10))
    io.print(is_odd(7))
}
";
    assert_eq!(lines(src), vec!["true", "true"]);
}

#[test]
fn arguments_are_passed_positionally() {
    let src = "\
fn sub(a: int, b: int) -> int {
    return a - b
}
fn main() {
    io.print(sub(10, 3))
    io.print(sub(3, 10))
}
";
    assert_eq!(lines(src), vec!["7", "-7"]);
}

/// A nested call must not clobber the outer call's argument window.
#[test]
fn nested_calls_do_not_clobber_the_argument_window() {
    let src = "\
fn add(a: int, b: int) -> int {
    return a + b
}
fn main() {
    io.print(add(add(1, 2), add(3, 4)))
}
";
    assert_eq!(lines(src), vec!["10"]);
}

#[test]
fn a_function_may_have_side_effects_and_no_return() {
    let src = "\
fn shout(s: str) {
    io.print(s + \"!\")
}
fn main() {
    shout(\"hey\")
    shout(\"ho\")
}
";
    assert_eq!(lines(src), vec!["hey!", "ho!"]);
}

#[test]
fn parameters_are_local_to_the_call() {
    let src = "\
fn twice(n: int) -> int {
    return n + n
}
fn main() {
    let n = 5
    io.print(twice(3))
    io.print(n)
}
";
    assert_eq!(lines(src), vec!["6", "5"]);
}

// ---- bindings -------------------------------------------------------------

#[test]
fn var_bindings_update() {
    assert_eq!(
        run_main("  var n = 1\n  n = 2\n  n += 3\n  n *= 2\n  io.print(n)"),
        vec!["10"]
    );
}

#[test]
fn nested_scopes_shadow_without_disturbing_the_outer_binding() {
    assert_eq!(
        run_main("  let x = 1\n  if true {\n    let x = 2\n    io.print(x)\n  }\n  io.print(x)"),
        vec!["2", "1"]
    );
}

#[test]
fn deferred_initialisation_assigns_on_the_taken_branch() {
    assert_eq!(
        run_main("  let z: int\n  if true {\n    z = 1\n  } else {\n    z = 2\n  }\n  io.print(z)"),
        vec!["1"]
    );
}

// ---- traps ----------------------------------------------------------------

/// Division by zero is a bug, not a runtime condition, so it traps rather than
/// producing a value. There is no `recover`.
#[test]
fn integer_division_by_zero_traps() {
    assert_eq!(
        exec("fn main() {\n  let a = 1\n  let b = 0\n  io.print(a / b)\n}\n"),
        Err(Trap::DivideByZero)
    );
}

#[test]
fn integer_remainder_by_zero_traps() {
    assert_eq!(
        exec("fn main() {\n  let a = 1\n  let b = 0\n  io.print(a % b)\n}\n"),
        Err(Trap::DivideByZero)
    );
}

/// IEEE-754 division by zero is defined, so it does not trap.
#[test]
fn float_division_by_zero_yields_infinity() {
    assert_eq!(run_main("  let a = 1.0\n  let b = 0.0\n  io.print(a / b)"), vec!["inf"]);
}

#[test]
fn integer_overflow_traps() {
    let src = "\
fn main() {
    var n = 9223372036854775807
    n += 1
    io.print(n)
}
";
    assert_eq!(exec(src), Err(Trap::IntegerOverflow("+")));
}

#[test]
fn runaway_recursion_traps_instead_of_crashing_the_host() {
    let src = "\
fn forever(n: int) -> int {
    return forever(n + 1)
}
fn main() {
    io.print(forever(0))
}
";
    assert_eq!(exec(src), Err(Trap::CallDepthExceeded));
}

// ---- evaluation order -----------------------------------------------------

#[test]
fn output_order_follows_evaluation_order() {
    let src = "\
fn step(n: int) -> int {
    io.print(n)
    return n
}
fn main() {
    let x = step(1) + step(2)
    io.print(x)
}
";
    assert_eq!(lines(src), vec!["1", "2", "3"]);
}

// ---- structs --------------------------------------------------------------

const RECT: &str = "\
struct Rect {
    width: int
    height: int
    var label: str
}

impl Rect {
    fn area(self) -> int {
        return self.width * self.height
    }

    fn scaled(self, factor: int) -> Rect {
        return Rect{ ..self, width: self.width * factor }
    }

    fn rename(var self, name: str) {
        self.label = name
    }

    fn square(side: int) -> Rect {
        return Rect{ width: side, height: side, label: \"square\" }
    }
}
";

fn with_rect(body: &str) -> Vec<String> {
    lines(&format!("{}\nfn main() {{\n{}\n}}\n", RECT, body))
}

#[test]
fn a_struct_literal_and_field_read() {
    assert_eq!(
        with_rect("  let r = Rect{ width: 3, height: 4, label: \"first\" }\n  io.print(r.width)\n  io.print(r.label)"),
        vec!["3", "first"]
    );
}

#[test]
fn a_method_reads_through_self() {
    assert_eq!(
        with_rect("  let r = Rect{ width: 3, height: 4, label: \"x\" }\n  io.print(r.area())"),
        vec!["12"]
    );
}

#[test]
fn an_associated_function_is_called_through_the_type() {
    assert_eq!(with_rect("  io.print(Rect.square(5).area())"), vec!["25"]);
}

/// `..base` produces a new value and leaves the original alone.
#[test]
fn functional_update_copies_the_untouched_fields() {
    assert_eq!(
        with_rect(
            "  let r = Rect{ width: 3, height: 4, label: \"first\" }\n\
             \x20 let big = r.scaled(10)\n  io.print(big.width)\n  io.print(big.height)\n\
             \x20 io.print(big.label)\n  io.print(r.width)"
        ),
        vec!["30", "4", "first", "3"]
    );
}

/// Structs are references: a method taking `var self` mutates the value the
/// caller is holding, not a copy. This is the whole reason Kite has no
/// value-versus-pointer receiver distinction.
#[test]
fn mutation_through_a_reference_is_visible_to_the_caller() {
    assert_eq!(
        with_rect(
            "  let r = Rect{ width: 1, height: 1, label: \"before\" }\n\
             \x20 r.rename(\"after\")\n  io.print(r.label)"
        ),
        vec!["after"]
    );
}

#[test]
fn assignment_copies_the_reference_not_the_contents() {
    assert_eq!(
        with_rect(
            "  let a = Rect{ width: 1, height: 1, label: \"one\" }\n\
             \x20 let b = a\n  b.rename(\"two\")\n  io.print(a.label)"
        ),
        vec!["two"]
    );
}

#[test]
fn a_var_field_can_be_assigned_directly() {
    assert_eq!(
        with_rect(
            "  let r = Rect{ width: 1, height: 1, label: \"one\" }\n\
             \x20 r.label = \"two\"\n  io.print(r.label)"
        ),
        vec!["two"]
    );
}

#[test]
fn structs_nest() {
    let src = "\
struct Inner {
    n: int
}
struct Outer {
    inner: Inner
    tag: str
}
fn main() {
    let o = Outer{ inner: Inner{ n: 42 }, tag: \"t\" }
    io.print(o.inner.n)
    io.print(o.tag)
}
";
    assert_eq!(lines(src), vec!["42", "t"]);
}

#[test]
fn a_struct_may_be_passed_to_and_returned_from_a_function() {
    let src = "\
struct P {
    x: int
}
fn bump(p: P) -> P {
    return P{ x: p.x + 1 }
}
fn main() {
    let a = P{ x: 1 }
    io.print(bump(bump(a)).x)
    io.print(a.x)
}
";
    assert_eq!(lines(src), vec!["3", "1"]);
}

/// Two structs are equal when their fields are, per the specification.
#[test]
fn struct_equality_is_structural() {
    let src = "\
struct P {
    x: int
    y: int
}
fn main() {
    let a = P{ x: 1, y: 2 }
    let b = P{ x: 1, y: 2 }
    let c = P{ x: 9, y: 2 }
    io.print(a == b)
    io.print(a == c)
}
";
    assert_eq!(lines(src), vec!["true", "false"]);
}

/// A recursive struct needs no boxing annotation, because every Kite aggregate
/// is already a GC reference. The self-reference is only *declared* here;
/// building a chain needs optionals, which arrive later in Phase 2.
#[test]
fn a_recursive_struct_declaration_is_accepted() {
    let src = "\
struct Node {
    value: int
    children: [Node]
}
fn describe(n: Node) -> int {
    return n.value
}
fn main() {
    io.print(1)
}
";
    assert_eq!(lines(src), vec!["1"]);
}

// ---- enums and match ------------------------------------------------------

const SHAPE: &str = "\
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}
";

fn with_shape(body: &str) -> Vec<String> {
    lines(&format!("{}\nfn main() {{\n{}\n}}\n", SHAPE, body))
}

#[test]
fn a_unit_variant_round_trips() {
    assert_eq!(
        with_shape("  let p = Point\n  io.print(match p {\n    Point => \"point\",\n    _ => \"other\",\n  })"),
        vec!["point"]
    );
}

#[test]
fn a_named_payload_is_constructed_and_destructured() {
    assert_eq!(
        with_shape("  let c = Circle(radius: 7)\n  io.print(match c {\n    Circle(r) => r,\n    _ => 0,\n  })"),
        vec!["7"]
    );
}

#[test]
fn named_arguments_may_be_written_out_of_order() {
    assert_eq!(
        with_shape("  let r = Rect(height: 4, width: 3)\n  io.print(match r {\n    Rect(w, h) => w * 10 + h,\n    _ => 0,\n  })"),
        vec!["34"]
    );
}

#[test]
fn named_patterns_bind_by_field_name() {
    assert_eq!(
        with_shape("  let r = Rect(width: 3, height: 4)\n  io.print(match r {\n    Rect(height: h, width: w) => w * 10 + h,\n    _ => 0,\n  })"),
        vec!["34"]
    );
}

#[test]
fn arms_are_tried_in_order_and_guards_can_fail_through() {
    let src = format!(
        "{}\nfn describe(s: Shape) -> str {{\n    return match s {{\n        Circle(r) => \"circle\",\n        Rect(w, h) if w == h => \"square\",\n        Rect(w, h) => \"rect\",\n        Point => \"point\",\n    }}\n}}\nfn main() {{\n    io.print(describe(Circle(radius: 1)))\n    io.print(describe(Rect(width: 2, height: 2)))\n    io.print(describe(Rect(width: 2, height: 3)))\n    io.print(describe(Point))\n}}\n",
        SHAPE
    );
    assert_eq!(lines(&src), vec!["circle", "square", "rect", "point"]);
}

#[test]
fn literal_alternation_and_range_patterns() {
    let src = "\
fn classify(n: int) -> str {
    return match n {
        0 => \"zero\",
        1 | 2 | 3 => \"small\",
        4..=9 => \"medium\",
        _ => \"large\",
    }
}
fn main() {
    io.print(classify(0))
    io.print(classify(2))
    io.print(classify(9))
    io.print(classify(10))
}
";
    assert_eq!(lines(src), vec!["zero", "small", "medium", "large"]);
}

#[test]
fn an_exclusive_range_pattern_excludes_its_end() {
    let src = "\
fn f(n: int) -> str {
    return match n {
        0..3 => \"in\",
        _ => \"out\",
    }
}
fn main() {
    io.print(f(2))
    io.print(f(3))
}
";
    assert_eq!(lines(src), vec!["in", "out"]);
}

#[test]
fn a_negative_literal_pattern_matches() {
    let src = "\
fn f(n: int) -> str {
    return match n {
        -1 => \"minus one\",
        _ => \"other\",
    }
}
fn main() {
    io.print(f(-1))
    io.print(f(1))
}
";
    assert_eq!(lines(src), vec!["minus one", "other"]);
}

#[test]
fn match_works_as_a_statement_for_its_effects() {
    assert_eq!(
        with_shape("  match Point {\n    Point => {\n      io.print(\"unit\")\n    }\n    _ => {\n      io.print(\"other\")\n    }\n  }"),
        vec!["unit"]
    );
}

#[test]
fn a_binding_pattern_captures_the_whole_value() {
    let src = "\
fn f(n: int) -> int {
    return match n {
        0 => 100,
        other => other * 2,
    }
}
fn main() {
    io.print(f(0))
    io.print(f(21))
}
";
    assert_eq!(lines(src), vec!["100", "42"]);
}

#[test]
fn a_struct_pattern_tests_and_binds_fields() {
    let src = "\
struct P {
    x: int
    y: int
}
fn f(p: P) -> str {
    return match p {
        P{ x: 0, y: 0 } => \"origin\",
        P{ x: 0, y } => \"on y\",
        P{ x, y } => \"elsewhere\",
    }
}
fn main() {
    io.print(f(P{ x: 0, y: 0 }))
    io.print(f(P{ x: 0, y: 5 }))
    io.print(f(P{ x: 1, y: 5 }))
}
";
    assert_eq!(lines(src), vec!["origin", "on y", "elsewhere"]);
}

#[test]
fn enum_equality_is_structural() {
    assert_eq!(
        with_shape(
            "  io.print(Circle(radius: 1) == Circle(radius: 1))\n\
             \x20 io.print(Circle(radius: 1) == Circle(radius: 2))\n\
             \x20 io.print(Circle(radius: 1) == Point)"
        ),
        vec!["true", "false", "false"]
    );
}

#[test]
fn a_recursive_enum_needs_no_boxing_annotation() {
    let src = "\
enum Tree {
    Leaf(int)
    Node(left: Tree, right: Tree)
}
fn total(t: Tree) -> int {
    return match t {
        Leaf(n) => n,
        Node(l, r) => total(l) + total(r),
    }
}
fn main() {
    let t = Node(left: Node(left: Leaf(1), right: Leaf(2)), right: Leaf(3))
    io.print(total(t))
}
";
    assert_eq!(lines(src), vec!["6"]);
}

// ---- traits ---------------------------------------------------------------

/// The `Shape` example from SPECIFICATION.md section 10, which is the Phase 2
/// exit criterion.
#[test]
fn the_specification_trait_example_runs() {
    let src = "\
struct Rect {
    width: int
    height: int
}
struct Circle {
    radius: int
}

pub trait Shape {
    fn area(self) -> int

    fn describe(self) -> str {
        return \"a shape\"
    }
}

impl Shape for Rect {
    fn area(self) -> int {
        return self.width * self.height
    }
    fn describe(self) -> str {
        return \"a rectangle\"
    }
}

impl Shape for Circle {
    fn area(self) -> int {
        return 3 * self.radius * self.radius
    }
}

fn main() {
    let r = Rect{ width: 3, height: 4 }
    let c = Circle{ radius: 2 }
    io.print(r.area())
    io.print(r.describe())
    io.print(c.area())
    io.print(c.describe())
}
";
    assert_eq!(lines(src), vec!["12", "a rectangle", "12", "a shape"]);
}

/// A default method's body lives in the trait but runs against the
/// implementing type's `self`.
#[test]
fn a_default_method_sees_the_implementing_types_fields() {
    let src = "\
struct P {
    n: int
}
trait Doubler {
    fn value(self) -> int
    fn doubled(self) -> int {
        return self.value() * 2
    }
}
impl Doubler for P {
    fn value(self) -> int {
        return self.n
    }
}
fn main() {
    io.print(P{ n: 21 }.doubled())
}
";
    assert_eq!(lines(src), vec!["42"]);
}

#[test]
fn one_type_may_implement_several_traits() {
    let src = "\
struct P {
    n: int
}
trait A {
    fn a(self) -> int
}
trait B {
    fn b(self) -> int
}
impl A for P {
    fn a(self) -> int {
        return self.n
    }
}
impl B for P {
    fn b(self) -> int {
        return self.n * 2
    }
}
fn main() {
    let p = P{ n: 5 }
    io.print(p.a())
    io.print(p.b())
}
";
    assert_eq!(lines(src), vec!["5", "10"]);
}

#[test]
fn inherent_and_trait_methods_coexist() {
    let src = "\
struct P {
    n: int
}
trait T {
    fn viaTrait(self) -> int
}
impl P {
    fn inherent(self) -> int {
        return self.n + 1
    }
}
impl T for P {
    fn viaTrait(self) -> int {
        return self.n + 2
    }
}
fn main() {
    let p = P{ n: 1 }
    io.print(p.inherent())
    io.print(p.viaTrait())
}
";
    assert_eq!(lines(src), vec!["2", "3"]);
}

// ---- slices ---------------------------------------------------------------

#[test]
fn slice_literals_index_and_length() {
    assert_eq!(
        run_main("  let xs = [10, 20, 30]\n  io.print(xs.len())\n  io.print(xs[0])\n  io.print(xs[2])"),
        vec!["3", "10", "30"]
    );
}

/// An out-of-range index is a program bug, so it traps. `.get()` is the form
/// for when it genuinely is a runtime condition.
#[test]
fn an_out_of_range_index_traps() {
    assert_eq!(
        exec("fn main() {\n  let xs = [1, 2]\n  io.print(xs[5])\n}\n"),
        Err(Trap::IndexOutOfRange { index: 5, len: 2 })
    );
}

#[test]
fn get_yields_an_optional_instead_of_trapping() {
    assert_eq!(
        run_main("  let xs = [10, 20]\n  io.print(xs.get(1) ?? -1)\n  io.print(xs.get(9) ?? -1)"),
        vec!["20", "-1"]
    );
}

#[test]
fn iterating_a_slice_visits_every_element() {
    assert_eq!(
        run_main("  for x in [1, 2, 3] {\n    io.print(x)\n  }"),
        vec!["1", "2", "3"]
    );
}

#[test]
fn an_empty_slice_iterates_zero_times() {
    assert!(run_main("  let xs: [int] = []\n  for x in xs {\n    io.print(x)\n  }").is_empty());
}

#[test]
fn push_and_index_assignment_mutate_the_binding() {
    assert_eq!(
        run_main("  var xs = [1, 2]\n  xs.push(3)\n  xs[0] = 99\n  io.print(xs.len())\n  io.print(xs[0])\n  io.print(xs[2])"),
        vec!["3", "99", "3"]
    );
}

/// Slices are copy-on-write *values*: assigning one and mutating the copy
/// leaves the original alone. This is what keeps `[T]` `Share` when `T` is.
#[test]
fn slices_have_value_semantics() {
    assert_eq!(
        run_main(
            "  var a = [1, 2]\n  var b = a\n  b.push(3)\n  b[0] = 9\n\
             \x20 io.print(a.len())\n  io.print(a[0])\n  io.print(b.len())\n  io.print(b[0])"
        ),
        vec!["2", "1", "3", "9"]
    );
}

#[test]
fn a_slice_passed_to_a_function_is_not_aliased() {
    let src = "\
fn grow(xs: [int]) -> int {
    var local = xs
    local.push(99)
    return local.len()
}
fn main() {
    let xs = [1, 2]
    io.print(grow(xs))
    io.print(xs.len())
}
";
    assert_eq!(lines(src), vec!["3", "2"]);
}

#[test]
fn slices_of_structs_work() {
    let src = "\
struct P {
    n: int
}
fn main() {
    let ps = [P{ n: 1 }, P{ n: 2 }]
    var total = 0
    for p in ps {
        total = total + p.n
    }
    io.print(total)
}
";
    assert_eq!(lines(src), vec!["3"]);
}

#[test]
fn slice_equality_is_structural() {
    assert_eq!(
        run_main("  io.print([1, 2] == [1, 2])\n  io.print([1, 2] == [1, 3])"),
        vec!["true", "false"]
    );
}

// ---- optionals ------------------------------------------------------------

const FINDER: &str = "\
struct User {
    name: str
}
fn find(id: int) -> ?User {
    if id == 1 {
        return User{ name: \"ada\" }
    }
    return nil
}
";

#[test]
fn an_optional_may_be_present_or_nil() {
    let src = format!(
        "{}\nfn main() {{\n  io.print(match find(1) {{\n    nil => \"missing\",\n    u => u.name,\n  }})\n  io.print(match find(2) {{\n    nil => \"missing\",\n    u => u.name,\n  }})\n}}\n",
        FINDER
    );
    assert_eq!(lines(&src), vec!["ada", "missing"]);
}

/// `a?.b` yields nil when `a` is nil, and `a.b` otherwise.
#[test]
fn optional_chaining_short_circuits_on_nil() {
    let src = format!(
        "{}\nfn main() {{\n  io.print(find(1)?.name ?? \"anonymous\")\n  io.print(find(2)?.name ?? \"anonymous\")\n}}\n",
        FINDER
    );
    assert_eq!(lines(&src), vec!["ada", "anonymous"]);
}

/// `??` must not evaluate its right side when the left is present.
#[test]
fn coalesce_short_circuits() {
    let src = "\
fn boom() -> int {
    let a = 1
    let b = 0
    return a / b
}
fn main() {
    let xs = [7]
    io.print(xs.get(0) ?? boom())
}
";
    assert_eq!(lines(src), vec!["7"]);
}

#[test]
fn a_value_widens_into_an_optional_binding() {
    assert_eq!(
        run_main("  let a: ?int = 5\n  let b: ?int = nil\n  io.print(a ?? 0)\n  io.print(b ?? 0)"),
        vec!["5", "0"]
    );
}
