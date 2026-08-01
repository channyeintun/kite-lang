use super::*;
use kite_span::SourceMap;

struct Ctx {
    map: ResolveMap,
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
    let map = resolve(&ast, &mut diags);
    Ctx { map, diags, sources }
}

fn ok(src: &str) -> Ctx {
    let c = run(src);
    assert!(!c.diags.has_errors(), "unexpected diagnostics:\n{}", c.render());
    c
}

#[test]
fn collects_function_signatures() {
    let c = ok("fn a() {\n}\nfn b(x: int) {\n}\n");
    assert_eq!(c.map.fns.len(), 2);
    assert_eq!(c.map.fns[0].name, "a");
    assert_eq!(c.map.fns[1].param_count, 1);
}

/// Signatures are collected before any body is resolved, so declaration order
/// does not constrain call order.
#[test]
fn a_call_may_precede_the_declaration() {
    ok("fn main() {\n    later()\n}\nfn later() {\n}\n");
}

#[test]
fn parameters_become_locals_zero_upward() {
    let c = ok("fn f(a: int, b: int) {\n    let z = a\n}\n");
    let locals = &c.map.locals[0];
    assert_eq!(locals[0].name, "a");
    assert_eq!(locals[1].name, "b");
    assert_eq!(locals[2].name, "z");
}

#[test]
fn var_is_mutable_and_let_is_not() {
    let c = ok("fn f() {\n    let a = 1\n    var b = 2\n}\n");
    let locals = &c.map.locals[0];
    assert!(!locals[0].mutable, "let must be immutable");
    assert!(locals[1].mutable, "var must be mutable");
}

#[test]
fn shadowing_in_a_nested_scope_is_allowed() {
    ok("fn f() {\n    let x = 1\n    if true {\n        let x = 2\n    }\n}\n");
}

#[test]
fn shadowing_in_the_same_scope_is_rejected() {
    let c = run("fn f() {\n    let x = 1\n    let x = 2\n}\n");
    assert!(c.codes().contains(&"E0112"), "{}", c.render());
    assert!(c.render().contains("first declared here"), "{}", c.render());
}

#[test]
fn duplicate_function_names_are_rejected() {
    let c = run("fn f() {\n}\nfn f() {\n}\n");
    assert!(c.codes().contains(&"E0112"), "{}", c.render());
    assert!(c.render().contains("no function overloading"), "{}", c.render());
}

/// `let x = x` must see the *outer* `x`, because the initialiser is resolved
/// before the new binding enters scope.
#[test]
fn initialiser_sees_the_outer_binding() {
    let c = ok("fn f() {\n    let x = 1\n    if true {\n        let x = x\n    }\n}\n");
    let locals = &c.map.locals[0];
    assert_eq!(locals.len(), 2);
}

#[test]
fn unknown_names_are_reported() {
    let c = run("fn f() {\n    let a = nope\n}\n");
    assert!(c.codes().contains(&"E0111"), "{}", c.render());
}

#[test]
fn a_typo_suggests_the_nearest_name() {
    let c = run("fn f() {\n    let total = 1\n    let x = totl\n}\n");
    assert!(c.render().contains("`total`"), "{}", c.render());
}

#[test]
fn io_print_resolves_to_a_builtin() {
    let c = ok("fn f() {\n    io.print(1)\n}\n");
    let found = c
        .map
        .uses
        .values()
        .any(|r| *r == Res::Builtin(BuiltinFn::IoPrint));
    assert!(found, "io.print did not resolve to a builtin");
}

#[test]
fn an_unknown_dotted_path_is_reported() {
    let c = run("fn f() {\n    other.thing(1)\n}\n");
    assert!(c.codes().contains(&"E0111"), "{}", c.render());
}

#[test]
fn a_loop_variable_is_visible_in_the_body() {
    ok("fn f() {\n    for i in 0..10 {\n        io.print(i)\n    }\n}\n");
}

#[test]
fn a_loop_variable_is_not_visible_after_the_loop() {
    let c = run("fn f() {\n    for i in 0..10 {\n    }\n    io.print(i)\n}\n");
    assert!(c.codes().contains(&"E0111"), "{}", c.render());
}

#[test]
fn break_outside_a_loop_is_rejected() {
    let c = run("fn f() {\n    break\n}\n");
    assert!(c.codes().contains(&"E0115"), "{}", c.render());
}

#[test]
fn break_inside_a_loop_is_accepted() {
    ok("fn f() {\n    for {\n        break\n    }\n}\n");
}

#[test]
fn a_known_loop_label_resolves() {
    ok("fn f() {\n    outer: for i in 0..3 {\n        for j in 0..3 {\n            continue outer\n        }\n    }\n}\n");
}

#[test]
fn an_unknown_loop_label_is_rejected() {
    let c = run("fn f() {\n    for i in 0..3 {\n        break nope\n    }\n}\n");
    assert!(c.codes().contains(&"E0111"), "{}", c.render());
}

#[test]
fn closure_parameters_scope_to_the_closure() {
    let c = run("fn f() {\n    let g = |a| a\n    let b = a\n}\n");
    assert!(c.codes().contains(&"E0111"), "{}", c.render());
}

#[test]
fn edit_distance_is_correct() {
    assert_eq!(edit_distance("", ""), 0);
    assert_eq!(edit_distance("abc", "abc"), 0);
    assert_eq!(edit_distance("totl", "total"), 1);
    assert_eq!(edit_distance("kitten", "sitting"), 3);
    assert_eq!(edit_distance("", "abc"), 3);
}
