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
    let mut hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(
        !diags.has_errors(),
        "program does not compile:\n{}",
        diags.render_all(&sources)
    );

    kite_hir::mono::monomorphise(&mut hir).expect("specialisation terminates");
    let mir = kite_mir::lower(&hir);
    let chunk = kite_codegen_kbc::compile(&mir);

    let mut out = Vec::new();
    run(&chunk, &mut out)?;
    Ok(String::from_utf8(out).expect("output is valid UTF-8"))
}

/// The same, built for release — which changes what integer overflow does.
fn exec_release(src: &str) -> Result<String, Trap> {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();

    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check_with(&ast, &resolved, &sources, &mut diags, true);
    assert!(
        !diags.has_errors(),
        "program does not compile:\n{}",
        diags.render_all(&sources)
    );

    kite_hir::mono::monomorphise(&mut hir).expect("specialisation terminates");
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

// ---- defer ----------------------------------------------------------------

/// The operands are evaluated when the `defer` is reached, not when it runs,
/// so a later assignment is not visible to it.
#[test]
fn a_deferred_call_uses_the_values_it_was_registered_with() {
    assert_eq!(
        lines(
            "fn main() {\n  var name = \"first\"\n  defer io.print(name)\n\
             \x20 name = \"second\"\n  io.print(\"body\")\n}\n"
        ),
        vec!["body", "first"]
    );
}

/// A `defer` inside a branch that never runs must not run either — and must
/// not read a guard that was never written.
#[test]
fn an_unreached_defer_does_not_run() {
    assert_eq!(
        lines(
            "fn f(run: bool) {\n  if run {\n    defer io.print(\"done\")\n  }\n}\n\
             fn main() {\n  f(false)\n  io.print(\"survived\")\n}\n"
        ),
        vec!["survived"]
    );
}

#[test]
fn a_reached_defer_still_runs() {
    assert_eq!(
        lines(
            "fn f(run: bool) {\n  if run {\n    defer io.print(\"done\")\n  }\n}\n\
             fn main() {\n  f(true)\n  io.print(\"after\")\n}\n"
        ),
        vec!["done", "after"]
    );
}

/// The returned value is evaluated before the deferred stack runs, so a
/// deferred call cannot change what the caller receives.
#[test]
fn the_return_value_is_evaluated_before_deferred_calls() {
    assert_eq!(
        lines(
            "fn note() {\n  io.print(\"deferred\")\n}\n\
             fn g() -> int {\n  defer note()\n  return 1\n}\n\
             fn main() {\n  io.print(g())\n}\n"
        ),
        vec!["deferred", "1"]
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

/// Section 3.1: overflow traps in a debug build and wraps in a release one.
/// The choice is made in the checker, so every backend gets it from the same
/// operation rather than deciding for itself.
#[test]
fn integer_overflow_wraps_in_a_release_build() {
    let src = "\
fn main() {
    var n = 9223372036854775807
    n += 1
    io.print(n)
}
";
    assert_eq!(exec_release(src).unwrap().trim(), "-9223372036854775808");
}

#[test]
fn multiplication_overflow_wraps_in_a_release_build() {
    let src = "\
fn main() {
    var n = 4611686018427387904
    n = n * 2
    io.print(n)
}
";
    assert_eq!(exec_release(src).unwrap().trim(), "-9223372036854775808");
}

/// Negation overflows for one input, and follows the same rule: a trap in a
/// debug build, a wrap in a release one. There was no release form, so a
/// release build trapped on the VM and natively and wrapped on Wasm.
#[test]
fn negating_the_minimum_traps_in_debug_and_wraps_in_release() {
    let src = "\
fn id(x: int) -> int {
    return x
}
fn main() {
    io.print(-id(-9223372036854775807 - 1))
}
";
    assert_eq!(exec(src), Err(Trap::IntegerOverflow("-")));
    assert_eq!(exec_release(src).unwrap().trim(), "-9223372036854775808");
}

/// A shift count outside `0..=63` traps in a debug build and is taken modulo
/// 64 in a release one. The VM trapped in both, Wasm masked in both.
#[test]
fn an_out_of_range_shift_traps_in_debug_and_masks_in_release() {
    let src = "\
fn id(x: int) -> int {
    return x
}
fn main() {
    io.print(id(1) << id(65))
    io.print(id(-16) >> id(-63))
}
";
    assert_eq!(exec(src), Err(Trap::IntegerOverflow("<<")));
    assert_eq!(exec_release(src).unwrap(), "2\n-8\n");
}

/// `min % -1` is 0, which fits, so unlike `min / -1` it is no overflow. The
/// VM and the native backend trapped on it and Wasm answered 0.
#[test]
fn the_remainder_of_the_minimum_by_minus_one_is_zero() {
    let src = "\
fn id(x: int) -> int {
    return x
}
fn main() {
    io.print(id(-9223372036854775807 - 1) % id(-1))
    io.print(-9223372036854775808)
}
";
    assert_eq!(exec(src).unwrap(), "0\n-9223372036854775808\n");
    assert_eq!(exec(&src.replace('%', "/")), Err(Trap::IntegerOverflow("/")));
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
            "  var r = Rect{ width: 1, height: 1, label: \"before\" }\n\
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
             \x20 var b = a\n  b.rename(\"two\")\n  io.print(a.label)"
        ),
        vec!["two"]
    );
}

#[test]
fn a_var_field_can_be_assigned_directly() {
    assert_eq!(
        with_rect(
            "  var r = Rect{ width: 1, height: 1, label: \"one\" }\n\
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

/// `.get()` yields an optional, unwrapped with an inline `if`. Kite has no
/// `??` operator: an `if` expression does the same work in the open.
#[test]
fn get_yields_an_optional_instead_of_trapping() {
    assert_eq!(
        run_main(
            "  let xs = [10, 20]\n  let a = xs.get(1)\n  let b = xs.get(9)\n\
             \x20 io.print(if a == nil { -1 } else { a })\n\
             \x20 io.print(if b == nil { -1 } else { b })"
        ),
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
fn find(id: int) -> Option<User> {
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

/// An inline `if` narrows the optional in the branch where it cannot be nil,
/// which is what makes it a complete replacement for `?.` and `??`.
#[test]
fn an_inline_if_narrows_the_optional() {
    let src = format!(
        "{}\nfn name_of(id: int) -> str {{\n  let u = find(id)\n  return if u == nil {{ \"anonymous\" }} else {{ u.name }}\n}}\nfn main() {{\n  io.print(name_of(1))\n  io.print(name_of(2))\n}}\n",
        FINDER
    );
    assert_eq!(lines(&src), vec!["ada", "anonymous"]);
}

/// An `if` expression evaluates only the branch it takes, so the fallback is
/// not run when the value is present.
#[test]
fn an_inline_if_evaluates_only_one_branch() {
    let src = "\
fn boom() -> int {
    let a = 1
    let b = 0
    return a / b
}
fn main() {
    let xs = [7]
    let first = xs.get(0)
    io.print(if first == nil { boom() } else { first })
}
";
    assert_eq!(lines(src), vec!["7"]);
}

#[test]
fn a_value_widens_into_an_optional_binding() {
    assert_eq!(
        run_main(
            "  let a: Option<int> = 5\n  let b: Option<int> = nil\n\
             \x20 io.print(if a == nil { 0 } else { a })\n\
             \x20 io.print(if b == nil { 0 } else { b })"
        ),
        vec!["5", "0"]
    );
}

// ---- error handling -------------------------------------------------------

const DIVIDE: &str = "\
fn divide(a: int, b: int) -> (int, error) {
    if b == 0 {
        return _, errors.new(\"division by zero\")
    }
    return a / b, nil
}
";

#[test]
fn a_fallible_call_returns_a_value_and_an_error() {
    let src = format!(
        "{}\nfn main() {{\n  let (q, err) = divide(10, 2)\n  if err != nil {{\n    io.print(\"failed\")\n  }} else {{\n    io.print(q)\n  }}\n}}\n",
        DIVIDE
    );
    assert_eq!(lines(&src), vec!["5"]);
}

#[test]
fn the_error_path_carries_a_message() {
    let src = format!(
        "{}\nfn main() {{\n  let (q, err) = divide(1, 0)\n  if err != nil {{\n    io.print(err.message())\n  }} else {{\n    io.print(q)\n  }}\n}}\n",
        DIVIDE
    );
    assert_eq!(lines(&src), vec!["division by zero"]);
}

/// `check` is exactly `if err != nil { return _, err }`, so the error reaches
/// the caller unchanged.
#[test]
fn check_propagates_to_the_caller() {
    let src = format!(
        "{}\nfn ratio(a: int, b: int) -> (int, error) {{\n  let (q, err) = divide(a, b)\n  check err\n  return q * 100, nil\n}}\nfn main() {{\n  let (r, err) = ratio(10, 2)\n  if err != nil {{\n    io.print(\"e: \" + err.message())\n  }} else {{\n    io.print(r)\n  }}\n  let (r2, err2) = ratio(10, 0)\n  if err2 != nil {{\n    io.print(\"e: \" + err2.message())\n  }} else {{\n    io.print(r2)\n  }}\n}}\n",
        DIVIDE
    );
    assert_eq!(lines(&src), vec!["500", "e: division by zero"]);
}

/// After `check`, the value is readable in the same function without any
/// further test — that is the whole point of the construct.
#[test]
fn check_makes_the_value_readable() {
    let src = format!(
        "{}\nfn twice(a: int, b: int) -> (int, error) {{\n  let (q, err) = divide(a, b)\n  check err\n  return q + q, nil\n}}\nfn main() {{\n  let (v, err) = twice(10, 5)\n  if err != nil {{\n    io.print(\"e\")\n  }} else {{\n    io.print(v)\n  }}\n}}\n",
        DIVIDE
    );
    assert_eq!(lines(&src), vec!["4"]);
}

#[test]
fn several_checks_chain_in_one_function() {
    let src = format!(
        "{}\nfn chain(a: int) -> (int, error) {{\n  let (x, err) = divide(a, 2)\n  check err\n  let (y, err) = divide(x, 2)\n  check err\n  return y, nil\n}}\nfn main() {{\n  let (v, err) = chain(20)\n  if err != nil {{\n    io.print(\"e\")\n  }} else {{\n    io.print(v)\n  }}\n}}\n",
        DIVIDE
    );
    assert_eq!(lines(&src), vec!["5"]);
}

// ---- tuples ---------------------------------------------------------------

#[test]
fn a_tuple_is_built_and_destructured() {
    let src = "\
fn pair() -> (int, str) {
    return (7, \"seven\")
}
fn main() {
    io.print(match pair() {
        (0, s) => \"zero\",
        (n, s) => s,
    })
}
";
    assert_eq!(lines(src), vec!["seven"]);
}

#[test]
fn tuple_elements_bind_by_position() {
    assert_eq!(
        run_main("  let q = (1, 2)\n  io.print(match q {\n    (a, b) => a + b,\n  })"),
        vec!["3"]
    );
}

#[test]
fn nested_tuple_patterns_work() {
    assert_eq!(
        run_main("  let t = (1, (2, 3))\n  io.print(match t {\n    (a, (b, c)) => a + b + c,\n  })"),
        vec!["6"]
    );
}

#[test]
fn tuple_equality_is_structural() {
    assert_eq!(
        run_main("  io.print((1, 2) == (1, 2))\n  io.print((1, 2) == (1, 3))"),
        vec!["true", "false"]
    );
}

// ---- maps -----------------------------------------------------------------

#[test]
fn a_map_is_built_read_and_written() {
    assert_eq!(
        run_main(
            "  var m = {\"a\": 1, \"b\": 2}\n  io.print(m.len())\n\
             \x20 let a = m[\"a\"]\n  io.print(if a == nil { -1 } else { a })\n\
             \x20 m[\"c\"] = 3\n  io.print(m.len())"
        ),
        vec!["2", "1", "3"]
    );
}

/// Map indexing yields an optional, never a zero value.
#[test]
fn a_missing_key_yields_nil() {
    assert_eq!(
        run_main("  let m = {\"a\": 1}\n  let z = m[\"zz\"]\n  io.print(if z == nil { -1 } else { z })"),
        vec!["-1"]
    );
}

#[test]
fn assigning_an_existing_key_replaces_it() {
    assert_eq!(
        run_main(
            "  var m = {\"a\": 1}\n  m[\"a\"] = 9\n  let a = m[\"a\"]\n\
             \x20 io.print(if a == nil { -1 } else { a })\n  io.print(m.len())"
        ),
        vec!["9", "1"]
    );
}

/// The specification guarantees insertion order, and the representation keeps
/// it: re-assigning an existing key updates it in place rather than appending,
/// so the length does not grow. Iteration over a map, which would observe the
/// order directly, arrives with the `Iterate` trait.
#[test]
fn reassigning_a_key_does_not_append() {
    assert_eq!(
        run_main(
            "  var m = {\"z\": 1, \"a\": 2}\n  m[\"z\"] = 9\n  io.print(m.len())\n\
             \x20 let z = m[\"z\"]\n  io.print(if z == nil { -1 } else { z })"
        ),
        vec!["2", "9"]
    );
}

#[test]
fn maps_have_value_semantics() {
    assert_eq!(
        run_main("  var a = {\"k\": 1}\n  var b = a\n  b[\"k\"] = 9\n\
                  \x20 let av = a[\"k\"]\n  io.print(if av == nil { -1 } else { av })"),
        vec!["1"]
    );
}

// ---- counts wider than a byte -----------------------------------------------

/// An element or argument count was one byte wide, so a 300-element literal
/// was 44 elements long on this backend alone, a 130-entry map had two
/// entries, and a call passing 300 arguments passed 44 of them.
#[test]
fn counts_wider_than_a_byte_survive() {
    let elems: Vec<String> = (0..300).map(|i| i.to_string()).collect();
    let entries: Vec<String> = (0..130).map(|i| format!("\"k{}\": {}", i, i)).collect();
    let params: Vec<String> = (0..300).map(|i| format!("a{}: int", i)).collect();
    let src = format!(
        "fn last({}) -> int {{\n  return a299 - a0\n}}\n\
         fn main() {{\n  let xs = [{}]\n  io.print(xs.len())\n  io.print(xs[299])\n\
         \x20 let m = {{{}}}\n  io.print(m.len())\n  io.print(last({}))\n}}\n",
        params.join(", "),
        elems.join(", "),
        entries.join(", "),
        elems.join(", "),
    );
    assert_eq!(lines(&src), ["300", "299", "130", "299"]);
}

// ---- deep values and deep calls -------------------------------------------------

/// A list of a few hundred thousand cells is an ordinary value, and dropping
/// or comparing one used to recurse once per cell on the Rust stack until the
/// VM aborted — not a trap, a crash of the process. The tests here run on a
/// test thread's small stack, which makes the old failure come early.
#[test]
fn a_deep_value_is_dropped_and_compared_without_recursing() {
    let src = "\
enum List {
    Cons(head: int, tail: List)
    Empty
}
fn build(n: int) -> List {
    var l = Empty
    for i in 0..n {
        l = Cons(i, l)
    }
    return l
}
fn main() {
    let a = build(200000)
    let b = build(200000)
    io.print(a == b)
    io.print(a == build(199999))
    var c = build(200000)
    c = Empty
    io.print(c == Empty)
}
";
    assert_eq!(lines(src), ["true", "false", "true"]);
}

/// Every allocation this test binary makes, counted per thread, so a test can
/// ask how many a piece of code made without a neighbour's counting too.
struct Counting;

thread_local! {
    static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// SAFETY: every call is passed straight to the system allocator; the count is
// a `Cell` in a `const`-initialised thread local, which neither allocates nor
// registers a destructor, so counting cannot recurse into this allocator.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        std::alloc::System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        std::alloc::System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn allocations_in(f: impl FnOnce() -> bool) -> (bool, usize) {
    let before = ALLOCATIONS.with(|n| n.get());
    let answer = f();
    (answer, ALLOCATIONS.with(|n| n.get()) - before)
}

/// `==` walks a worklist only once it has an aggregate to descend into. A
/// map is a scan comparing its key with every entry's, and the worklist it
/// once made for each pair of `int`s or `str`s there made map lookups six
/// times slower — so a flat value, and an aggregate holding only flat values,
/// compares without allocating at all.
#[test]
fn equality_of_flat_values_allocates_nothing() {
    let s = |t: &str| Value::Str(Rc::from(t));
    let point = |x: i64, name: &str| {
        Value::Struct(Rc::new(StructValue {
            struct_id: 0,
            fields: RefCell::new(vec![Value::Int(x), Value::Float(0.5), s(name), Value::Nil]),
        }))
    };
    let cases = [
        (Value::Int(7), Value::Int(7), true),
        (Value::Int(7), Value::Int(8), false),
        (Value::Float(f64::NAN), Value::Float(f64::NAN), false),
        (s("key 1234"), s("key 1234"), true),
        (s("key 1234"), s("key 1235"), false),
        (Value::Nil, Value::Nil, true),
        (Value::Int(1), Value::Nil, false),
        (point(1, "a"), point(1, "a"), true),
        (point(1, "a"), point(1, "b"), false),
        (
            Value::Slice(Rc::new(vec![Value::Int(1), s("two")])),
            Value::Slice(Rc::new(vec![Value::Int(1), s("two")])),
            true,
        ),
    ];
    for (a, b, want) in &cases {
        let (answer, made) = allocations_in(|| a == b);
        assert_eq!(answer, *want, "{:?} == {:?}", a, b);
        assert_eq!(made, 0, "{:?} == {:?} allocated {} times", a, b, made);
    }
    // One level of nesting is where the worklist starts, and it still answers.
    let nested = |x| Value::Tuple(Rc::new(vec![point(x, "a"), Value::Int(3)]));
    assert!(nested(1) == nested(1));
    assert!(nested(1) != nested(2));
}

/// Frames live on the heap, so depth is bounded by memory rather than by the
/// host's stack. The limit was 2,048, and a recursion 3,000 deep trapped here
/// and nowhere else.
#[test]
fn a_deep_recursion_runs_on_the_heap() {
    let src = "\
fn sum(n: int) -> int {
    if n == 0 {
        return 0
    }
    return n + sum(n - 1)
}
fn main() {
    io.print(sum(50000))
}
";
    assert_eq!(lines(src), ["1250025000"]);
}

// ---- or-patterns --------------------------------------------------------------

/// `match s { Circle(n) | Square(n) => n, Dot => 0 }`, built as HIR directly.
///
/// Lowering bound an or-pattern's names through its first alternative
/// whichever one matched, so a `Square` read its payload as a `Circle`'s — or,
/// where the first alternative bound nothing, left the name unwritten. The
/// program is built by hand because it is MIR's half of the rule being tested:
/// the checker's half, that every alternative binds the same names, is what
/// admits this source form.
#[test]
fn an_or_pattern_binds_through_the_alternative_that_matched() {
    use kite_hir::{self as hir, Expr, ExprKind, FieldDef, LocalId, Pattern, TyId, VariantDef};
    let span = kite_span::Span::new(kite_span::FileId(0), 0, 0);
    let mut types = hir::Types::new();
    let shape = types.declare_enum("Shape", true, span);
    let payload = |name: &str| FieldDef {
        name: name.into(),
        ty: TyId::INT,
        mutable: false,
        is_pub: true,
        span,
    };
    let variant = |name: &str, fields: Vec<FieldDef>| VariantDef {
        name: name.into(),
        named: !fields.is_empty(),
        fields,
        span,
    };
    types.set_enum_variants(
        shape,
        vec![
            variant("Circle", vec![payload("r")]),
            // A second field ahead of the bound one, so reading `Square`'s
            // payload at `Circle`'s position would find the wrong value.
            variant("Square", vec![payload("colour"), payload("side")]),
            variant("Dot", Vec::new()),
        ],
    );
    let shape_ty = types.enum_ty(shape);
    let expr = |kind: ExprKind, ty: TyId| Expr { kind, ty, span };
    let n = || Pattern::Binding { local: LocalId(1), unwrap: false };
    let local = |name: &str, ty: TyId| hir::Local {
        name: name.into(),
        ty,
        mutable: false,
        span,
        synthetic: false,
    };

    let arms = vec![
        hir::MatchArm {
            pattern: Pattern::Or(vec![
                Pattern::Variant { enum_id: shape, variant: 0, fields: vec![n()] },
                Pattern::Variant { enum_id: shape, variant: 1, fields: vec![Pattern::Wildcard, n()] },
            ]),
            guard: None,
            body: expr(ExprKind::Local(LocalId(1)), TyId::INT),
            span,
        },
        hir::MatchArm {
            pattern: Pattern::Variant { enum_id: shape, variant: 2, fields: Vec::new() },
            guard: None,
            body: expr(ExprKind::Int(0), TyId::INT),
            span,
        },
    ];
    let subject = expr(ExprKind::Local(LocalId(0)), shape_ty);
    let pick = hir::Function {
        name: "pick".into(),
        is_free: true,
        generic_count: 0,
        is_pub: false,
        is_async: false,
        param_count: 1,
        locals: vec![local("s", shape_ty), local("n", TyId::INT)],
        ret: TyId::INT,
        body: hir::Block {
            stmts: vec![hir::Stmt::Return {
                value: Some(expr(
                    ExprKind::Match { scrutinee: Box::new(subject), arms },
                    TyId::INT,
                )),
                span,
            }],
        },
        span,
    };

    let print_pick = |variant: u32, fields: Vec<i64>| {
        let value = expr(
            ExprKind::EnumNew {
                enum_id: shape,
                variant,
                fields: fields.into_iter().map(|v| expr(ExprKind::Int(v), TyId::INT)).collect(),
            },
            shape_ty,
        );
        let call = expr(
            ExprKind::Call { callee: hir::FnId(0), args: vec![value], targs: Vec::new() },
            TyId::INT,
        );
        hir::Stmt::Expr(expr(
            ExprKind::CallBuiltin { builtin: hir::Builtin::IoPrint, args: vec![call] },
            TyId::UNIT,
        ))
    };
    let main = hir::Function {
        name: "main".into(),
        is_free: true,
        generic_count: 0,
        is_pub: false,
        is_async: false,
        param_count: 0,
        locals: Vec::new(),
        ret: TyId::UNIT,
        body: hir::Block {
            stmts: vec![
                print_pick(1, vec![99, 7]),
                print_pick(0, vec![3]),
                print_pick(2, Vec::new()),
            ],
        },
        span,
    };

    let program = hir::Program {
        types,
        externs: Vec::new(),
        fns: vec![pick, main],
        entry: Some(hir::FnId(1)),
        vtables: Vec::new(),
    };
    let mir = kite_mir::lower(&program);
    let chunk = kite_codegen_kbc::compile(&mir);
    let mut out = Vec::new();
    run(&chunk, &mut out).expect("the program runs");
    assert_eq!(String::from_utf8(out).unwrap(), "7\n3\n0\n");
}
