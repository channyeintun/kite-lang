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
    let program = check(&ast, &resolved, &sources, &mut diags);
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

/// The carve-out the specification states: a literal operand means the
/// comparison is deliberate. `x == 0.0` is the guard written before a division
/// or a logarithm — the question really is "exactly zero?", and a tolerance
/// answers a different one. Warning on it fired on every such guard in
/// `std/math`, which is how a warning teaches people to stop reading warnings.
#[test]
fn float_equality_against_a_literal_is_deliberate() {
    let c = body("  let a = 1.0\n  let e = a == 0.0");
    assert!(!c.has("E0201"), "{}", c.render());
    let flipped = body("  let a = 1.0\n  let e = 0.0 == a");
    assert!(!flipped.has("E0201"), "{}", flipped.render());
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
    ok_rect("  var r = Rect{ width: 1, label: \"x\" }\n  r.label = \"y\"");
}

/// A `var` field is still only reachable through a binding that may change:
/// otherwise `let` would promise nothing at all about the value it names.
#[test]
fn a_var_field_cannot_be_assigned_through_a_let() {
    let c = with_rect("  let r = Rect{ width: 1, label: \"x\" }\n  r.label = \"y\"");
    assert!(c.has("E0114"), "{}", c.render());
}

/// Both halves of the `var self` contract from section 8.2.
#[test]
fn a_plain_self_receiver_cannot_modify_a_field() {
    let c = run("struct C {\n  var n: int\n}\nimpl C {\n  fn bump(self) {\n    self.n = self.n + 1\n  }\n}\nfn main() {\n}\n");
    assert!(c.has("E0114"), "{}", c.render());
    assert!(c.render().contains("var self"), "{}", c.render());
}

#[test]
fn a_var_self_method_needs_a_var_receiver() {
    let c = run("struct C {\n  var n: int\n}\nimpl C {\n  fn bump(var self) {\n    self.n = self.n + 1\n  }\n}\nfn main() {\n  let c = C{ n: 1 }\n  c.bump()\n}\n");
    assert!(c.has("E0114"), "{}", c.render());
}

#[test]
fn a_var_self_method_through_a_var_binding_is_accepted() {
    let c = run("struct C {\n  var n: int\n}\nimpl C {\n  fn bump(var self) {\n    self.n = self.n + 1\n  }\n}\nfn main() {\n  var c = C{ n: 1 }\n  c.bump()\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
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

// ---- traits ---------------------------------------------------------------

const TRAITED: &str = "\
struct P {
    n: int
}
trait Shape {
    fn area(self) -> int
    fn describe(self) -> str {
        return \"a shape\"
    }
}
";

#[test]
fn a_complete_trait_impl_checks() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area(self) -> int {{\n    return self.n\n  }}\n}}\nfn main() {{\n  io.print(P{{ n: 1 }}.area())\n}}\n",
        TRAITED
    ));
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_missing_required_method_is_reported() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n}}\nfn main() {{\n}}\n",
        TRAITED
    ));
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("`area`"), "{}", c.render());
}

/// A method with a default need not be provided.
#[test]
fn a_defaulted_method_may_be_omitted() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area(self) -> int {{\n    return 1\n  }}\n}}\nfn main() {{\n  io.print(P{{ n: 1 }}.describe())\n}}\n",
        TRAITED
    ));
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_method_the_trait_does_not_declare_is_rejected() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area(self) -> int {{\n    return 1\n  }}\n  fn extra(self) -> int {{\n    return 2\n  }}\n}}\nfn main() {{\n}}\n",
        TRAITED
    ));
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("inherent"), "{}", c.render());
}

#[test]
fn a_wrong_parameter_count_is_reported() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area(self, extra: int) -> int {{\n    return 1\n  }}\n}}\nfn main() {{\n}}\n",
        TRAITED
    ));
    assert!(c.has("E0113"), "{}", c.render());
}

#[test]
fn a_missing_receiver_is_reported() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area() -> int {{\n    return 1\n  }}\n}}\nfn main() {{\n}}\n",
        TRAITED
    ));
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("receiver"), "{}", c.render());
}

/// Exactly one implementation per trait and type is what makes trait
/// resolution decidable.
#[test]
fn implementing_a_trait_twice_for_one_type_is_rejected() {
    let c = run(&format!(
        "{}\nimpl Shape for P {{\n  fn area(self) -> int {{\n    return 1\n  }}\n}}\nimpl Shape for P {{\n  fn area(self) -> int {{\n    return 2\n  }}\n}}\nfn main() {{\n}}\n",
        TRAITED
    ));
    assert!(c.has("E0112"), "{}", c.render());
    assert!(c.render().contains("decidable"), "{}", c.render());
}

#[test]
fn a_trait_is_not_a_type() {
    let c = run(&format!("{}\nfn f(s: Shape) {{\n}}\nfn main() {{\n}}\n", TRAITED));
    assert!(c.has("E0204"), "{}", c.render());
    assert!(c.render().contains("dyn Shape"), "{}", c.render());
}

#[test]
fn implementing_an_unknown_trait_is_reported() {
    let c = run("struct P {\n  n: int\n}\nimpl Nope for P {\n}\nfn main() {\n}\n");
    assert!(c.has("E0204"), "{}", c.render());
}

// ---- slices and optionals -------------------------------------------------

#[test]
fn an_empty_slice_literal_needs_a_type() {
    let c = body("  let xs = []");
    assert!(c.has("E0204"), "{}", c.render());
    assert!(c.render().contains("[int]"), "{}", c.render());
    ok_body("  let xs: [int] = []");
}

#[test]
fn slice_elements_must_share_one_type() {
    let c = body("  let xs = [1, \"two\"]");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn indexing_a_non_slice_is_reported() {
    let c = body("  let n = 1\n  io.print(n[0])");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("cannot be indexed"), "{}", c.render());
}

#[test]
fn a_slice_index_must_be_an_int() {
    let c = body("  let xs = [1]\n  io.print(xs[\"a\"])");
    assert!(c.has("E0200"), "{}", c.render());
}

/// Slices are copy-on-write values, so mutating one changes the binding, which
/// must therefore be `var`.
#[test]
fn mutating_a_let_slice_is_rejected_with_a_var_fix() {
    let c = body("  let xs = [1]\n  xs.push(2)");
    assert!(c.has("E0114"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("copy-on-write"), "{}", out);
    assert!(out.contains("var xs = [1]"), "{}", out);

    let c = body("  let xs = [1]\n  xs[0] = 2");
    assert!(c.has("E0114"), "{}", c.render());
}

#[test]
fn a_var_slice_may_be_mutated() {
    ok_body("  var xs = [1]\n  xs.push(2)\n  xs[0] = 3");
}

#[test]
fn an_unknown_slice_method_lists_the_real_ones() {
    let c = body("  let xs = [1]\n  io.print(xs.pop())");
    assert!(c.has("E0205"), "{}", c.render());
    assert!(c.render().contains("len, get, push"), "{}", c.render());
}

#[test]
fn iterating_a_non_iterable_is_reported() {
    let c = body("  let n = 1\n  for x in n {\n  }");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("not iterable"), "{}", c.render());
}

// ---- optionals ------------------------------------------------------------

/// Kite has no null: `nil` only fits where the type is optional.
#[test]
fn nil_is_rejected_where_a_plain_type_is_wanted() {
    let c = body("  let n: int = nil");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("no null"), "{}", c.render());
}

#[test]
fn nil_needs_a_type_from_context() {
    let c = body("  let n = nil");
    assert!(c.has("E0204"), "{}", c.render());
}

/// An inline `if` narrows the optional in the branch where it cannot be nil.
/// This is the whole replacement for `??` and `?.`.
#[test]
fn an_inline_if_narrows_the_optional() {
    ok_body("  let xs = [1]\n  let a = xs.get(0)\n  let n: int = if a == nil { 0 } else { a }");
}

/// The narrowing is directional: `x != nil` narrows the *then* branch.
#[test]
fn a_not_nil_test_narrows_the_then_branch() {
    ok_body("  let xs = [1]\n  let a = xs.get(0)\n  let n: int = if a != nil { a } else { 0 }");
}

/// Outside the narrowed branch the value is still optional.
#[test]
fn the_other_branch_is_not_narrowed() {
    let c = body("  let xs = [1]\n  let a = xs.get(0)\n  let n: int = if a == nil { a } else { 0 }");
    assert!(c.has("E0200"), "{}", c.render());
}

/// The specification's example: once `nil` is matched, the binding is the
/// unwrapped type.
#[test]
fn a_binding_narrows_once_nil_is_covered() {
    let c = run("struct U {\n  name: str\n}\nfn find() -> Option<U> {\n  return nil\n}\nfn main() {\n  io.print(match find() {\n    nil => \"none\",\n    u => u.name,\n  })\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// Without an earlier `nil` arm the binding could still receive one, so it is
/// not narrowed.
#[test]
fn a_binding_is_not_narrowed_before_nil_is_covered() {
    let c = run("struct U {\n  name: str\n}\nfn find() -> Option<U> {\n  return nil\n}\nfn main() {\n  io.print(match find() {\n    u => u.name,\n  })\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn an_optional_match_must_cover_nil_and_a_value() {
    let c = run("fn main() {\n  let x: Option<int> = 1\n  let d = match x {\n    nil => 0,\n  }\n}\n");
    assert!(c.has("E0210"), "{}", c.render());
    assert!(c.render().contains("a present value"), "{}", c.render());
}

#[test]
fn nil_cannot_match_a_non_optional() {
    let c = body("  let n = 1\n  let d = match n {\n    nil => 0,\n    _ => 1,\n  }");
    assert!(c.has("E0200"), "{}", c.render());
}

// ---- error handling -------------------------------------------------------

const FALLIBLE: &str = "\
fn load() -> (int, error) {
    return 1, nil
}
";

fn with_fallible(body: &str) -> Ctx {
    run(&format!("{}\nfn main() {{\n{}\n}}\n", FALLIBLE, body))
}

#[test]
fn checking_the_error_makes_the_value_readable() {
    let c = with_fallible("  let (v, err) = load()\n  if err != nil {\n    io.print(0)\n  } else {\n    io.print(v)\n  }");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The flaw Kite fixes in Go: reading the value before the error is known.
#[test]
fn reading_the_value_before_checking_is_rejected() {
    let c = with_fallible("  let (v, err) = load()\n  io.print(v)\n  if err != nil {\n    io.print(0)\n  }");
    assert!(c.has("E0301"), "{}", c.render());
    let out = c.render();
    assert!(out.contains("only valid when the error is nil"), "{}", out);
    assert!(out.contains("in Go the value on a failure path"), "{}", out);
}

#[test]
fn an_unchecked_error_is_rejected() {
    let c = with_fallible("  let (v, err) = load()\n  io.print(1)");
    assert!(c.has("E0302"), "{}", c.render());
    assert!(c.render().contains("goes out of scope uninspected"), "{}", c.render());
}

/// Discarding the *error* slot with `_` is exactly what Kite forbids.
#[test]
fn discarding_the_error_with_underscore_is_rejected() {
    let c = with_fallible("  let (v, _) = load()\n  io.print(v)");
    assert!(c.has("E0302"), "{}", c.render());
}

/// `if err != nil { return _, err }` — control continues only when the error
/// was nil, so the value it guards becomes valid afterwards.
#[test]
fn an_early_return_cleans_the_value() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn use_it() -> (int, error) {\n  let (v, err) = load()\n  if err != nil {\n    return _, err\n  }\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The same shape, but the error branch does *not* diverge, so the value is
/// still not proven valid on the path that falls through.
#[test]
fn a_non_diverging_error_branch_does_not_clean_the_value() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n  let (v, err) = load()\n  if err != nil {\n    io.print(\"oops\")\n  }\n  io.print(v)\n}\n");
    assert!(c.has("E0301"), "{}", c.render());
}

/// The wrapping form the specification and `std/errors` both show. A wrapper
/// answers nil exactly when what it wrapped was nil, so passing the `check`
/// proves the wrapped error nil and its value readable.
#[test]
fn checking_a_wrapped_error_cleans_the_value() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn wrap(e: error, c: str) -> error {\n  return e\n}\nfn f() -> (int, error) {\n  let (v, err) = load()\n  check wrap(err, \"while loading\")\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// Only the error arguments count: a call that happens to return an error
/// without being handed one proves nothing about an unrelated error.
#[test]
fn checking_a_call_without_the_error_does_not_clean_the_value() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn other(n: int) -> error {\n  return nil\n}\nfn f() -> (int, error) {\n  let (v, err) = load()\n  check other(3)\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(c.has("E0301"), "{}", c.render());
    assert!(c.has("E0302"), "{}", c.render());
}

/// A loop body may run zero times, so a `check` inside it proves nothing about
/// the state after the loop.
#[test]
fn a_check_inside_a_loop_does_not_clean_the_value_after_it() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn f() -> (int, error) {\n  let (v, err) = load()\n  for false {\n    check err\n  }\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(c.has("E0301"), "{}", c.render());
    assert!(c.has("E0302"), "{}", c.render());
}

/// Arms are alternatives: checking in one says nothing about the others.
#[test]
fn a_check_in_one_match_arm_does_not_clean_the_others() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn f(k: int) -> (int, error) {\n  let (v, err) = load()\n  match k {\n    0 => { check err }\n    _ => { }\n  }\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(c.has("E0301"), "{}", c.render());
}

/// Checking in *every* arm does prove it, on every path.
#[test]
fn a_check_in_every_match_arm_cleans_the_value() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn f(k: int) -> (int, error) {\n  let (v, err) = load()\n  match k {\n    0 => { check err }\n    _ => { check err }\n  }\n  return v, nil\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// An error bound inside a branch still has to be inspected: the join has to
/// carry the branch's state out, not just the state the branch was entered in.
#[test]
fn an_error_bound_inside_a_branch_is_still_reported() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n  if true {\n    let (v, err) = load()\n  }\n}\n");
    assert!(c.has("E0302"), "{}", c.render());
}

#[test]
fn an_error_bound_inside_a_loop_is_still_reported() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n  for i in 0..3 {\n    let (v, err) = load()\n  }\n}\n");
    assert!(c.has("E0302"), "{}", c.render());
}

/// An `error` is nil-able, so its message needs an error that is there. The
/// backends cannot even agree what a nil receiver does — the VM answers with an
/// empty string, Wasm traps — so the call is rejected until control flow proves
/// the error present.
#[test]
fn reading_the_message_of_a_possibly_nil_error_is_rejected() {
    let c = body("  let err: error = nil\n  io.print(err.message())");
    assert!(c.has("E0301"), "{}", c.render());
}

#[test]
fn a_message_read_inside_a_non_nil_test_is_accepted() {
    let c = with_fallible("  let (v, err) = load()\n  if err != nil {\n    io.print(err.message())\n  }");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The shape `std/errors.kite` is written in.
#[test]
fn a_message_read_after_a_nil_guard_clause_is_accepted() {
    let c = run("fn describe(err: error) -> str {\n  if err == nil {\n    return \"none\"\n  }\n  return err.message()\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The proof does not outlive the branch that established it.
#[test]
fn a_message_read_after_the_test_closes_is_rejected() {
    let c = with_fallible("  let (v, err) = load()\n  if err != nil {\n    io.print(1)\n  }\n  io.print(err.message())");
    assert!(c.has("E0301"), "{}", c.render());
}

#[test]
fn check_outside_a_fallible_function_is_rejected() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n  let (v, err) = load()\n  check err\n}\n");
    assert!(c.has("E0303"), "{}", c.render());
}

#[test]
fn a_fallible_function_must_return_two_values() {
    let c = run("fn f() -> (int, error) {\n  return 1\n}\n");
    assert!(c.has("E0203"), "{}", c.render());
    assert!(c.render().contains("return value, nil"), "{}", c.render());
}

#[test]
fn destructuring_a_non_fallible_call_is_rejected() {
    let c = run("fn plain() -> int {\n  return 1\n}\nfn main() {\n  let (v, err) = plain()\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn nil_is_the_no_error_value() {
    let c = run("fn f() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn one_unchecked_error_yields_one_diagnostic() {
    let c = with_fallible("  let (v, err) = load()\n  io.print(1)\n  io.print(2)\n  io.print(3)");
    assert_eq!(c.diags.error_count(), 1, "{}", c.render());
}

/// The specification's one exception to same-scope shadowing: rebinding `err`
/// is what lets a function chain several fallible calls.
#[test]
fn rebinding_err_in_the_same_scope_is_permitted() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn chain() -> (int, error) {\n  let (a, err) = load()\n  check err\n  let (b, err) = load()\n  check err\n  return a + b, nil\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// Each `err` still gets its own slot, so an earlier one that was never checked
/// is still reported.
#[test]
fn a_shadowed_but_unchecked_error_is_still_reported() {
    let c = run("fn load() -> (int, error) {\n  return 1, nil\n}\nfn chain() -> (int, error) {\n  let (a, err) = load()\n  let (b, err) = load()\n  check err\n  return b, nil\n}\nfn main() {\n}\n");
    assert!(c.has("E0302"), "{}", c.render());
    // `a` is never read, so only the unchecked error is reported.
    assert_eq!(c.diags.error_count(), 1, "{}", c.render());
}

/// Rebinding a *value* in the same scope is still rejected; only the error slot
/// is exempt.
#[test]
fn rebinding_a_value_in_the_same_scope_is_still_rejected() {
    let c = body("  let x = 1\n  let x = 2");
    assert!(c.has("E0112"), "{}", c.render());
}

// ---- concurrency ----------------------------------------------------------

/// Calling an `async fn` starts it and yields the task. That is the whole of
/// how concurrency is expressed, so it is the first thing to pin down.
#[test]
fn calling_an_async_function_yields_a_task() {
    let c = run("async fn work() -> int {\n  return 1\n}\nfn main() {\n  let t = work()\n  let n: int = t\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
    assert!(c.render().contains("Task<int>"), "{}", c.render());
}

#[test]
fn await_outside_an_async_function_is_rejected() {
    let c = run("async fn work() -> int {\n  return 1\n}\nfn main() {\n  let n = await work()\n}\n");
    assert!(c.has("E0521"), "{}", c.render());
}

#[test]
fn awaiting_something_that_is_not_a_task_is_rejected() {
    let c = run("async fn main() {\n  let n = await 3\n}\n");
    assert!(c.has("E0200"), "{}", c.render());
}

#[test]
fn awaiting_a_task_gives_the_value_it_produces() {
    let c = run("async fn work() -> int {\n  return 1\n}\nasync fn main() {\n  let n: int = await work()\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// `task.yield` suspends, so it is subject to the same rule as `await`.
#[test]
fn yielding_outside_an_async_function_is_rejected() {
    let c = body("  task.yield()");
    assert!(c.has("E0521"), "{}", c.render());
}

/// The `Share` bound is structural: nobody implements it, and most types
/// satisfy it without their author knowing it exists.
#[test]
fn an_immutable_type_satisfies_the_share_bound() {
    let c = run(
        "trait Share {\n}\n\
         struct Order {\n  id: int\n  name: str\n}\n\
         fn send<T: Share>(value: T) {\n}\n\
         fn main() {\n  send(Order{id: 1, name: \"a\"})\n}\n",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_mutable_field_is_reported_where_share_is_required() {
    let c = run(
        "trait Share {\n}\n\
         struct Counter {\n  var count: int\n}\n\
         fn send<T: Share>(value: T) {\n}\n\
         fn main() {\n  send(Counter{count: 0})\n}\n",
    );
    assert!(c.has("E0520"), "{}", c.render());
    // The message has to name the field, not just the type: "not Share" is
    // not something a reader can act on.
    assert!(
        c.render().contains("because this field is mutable"),
        "{}",
        c.render()
    );
}

#[test]
fn share_is_transitive_through_a_field() {
    let c = run(
        "trait Share {\n}\n\
         struct Counter {\n  var count: int\n}\n\
         struct Holder {\n  c: Counter\n}\n\
         fn send<T: Share>(value: T) {\n}\n\
         fn main() {\n  send(Holder{c: Counter{count: 0}})\n}\n",
    );
    assert!(c.has("E0520"), "{}", c.render());
}

/// A `for { }` with no way out never falls through, so a function whose every
/// exit is a `return` inside one needs no unreachable return after it.
#[test]
fn an_unbroken_loop_diverges() {
    let c = run("fn spin() -> int {\n  for {\n    return 1\n  }\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_loop_that_breaks_still_needs_a_return_after_it() {
    let c = run("fn spin() -> int {\n  for {\n    break\n  }\n}\nfn main() {\n}\n");
    assert!(c.has("E0203"), "{}", c.render());
}

/// An unlabelled `break` belongs to the innermost loop, so it does not stop
/// the outer one from diverging.
#[test]
fn a_break_in_a_nested_loop_does_not_escape_the_outer_one() {
    let c = run("fn spin() -> int {\n  for {\n    for i in 0..3 {\n      break\n    }\n    return 1\n  }\n}\nfn main() {\n}\n");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

// ---- tuple bindings -------------------------------------------------------

#[test]
fn a_tuple_binding_takes_a_tuple_apart() {
    let c = body("  let (a, b) = (1, \"two\")\n  io.print(a)\n  io.print(b)");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_tuple_binding_of_the_wrong_width_is_reported() {
    let c = body("  let (a, b, c) = (1, 2)");
    assert!(c.has("E0200"), "{}", c.render());
}

// ---- defer ----------------------------------------------------------------

#[test]
fn defer_runs_at_every_exit() {
    let c = run(
        "fn close(what: str) {\n}\n\
         fn work(fail: bool) -> (int, error) {\n\
         \x20 defer close(\"a\")\n\
         \x20 if fail {\n    return _, errors.new(\"no\")\n  }\n\
         \x20 return 1, nil\n}\n\
         fn main() {\n}\n",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// `defer` takes a call. An expression that is not one has nothing to run.
#[test]
fn defer_needs_a_call() {
    let c = body("  defer 1 + 2");
    assert!(c.has("E0200"), "{}", c.render());
}

// ---- assert and require ---------------------------------------------------

#[test]
fn require_takes_a_condition_and_a_message() {
    let c = body("  require(1 == 1, \"always\")");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

#[test]
fn a_claim_must_be_a_bool() {
    let c = body("  require(1, \"not a bool\")");
    assert!(c.has("E0202"), "{}", c.render());
}

#[test]
fn a_claim_needs_a_message() {
    let c = body("  require(true)");
    assert!(c.has("E0113"), "{}", c.render());
}

// ---- exclusivity ----------------------------------------------------------

/// The prelude for these: a struct with a mutable field, and a function that
/// writes two of them.
fn two_writers(call: &str) -> Ctx {
    run(&format!(
        "struct Account {{\n  var balance: int\n}}\n\
         fn transfer(var from: Account, var to: Account, amount: int) {{\n\
         \x20 from.balance = from.balance - amount\n\
         \x20 to.balance = to.balance + amount\n}}\n\
         fn main() {{\n\
         \x20 let a = Account{{ balance: 100 }}\n\
         \x20 let b = Account{{ balance: 0 }}\n\
         {}\n}}\n",
        call
    ))
}

#[test]
fn one_object_under_two_var_parameters() {
    let c = two_writers("  transfer(a, a, 50)");
    assert!(c.has("E0800"), "{}", c.render());
}

#[test]
fn distinct_objects_are_fine() {
    let c = two_writers("  transfer(a, b, 50)");
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// The prefix case: writing through the outer object writes the inner one, so
/// passing both is the same aliasing spelled differently.
#[test]
fn a_field_and_its_owner_are_one_object() {
    let c = run(
        "struct Inner {\n  var n: int\n}\n\
         struct Outer {\n  inner: Inner\n  var tag: int\n}\n\
         fn bump(var i: Inner, var o: Outer) {\n\
         \x20 i.n = i.n + 1\n  o.tag = o.tag + i.n\n}\n\
         fn main() {\n\
         \x20 let o = Outer{ inner: Inner{ n: 1 }, tag: 0 }\n\
         \x20 bump(o.inner, o)\n}\n",
    );
    assert!(c.has("E0800"), "{}", c.render());
}

/// Two different fields are two different objects, whatever they were built
/// from. Seeing through the heap is what this pass does not do.
#[test]
fn sibling_fields_are_distinct() {
    let c = run(
        "struct Inner {\n  var n: int\n}\n\
         struct Pair {\n  left: Inner\n  right: Inner\n}\n\
         fn swap_in(var a: Inner, var b: Inner) {\n\
         \x20 a.n = b.n\n}\n\
         fn main() {\n\
         \x20 let p = Pair{ left: Inner{ n: 1 }, right: Inner{ n: 2 } }\n\
         \x20 swap_in(p.left, p.right)\n}\n",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// One `var` is enough: the read is a reference too, so the callee watches the
/// value change under it.
#[test]
fn a_read_alongside_a_write_is_reported() {
    let c = run(
        "struct Account {\n  var balance: int\n}\n\
         fn audit(var live: Account, snapshot: Account) {\n\
         \x20 live.balance = live.balance + snapshot.balance\n}\n\
         fn main() {\n\
         \x20 let a = Account{ balance: 100 }\n\
         \x20 audit(a, a)\n}\n",
    );
    assert!(c.has("E0800"), "{}", c.render());
}

/// Nothing is written, so two names for one object are two ways of reading it.
#[test]
fn two_reads_are_not_a_conflict() {
    let c = run(
        "struct Account {\n  balance: int\n}\n\
         fn total(x: Account, y: Account) -> int {\n\
         \x20 return x.balance + y.balance\n}\n\
         fn main() {\n\
         \x20 let a = Account{ balance: 100 }\n\
         \x20 let n = total(a, a)\n}\n",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// Slices are copy-on-write values, so a `var [T]` parameter is the callee's
/// own copy and two of them cannot interfere.
#[test]
fn slice_arguments_are_values() {
    let c = run(
        "fn grow(var xs: [int], var ys: [int]) {\n\
         \x20 xs.push(1)\n  ys.push(2)\n}\n\
         fn main() {\n\
         \x20 var xs: [int] = [1]\n\
         \x20 grow(xs, xs)\n}\n",
    );
    assert!(!c.diags.has_errors(), "{}", c.render());
}

/// A literal index distinguishes elements; an unknown one could be any of
/// them, so it is taken to overlap.
#[test]
fn indices_compare_when_they_are_known() {
    let src = |args: &str| {
        format!(
            "struct Account {{\n  var balance: int\n}}\n\
             fn transfer(var from: Account, var to: Account) {{\n\
             \x20 from.balance = to.balance\n}}\n\
             fn main() {{\n\
             \x20 let xs = [Account{{ balance: 1 }}, Account{{ balance: 2 }}]\n\
             \x20 let i = 0\n\
             \x20 let j = 1\n\
             \x20 transfer({})\n}}\n",
            args
        )
    };
    assert!(!run(&src("xs[0], xs[1]")).diags.has_errors());
    assert!(run(&src("xs[0], xs[0]")).has("E0800"));
    assert!(run(&src("xs[i], xs[j]")).has("E0800"));
}

/// A method's receiver is its first parameter, so `var self` is checked like
/// any other write.
#[test]
fn a_var_receiver_counts() {
    let c = run(
        "struct Account {\n  var balance: int\n}\n\
         impl Account {\n\
         \x20 fn drain_into(var self, var other: Account) {\n\
         \x20   other.balance = other.balance + self.balance\n\
         \x20   self.balance = 0\n  }\n}\n\
         fn main() {\n\
         \x20 var a = Account{ balance: 100 }\n\
         \x20 a.drain_into(a)\n}\n",
    );
    assert!(c.has("E0800"), "{}", c.render());
}

/// A virtual call reaches an implementation the compiler cannot name, so the
/// parameters checked are every implementation's at once.
#[test]
fn a_virtual_call_is_checked_through_the_vtable() {
    let c = run(
        "trait Sink {\n\
         \x20 fn drain(var self, other: dyn Sink)\n}\n\
         struct Bucket {\n  var level: int\n}\n\
         impl Sink for Bucket {\n\
         \x20 fn drain(var self, other: dyn Sink) {\n\
         \x20   self.level = 0\n  }\n}\n\
         fn empty(var s: dyn Sink) {\n\
         \x20 s.drain(s)\n}\n\
         fn main() {\n\
         \x20 var b = Bucket{ level: 1 }\n\
         \x20 empty(b)\n}\n",
    );
    assert!(c.has("E0800"), "{}", c.render());
}

// ---- type aliases ---------------------------------------------------------

/// An alias is interchangeable with what it names, so a local annotated with
/// one has the underlying type — not a distinct type, and not the error type.
/// Until this was fixed an alias resolved to `TyId::ERROR`, which suppressed
/// every later check and let the program compile to a unit value instead.
#[test]
fn an_alias_is_the_type_it_names() {
    let c = ok("type Id = int\ntype Name = str\nfn main() {\n  let a: Id = 1\n  let b: Name = \"x\"\n}\n");
    let locals = &c.program.fns[0].locals;
    assert_eq!(locals[0].ty, TyId::INT);
    assert_eq!(locals[1].ty, TyId::STR);
}

/// The aggregates are where the old bug showed as wrong output rather than a
/// missing diagnostic: indexing an aliased map answered `()`.
#[test]
fn an_alias_of_an_aggregate_keeps_its_operations() {
    let c = ok(
        "type Prices = {str: int}\n\
         type Names = [str]\n\
         fn main() {\n\
         \x20 let p: Prices = { \"kite\": 40 }\n\
         \x20 let n: Names = [\"a\"]\n\
         \x20 let plain: {str: int} = { \"kite\": 40 }\n\
         \x20 let flat: [str] = [\"a\"]\n\
         \x20 let first = n[0]\n}\n",
    );
    let locals = &c.program.fns[0].locals;
    assert_eq!(locals[0].ty, locals[2].ty, "an aliased map is not the map it names");
    assert_eq!(locals[1].ty, locals[3].ty, "an aliased slice is not the slice it names");
    assert_eq!(locals[4].ty, TyId::STR, "indexing through an alias lost the element type");
}

/// One alias may name another, and either may be written first.
#[test]
fn aliases_chain_and_may_be_declared_out_of_order() {
    let c = ok(
        "type Key = Id\n\
         type Id = int\n\
         struct Row {\n  id: Key\n}\n\
         fn main() {\n\
         \x20 let r = Row{ id: 1 }\n\
         \x20 let n: Id = r.id + 1\n}\n",
    );
    assert_eq!(c.program.fns[0].locals[1].ty, TyId::INT);
}

/// A cycle has no underlying type, and following it would not terminate.
#[test]
fn a_circular_alias_is_rejected() {
    let c = run("type A = B\ntype B = A\nfn main() {\n  let x: A = 1\n}\n");
    assert!(c.has("E0214"), "{}", c.render());
}

#[test]
fn an_alias_naming_itself_is_rejected() {
    let c = run("type A = A\nfn main() {\n  let x: A = 1\n}\n");
    assert!(c.has("E0214"), "{}", c.render());
}

/// Substituting arguments through an alias is a second instantiation path,
/// and the language has one. Rejecting it beats compiling it wrongly.
#[test]
fn a_generic_alias_is_rejected() {
    let c = run("type Pair<T> = (T, T)\nfn main() {\n  io.print(1)\n}\n");
    assert!(c.has("E0214"), "{}", c.render());
}
