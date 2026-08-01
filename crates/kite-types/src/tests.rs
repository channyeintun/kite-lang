use super::*;
use kite_span::SourceMap;

struct Ctx {
    program: hir::Program,
    diags: DiagBag,
    sources: SourceMap,
}

impl Ctx {
    fn codes(&self) -> Vec<&'static str> {
        self.diags.iter().filter_map(|d| d.code.map(|c| c.0)).collect()
    }

    fn render(&self) -> String {
        self.diags.render_all(&self.sources)
    }

    fn has(&self, code: &str) -> bool {
        self.codes().contains(&code)
    }
}

fn run(src: &str) -> Ctx {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source has syntax errors:\n{}",
        diags.render_all(&sources)
    );
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let program = check(&ast, &resolved, src, &mut diags);
    Ctx { program, diags, sources }
}

/// Wrap statements in a `main` so tests read as bodies.
fn body(stmts: &str) -> Ctx {
    run(&format!("fn main() {{\n{}\n}}\n", stmts))
}

fn ok(src: &str) -> Ctx {
    let c = run(src);
    assert!(!c.diags.has_errors(), "unexpected diagnostics:\n{}", c.render());
    c
}

fn ok_body(stmts: &str) -> Ctx {
    let c = body(stmts);
    assert!(!c.diags.has_errors(), "unexpected diagnostics:\n{}", c.render());
    c
}

// ---- inference ------------------------------------------------------------

#[test]
fn infers_literal_types() {
    let c = ok_body("  let a = 1\n  let b = 1.5\n  let s = \"x\"\n  let t = true");
    let locals = &c.program.fns[0].locals;
    assert_eq!(locals[0].ty, TyId::INT);
    assert_eq!(locals[1].ty, TyId::FLOAT);
    assert_eq!(locals[2].ty, TyId::STR);
    assert_eq!(locals[3].ty, TyId::BOOL);
}

#[test]
fn annotations_are_checked_against_initialisers() {
    let c = body("  let a: int = 1.5");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn binding_a_unit_value_is_rejected() {
    let c = run("fn nothing() {\n}\nfn main() {\n  let x = nothing()\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("produces no value"), "{}", c.render());
}

#[test]
fn a_let_with_neither_type_nor_value_is_rejected() {
    // The parser accepts it; the checker needs one or the other.
    let c = run("fn main() {\n  let x\n}\n");
    assert!(c.has("E0204"), "{}", c.render());
}

// ---- no implicit conversion ----------------------------------------------

#[test]
fn int_and_float_do_not_mix() {
    let c = body("  let a = 1 + 1.5");
    assert!(c.has("E0201"), "{}", c.render());
    assert!(
        c.render().contains("no implicit numeric conversion"),
        "{}",
        c.render()
    );
}

#[test]
fn passing_a_float_where_int_is_wanted_is_rejected() {
    let c = run("fn f(n: int) {\n}\nfn main() {\n  f(1.5)\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("as int"), "{}", c.render());
}

/// The right operand's literal is steered by the left operand's type, so this
/// works without an annotation.
#[test]
fn float_arithmetic_with_a_literal_works() {
    ok_body("  let x = 1.5\n  let y = x * 2.0");
}

// ---- no truthiness --------------------------------------------------------

#[test]
fn an_int_condition_is_rejected_with_a_suggestion() {
    let c = body("  if 1 {\n  }");
    assert!(c.has("E0202"), "{}", c.render());
    assert!(c.render().contains("n != 0"), "{}", c.render());
}

#[test]
fn logical_operators_require_bool() {
    let c = body("  let x = 1 && true");
    assert!(c.has("E0201"), "{}", c.render());
    assert!(c.render().contains("no truthiness"), "{}", c.render());
}

#[test]
fn bool_conditions_are_accepted() {
    ok_body("  let a = 3\n  if a > 2 && a < 10 {\n    io.print(a)\n  }");
}

// ---- operators ------------------------------------------------------------

#[test]
fn string_concatenation_works_but_subtraction_does_not() {
    ok_body("  let s = \"a\" + \"b\"");
    let c = body("  let s = \"a\" - \"b\"");
    assert!(c.has("E0201"), "{}", c.render());
    assert!(c.render().contains("concatenates"), "{}", c.render());
}

#[test]
fn bools_are_equatable_but_not_ordered() {
    ok_body("  let a = true == false");
    let c = body("  let a = true < false");
    assert!(c.has("E0201"), "{}", c.render());
    assert!(c.render().contains("not ordered"), "{}", c.render());
}

#[test]
fn float_equality_warns() {
    let c = body("  let a = 1.0\n  let b = 2.0\n  let e = a == b");
    assert!(c.has("E0201"), "{}", c.render());
    assert!(c.render().contains("approx_eq"), "{}", c.render());
    assert!(!c.diags.has_errors(), "must be a warning, not an error");
}

#[test]
fn float_remainder_suggests_the_library_function() {
    let c = body("  let a = 1.0 % 2.0");
    assert!(c.render().contains("math.rem"), "{}", c.render());
}

#[test]
fn negation_type_rules() {
    ok_body("  let a = -1\n  let b = -1.5\n  let c = !true");
    let c = body("  let a = -true");
    assert!(c.has("E0201"), "{}", c.render());
}

// ---- functions ------------------------------------------------------------

#[test]
fn checks_argument_types_and_counts() {
    ok("fn add(a: int, b: int) -> int {\n  return a + b\n}\nfn main() {\n  let x = add(1, 2)\n}\n");

    let c = run("fn add(a: int, b: int) -> int {\n  return a + b\n}\nfn main() {\n  let x = add(1)\n}\n");
    assert!(c.has("E0113"), "{}", c.render());
    assert!(c.render().contains("no default arguments"), "{}", c.render());
}

#[test]
fn a_missing_return_is_reported() {
    let c = run("fn f() -> int {\n  let x = 1\n}\n");
    assert!(c.has("E0203"), "{}", c.render());
}

#[test]
fn returning_from_every_branch_satisfies_the_check() {
    ok("fn f(a: int) -> int {\n  if a > 0 {\n    return 1\n  } else {\n    return 2\n  }\n}\n");
}

/// Without an `else`, control can fall past the `if`.
#[test]
fn a_return_in_only_one_branch_is_not_enough() {
    let c = run("fn f(a: int) -> int {\n  if a > 0 {\n    return 1\n  }\n}\n");
    assert!(c.has("E0203"), "{}", c.render());
}

#[test]
fn returning_the_wrong_type_is_reported() {
    let c = run("fn f() -> int {\n  return \"x\"\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn returning_a_value_from_a_unit_function_is_reported() {
    let c = run("fn f() {\n  return 1\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn calling_a_local_is_reported() {
    let c = body("  let x = 1\n  x(2)");
    assert!(c.has("E0205"), "{}", c.render());
}

#[test]
fn unknown_types_are_reported_with_the_known_set() {
    let c = run("fn f(a: Widget) {\n}\n");
    assert!(c.has("E0204"), "{}", c.render());
    assert!(c.render().contains("int"), "{}", c.render());
}

// ---- mutability -----------------------------------------------------------

#[test]
fn assigning_to_a_let_is_rejected_with_a_var_fix() {
    let c = body("  let total = 0\n  total = 1");
    assert!(c.has("E0114"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("declared immutable here"), "{}", out);
    assert!(out.contains("help: make the binding mutable"), "{}", out);
    assert!(out.contains("var total = 0"), "{}", out);
}

#[test]
fn assigning_to_a_var_is_accepted() {
    ok_body("  var total = 0\n  total = 1\n  total += 2");
}

#[test]
fn compound_assignment_checks_operand_types() {
    let c = body("  var n = 0\n  n += \"x\"");
    assert!(c.has("E0201"), "{}", c.render());
}

// ---- definite assignment --------------------------------------------------
//
// The specification permits `let x: T` followed by assignment in branches,
// "provided the compiler can prove exactly one assignment occurs on every path
// before first use". These pin down that proof.

#[test]
fn a_let_declared_without_a_value_may_be_assigned_once_per_path() {
    ok_body("  let z: int\n  if true {\n    z = 1\n  } else {\n    z = 2\n  }\n  io.print(z)");
}

#[test]
fn assigning_twice_to_an_immutable_binding_is_rejected() {
    let c = body("  let z: int\n  z = 1\n  z = 2");
    assert!(c.has("E0114"), "{}", c.render());
    assert_eq!(c.diags.error_count(), 1, "{}", c.render());
}

/// Assigned on only one branch, so control can reach the read with no value.
#[test]
fn reading_a_binding_assigned_on_only_one_path_is_rejected() {
    let c = body("  let z: int\n  if true {\n    z = 1\n  }\n  io.print(z)");
    assert!(c.has("E0110"), "{}", c.render());
}

/// A branch that diverges contributes nothing to the join, so the other
/// branch's assignment is enough.
#[test]
fn a_diverging_branch_does_not_block_the_other_branchs_assignment() {
    ok("fn f(c: bool) -> int {\n  let z: int\n  if c {\n    return 0\n  } else {\n    z = 1\n  }\n  return z\n}\n");
}

#[test]
fn reading_before_any_assignment_is_rejected() {
    let c = body("  let z: int\n  io.print(z)");
    assert!(c.has("E0110"), "{}", c.render());
}

/// A loop body may run more than once, so it cannot be the single write to an
/// immutable binding.
#[test]
fn assigning_to_an_immutable_binding_inside_a_loop_is_rejected() {
    let c = body("  let z: int\n  for i in 0..3 {\n    z = i\n  }");
    assert!(c.has("E0114"), "{}", c.render());
    assert!(c.render().contains("more than once"), "{}", c.render());
}

/// A loop body may run zero times, so an assignment inside it does not make
/// the binding definitely assigned afterwards.
#[test]
fn a_loop_body_assignment_does_not_count_as_definite() {
    let c = body("  var z: int = 0\n  let w: int\n  for i in 0..3 {\n    z = i\n  }\n  io.print(z)");
    assert!(!c.has("E0110"), "{}", c.render());
    let _ = c;
}

#[test]
fn a_var_may_be_assigned_repeatedly_including_in_a_loop() {
    ok_body("  var n = 0\n  for i in 0..3 {\n    n = n + i\n  }\n  io.print(n)");
}

#[test]
fn a_loop_variable_counts_as_assigned() {
    ok_body("  for i in 0..3 {\n    io.print(i)\n  }");
}

#[test]
fn one_missing_assignment_yields_one_diagnostic() {
    let c = body("  let z: int\n  io.print(z)\n  io.print(z)\n  io.print(z)");
    assert_eq!(c.diags.error_count(), 1, "{}", c.render());
}

// ---- control flow ---------------------------------------------------------

#[test]
fn for_range_binds_an_int_loop_variable() {
    let c = ok_body("  for i in 0..10 {\n    io.print(i)\n  }");
    let locals = &c.program.fns[0].locals;
    assert!(locals.iter().any(|l| l.name == "i" && l.ty == TyId::INT));
}

#[test]
fn a_non_int_range_bound_is_rejected() {
    let c = body("  for i in 0..\"x\" {\n  }");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn unreachable_code_is_a_warning_not_an_error() {
    let c = run("fn f() -> int {\n  return 1\n  let x = 2\n}\n");
    assert!(c.has("E0116"), "{}", c.render());
    assert!(!c.diags.has_errors(), "must be a warning:\n{}", c.render());
}

#[test]
fn if_used_as_a_value_requires_matching_branches() {
    ok_body("  let a = if true { 1 } else { 2 }");
    let c = body("  let a = if true { 1 } else { \"x\" }");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("different types"), "{}", c.render());
}

// ---- builtins -------------------------------------------------------------

#[test]
fn io_print_accepts_every_printable_type() {
    ok_body("  io.print(1)\n  io.print(1.5)\n  io.print(true)\n  io.print(\"s\")");
}

#[test]
fn io_print_rejects_a_wrong_arity() {
    let c = body("  io.print(1, 2)");
    assert!(c.has("E0113"), "{}", c.render());
}

// ---- literals -------------------------------------------------------------

#[test]
fn integer_literal_forms_all_parse() {
    let c = ok_body("  let a = 42\n  let b = 0xFF\n  let c = 0o755\n  let d = 0b1010\n  let e = 1_000");
    let f = &c.program.fns[0];
    let values: Vec<i64> = f
        .body
        .stmts
        .iter()
        .filter_map(|s| match s {
            hir::Stmt::Let { init: Some(e), .. } => match e.kind {
                ExprKind::Int(v) => Some(v),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(values, vec![42, 255, 493, 10, 1000]);
}

#[test]
fn an_out_of_range_integer_is_reported() {
    let c = body("  let a = 99999999999999999999");
    assert!(c.has("E0004"), "{}", c.render());
}

#[test]
fn string_escapes_are_decoded() {
    let c = ok_body("  let s = \"a\\nb\\tc\\u{41}\"");
    let f = &c.program.fns[0];
    let hir::Stmt::Let { init: Some(e), .. } = &f.body.stmts[0] else {
        panic!()
    };
    let ExprKind::Str(s) = &e.kind else { panic!() };
    assert_eq!(s, "a\nb\tcA");
}

#[test]
fn an_invalid_escape_is_reported() {
    let c = body("  let s = \"a\\qb\"");
    assert!(c.has("E0003"), "{}", c.render());
}

// ---- cascade suppression --------------------------------------------------

/// One mistake must yield one diagnostic. `TyId::ERROR` satisfies every
/// expectation precisely so that downstream uses stay quiet.
#[test]
fn one_type_error_does_not_cascade() {
    let c = body("  let a: int = \"x\"\n  let b = a + 1\n  let d = a * 2\n  io.print(a)");
    assert_eq!(
        c.diags.error_count(),
        1,
        "expected one error, got:\n{}",
        c.render()
    );
}

#[test]
fn an_unknown_name_does_not_cascade_into_type_errors() {
    let c = body("  let a = nope\n  let b = a + 1");
    assert_eq!(
        c.diags.error_count(),
        1,
        "expected one error, got:\n{}",
        c.render()
    );
}

// ---- the Phase 1 program --------------------------------------------------

#[test]
fn the_phase_one_program_type_checks() {
    let c = ok("\
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
");
    assert_eq!(c.program.fns.len(), 2);
    assert_eq!(c.program.entry, Some(hir::FnId(1)));
    assert_eq!(c.program.fns[0].ret, TyId::INT);
    assert_eq!(c.program.fns[0].param_count, 2);
}

// ---- structs --------------------------------------------------------------

const RECT: &str = "\
struct Rect {
    width: int
    var label: str
}
";

fn with_rect(body: &str) -> Ctx {
    run(&format!("{}\nfn main() {{\n{}\n}}\n", RECT, body))
}

fn ok_rect(body: &str) -> Ctx {
    let c = with_rect(body);
    assert!(!c.diags.has_errors(), "unexpected diagnostics:\n{}", c.render());
    c
}

#[test]
fn a_complete_struct_literal_checks() {
    ok_rect("  let r = Rect{ width: 1, label: \"x\" }\n  io.print(r.width)");
}

/// Kite has no zero values, so a forgotten field is an error rather than a
/// silent `0`. This is the specification's stated reason for the rule.
#[test]
fn a_missing_field_is_rejected_and_names_it() {
    let c = with_rect("  let r = Rect{ width: 1 }");
    assert!(c.has("E0200"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("`label`"), "{}", out);
    assert!(out.contains("no zero values"), "{}", out);
}

#[test]
fn an_unknown_field_lists_the_real_ones() {
    let c = with_rect("  let r = Rect{ width: 1, label: \"x\", height: 2 }");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("width, label"), "{}", c.render());
}

#[test]
fn a_duplicated_field_is_rejected() {
    let c = with_rect("  let r = Rect{ width: 1, width: 2, label: \"x\" }");
    assert!(c.has("E0112"), "{}", c.render());
}

#[test]
fn a_field_type_mismatch_is_reported() {
    let c = with_rect("  let r = Rect{ width: \"wide\", label: \"x\" }");
    assert!(c.has("E0200"), "{}", c.render());
}

/// `..base` supplies the fields the literal omits.
#[test]
fn functional_update_fills_the_gaps() {
    ok_rect("  let a = Rect{ width: 1, label: \"x\" }\n  let b = Rect{ ..a, width: 2 }");
}

#[test]
fn reading_an_unknown_field_lists_the_real_ones() {
    let c = with_rect("  let r = Rect{ width: 1, label: \"x\" }\n  io.print(r.height)");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("width, label"), "{}", c.render());
}

#[test]
fn a_primitive_has_no_fields() {
    let c = with_rect("  let n = 1\n  io.print(n.x)");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("has no fields"), "{}", c.render());
}

// ---- field mutability -----------------------------------------------------

#[test]
fn a_var_field_may_be_assigned() {
    ok_rect("  let r = Rect{ width: 1, label: \"x\" }\n  r.label = \"y\"");
}

/// Fields are immutable by default. The message explains both fixes, because
/// building a new value is usually the better one.
#[test]
fn an_immutable_field_cannot_be_assigned() {
    let c = with_rect("  let r = Rect{ width: 1, label: \"x\" }\n  r.width = 2");
    assert!(c.has("E0114"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("declared immutable here"), "{}", out);
    assert!(out.contains("var width"), "{}", out);
    assert!(out.contains("..old"), "{}", out);
}

// ---- methods --------------------------------------------------------------

const SHAPES: &str = "\
struct Sq {
    side: int
}

impl Sq {
    fn area(self) -> int {
        return self.side * self.side
    }
    fn make(n: int) -> Sq {
        return Sq{ side: n }
    }
}
";

fn with_shapes(body: &str) -> Ctx {
    run(&format!("{}\nfn main() {{\n{}\n}}\n", SHAPES, body))
}

#[test]
fn methods_and_associated_functions_check() {
    let c = with_shapes("  io.print(Sq.make(3).area())");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn an_unknown_method_lists_the_real_ones() {
    let c = with_shapes("  let s = Sq{ side: 1 }\n  io.print(s.perimeter())");
    assert!(c.has("E0205"), "{}", c.render());
    assert!(c.render().contains("area, make"), "{}", c.render());
}

#[test]
fn calling_a_field_suggests_dropping_the_parentheses() {
    let c = with_shapes("  let s = Sq{ side: 1 }\n  io.print(s.side())");
    assert!(c.has("E0205"), "{}", c.render());
    assert!(c.render().contains("is a field, not a method"), "{}", c.render());
}

#[test]
fn calling_an_associated_function_as_a_method_is_reported() {
    let c = with_shapes("  let s = Sq{ side: 1 }\n  io.print(s.make(2).side)");
    assert!(c.has("E0205"), "{}", c.render());
    assert!(c.render().contains("associated function"), "{}", c.render());
}

#[test]
fn calling_a_method_as_an_associated_function_is_reported() {
    let c = with_shapes("  io.print(Sq.area())");
    assert!(c.has("E0205"), "{}", c.render());
    assert!(c.render().contains("not an associated function"), "{}", c.render());
}

#[test]
fn self_outside_a_method_is_reported() {
    let c = run("fn f() {\n  io.print(self)\n}\n");
    assert!(c.has("E0111"), "{}", c.render());
}

#[test]
fn a_type_name_is_not_a_value() {
    let c = with_rect("  let r = Rect");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("is a type, not a value"), "{}", c.render());
}

#[test]
fn a_local_shadows_a_module_path() {
    // A local named `io` wins over the `io.print` builtin, which is why the
    // resolver checks locals before dotted names.
    let c = run("struct B {\n  print: int\n}\nfn main() {\n  let io = B{ print: 7 }\n  let n = io.print\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

// ---- enums and match ------------------------------------------------------

const SHAPE: &str = "\
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}
";

fn with_shape(body: &str) -> Ctx {
    run(&format!("{}\nfn main() {{\n{}\n}}\n", SHAPE, body))
}

#[test]
fn a_match_covering_every_variant_checks() {
    let c = with_shape(
        "  let d = match Point {\n    Circle(r) => 1,\n    Rect(w, h) => 2,\n    Point => 3,\n  }",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The Phase 2 exit criterion: the missing variants are named.
#[test]
fn a_non_exhaustive_match_names_the_missing_variants() {
    let c = with_shape("  let d = match Point {\n    Circle(r) => 1,\n  }");
    assert!(c.has("E0210"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("`Rect(_, _)`"), "{}", out);
    assert!(out.contains("`Point`"), "{}", out);
}

#[test]
fn a_wildcard_makes_a_match_exhaustive() {
    let c = with_shape("  let d = match Point {\n    Circle(r) => 1,\n    _ => 0,\n  }");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// A guard may fail at run time, so a guarded arm cannot make a match
/// exhaustive. The message says so.
#[test]
fn guarded_arms_do_not_count_towards_coverage() {
    let c = with_shape(
        "  let d = match Point {\n    Circle(r) if r > 0 => 1,\n    Rect(w, h) if w > 0 => 2,\n    Point if true => 3,\n  }",
    );
    assert!(c.has("E0210"), "{}", c.render());
    assert!(c.render().contains("guard may fail at run time"), "{}", c.render());
}

#[test]
fn an_int_match_needs_a_catch_all() {
    let c = run("fn main() {\n  let n = 1\n  let d = match n {\n    0 => \"a\",\n    1 => \"b\",\n  }\n}\n");
    assert!(c.has("E0210"), "{}", c.render());
}

#[test]
fn a_bool_match_is_exhaustive_with_both_values() {
    let c = run("fn main() {\n  let b = true\n  let d = match b {\n    true => 1,\n    false => 2,\n  }\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn match_arms_must_agree_on_type() {
    let c = with_shape("  let d = match Point {\n    Circle(r) => 1,\n    _ => \"x\",\n  }");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("different types"), "{}", c.render());
}

#[test]
fn a_guard_must_be_bool() {
    let c = with_shape("  let d = match Point {\n    Circle(r) if r => 1,\n    _ => 0,\n  }");
    assert!(c.has("E0202"), "{}", c.render());
}

#[test]
fn a_payload_pattern_must_match_the_variants_arity() {
    let c = with_shape("  let d = match Point {\n    Rect(w) => 1,\n    _ => 0,\n  }");
    assert!(c.has("E0113"), "{}", c.render());
}

/// Forgetting the payload is a common slip, so the message shows both fixes.
#[test]
fn omitting_a_payload_pattern_suggests_both_forms() {
    let c = with_shape("  let d = match Point {\n    Circle => 1,\n    _ => 0,\n  }");
    assert!(c.has("E0113"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("Circle(radius)"), "{}", out);
    assert!(out.contains("Circle(_)"), "{}", out);
}

#[test]
fn a_unit_variant_rejects_a_payload() {
    let c = with_shape("  let d = match Point {\n    Point(x) => 1,\n    _ => 0,\n  }");
    assert!(c.has("E0113"), "{}", c.render());
}

#[test]
fn variant_construction_checks_payload_types() {
    let c = with_shape("  let s = Circle(radius: \"big\")");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn an_unknown_payload_field_lists_the_real_ones() {
    let c = with_shape("  let s = Circle(diameter: 2)");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("radius"), "{}", c.render());
}

/// Named arguments exist only for variant payloads. Everywhere else the answer
/// is a struct, whose literal names every field anyway.
#[test]
fn functions_reject_named_arguments() {
    let c = run("fn f(a: int) {\n}\nfn main() {\n  f(a: 1)\n}\n");
    assert!(c.has("E0113"), "{}", c.render());
    assert!(c.render().contains("no named arguments"), "{}", c.render());
}

#[test]
fn matching_the_wrong_enum_is_reported() {
    let c = run(&format!(
        "{}\nenum Other {{\n  Alpha\n}}\nfn main() {{\n  let d = match Point {{\n    Alpha => 1,\n    _ => 0,\n  }}\n}}\n",
        SHAPE
    ));
    assert!(c.has("E0200"), "{}", c.render());
}
