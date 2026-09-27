use super::*;
use kite_span::SourceMap;

struct Parsed {
    file: SourceFile,
    diags: DiagBag,
    map: SourceMap,
}

impl Parsed {
    fn codes(&self) -> Vec<&'static str> {
        self.diags.iter().filter_map(|d| d.code.map(|c| c.0)).collect()
    }

    fn render(&self) -> String {
        self.diags.render_all(&self.map)
    }

    fn fns(&self) -> Vec<&FnDecl> {
        self.file
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some(f),
                _ => None,
            })
            .collect()
    }
}

fn parse_src(src: &str) -> Parsed {
    let mut map = SourceMap::new();
    let f = map.add("t.kite", src);
    let mut diags = DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let file = parse(f, src, &tokens, &mut diags);
    Parsed { file, diags, map }
}

fn ok(src: &str) -> Parsed {
    let p = parse_src(src);
    assert!(!p.diags.has_errors(), "unexpected diagnostics:\n{}", p.render());
    p
}

/// Render an expression as a fully parenthesised s-expression, so precedence
/// and associativity assertions are unambiguous.
fn sexp(e: &Expr, src: &str) -> String {
    let text = |s: Span| src[s.start as usize..s.end as usize].to_string();
    match e {
        Expr::Int(s) | Expr::Float(s) | Expr::Str(s) | Expr::Char(s) => text(*s),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::ImpliedInt { value, .. } => format!("<{}>", value),
        Expr::Interpolated { parts, .. } => {
            let rendered: Vec<String> = parts
                .iter()
                .map(|p| match p {
                    kite_ast::StrPart::Text(s) => format!("{:?}", text(*s)),
                    kite_ast::StrPart::Hole(e) => sexp(e, src),
                })
                .collect();
            format!("(str {})", rendered.join(" "))
        }
        Expr::Nil(_) => "nil".into(),
        Expr::Path(p) => p.text(),
        Expr::SelfExpr(_) => "self".into(),
        Expr::Unary { op, operand, .. } => format!("({} {})", op.text(), sexp(operand, src)),
        Expr::Binary { op, lhs, rhs, .. } => {
            format!("({} {} {})", op.text(), sexp(lhs, src), sexp(rhs, src))
        }
        Expr::Call { callee, args, .. } => {
            let a: Vec<_> = args.iter().map(|x| sexp(x, src)).collect();
            format!("(call {} {})", sexp(callee, src), a.join(" "))
        }
        Expr::Field { base, name, .. } => {
            format!("(. {} {})", sexp(base, src), name.name)
        }
        Expr::Index { base, index, .. } => {
            format!("(index {} {})", sexp(base, src), sexp(index, src))
        }
        Expr::Range { start, end, inclusive, .. } => format!(
            "({} {} {})",
            if *inclusive { "..=" } else { ".." },
            sexp(start, src),
            sexp(end, src)
        ),
        Expr::Cast { expr, .. } => format!("(as {})", sexp(expr, src)),
        Expr::Await { expr, .. } => format!("(await {})", sexp(expr, src)),
        Expr::Paren { inner, .. } => sexp(inner, src),
        Expr::Tuple { elems, .. } => {
            let a: Vec<_> = elems.iter().map(|x| sexp(x, src)).collect();
            format!("(tuple {})", a.join(" "))
        }
        Expr::Slice { elems, .. } => {
            let a: Vec<_> = elems.iter().map(|x| sexp(x, src)).collect();
            format!("(slice {})", a.join(" "))
        }
        Expr::Closure { .. } => "(closure)".into(),
        Expr::If { .. } => "(if)".into(),
        Expr::Map { entries, .. } => {
            let a: Vec<_> = entries
                .iter()
                .map(|e| format!("{}: {}", sexp(&e.key, src), sexp(&e.value, src)))
                .collect();
            format!("(map {})", a.join(" "))
        }
        Expr::StructLit(s) => {
            let a: Vec<_> = s
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name.name, sexp(&f.value, src)))
                .collect();
            let base = match &s.base {
                Some(b) => format!("..{} ", sexp(b, src)),
                None => String::new(),
            };
            format!("({}{{{}{}}})", s.path.name(), base, a.join(" "))
        }
        Expr::Match(m) => format!("(match {} {} arms)", sexp(&m.scrutinee, src), m.arms.len()),
        Expr::Error(_) => "(error)".into(),
    }
}

/// Parse `expr_src` as the initialiser of a `let` and return its s-expression.
fn expr_sexp(expr_src: &str) -> String {
    let src = format!("fn f() {{\n    let x = {}\n}}\n", expr_src);
    let p = ok(&src);
    let fns = p.fns();
    match &fns[0].body.stmts[0] {
        Stmt::Let(l) => sexp(l.init.as_ref().expect("initialiser"), &src),
        other => panic!("expected a let, got {:?}", other),
    }
}

// ---- declarations ---------------------------------------------------------

#[test]
fn parses_a_function_signature() {
    let p = ok("fn add(a: int, b: int) -> int {\n    return a + b\n}\n");
    let fns = p.fns();
    let f = fns[0];
    assert_eq!(f.name.name, "add");
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name.name, "a");
    assert!(!f.is_pub);
    assert!(!f.is_async);
    assert!(matches!(f.ret, Some(RetType::Simple(_))));
}

#[test]
fn parses_pub_and_async_modifiers() {
    let p = ok("pub async fn f() {\n}\n");
    let fns = p.fns();
    assert!(fns[0].is_pub);
    assert!(fns[0].is_async);
    assert!(fns[0].ret.is_none());
}

#[test]
fn parses_fallible_return_type() {
    let p = ok("fn load(p: str) -> (Config, error) {\n}\n");
    let fns = p.fns();
    assert!(fns[0].ret.as_ref().unwrap().is_fallible());
}

/// `(A, B)` is an ordinary tuple return; only a trailing `error` makes it
/// fallible.
#[test]
fn tuple_return_is_not_fallible() {
    let p = ok("fn f() -> (int, str) {\n}\n");
    let fns = p.fns();
    assert!(!fns[0].ret.as_ref().unwrap().is_fallible());
}

#[test]
fn parses_use_declarations() {
    let p = ok("use std/http\nuse std/json as j\n\nfn main() {\n}\n");
    assert_eq!(p.file.uses.len(), 2);
    assert_eq!(p.file.uses[0].path.len(), 2);
    assert_eq!(p.file.uses[1].alias.as_ref().unwrap().name, "j");
}

#[test]
fn parses_multiline_parameter_list_without_trailing_comma() {
    ok("fn f(\n    a: int,\n    b: int\n) -> int {\n    return a\n}\n");
}

// ---- precedence -----------------------------------------------------------

#[test]
fn arithmetic_precedence() {
    assert_eq!(expr_sexp("1 + 2 * 3"), "(+ 1 (* 2 3))");
    assert_eq!(expr_sexp("1 * 2 + 3"), "(+ (* 1 2) 3)");
    assert_eq!(expr_sexp("1 - 2 - 3"), "(- (- 1 2) 3)");
}

/// The documented departure from C.
#[test]
fn bitwise_binds_tighter_than_comparison() {
    assert_eq!(expr_sexp("a & b == c"), "(== (& a b) c)");
    assert_eq!(expr_sexp("a | b != c"), "(!= (| a b) c)");
}

#[test]
fn logical_precedence() {
    assert_eq!(expr_sexp("a || b && c"), "(|| a (&& b c))");
    assert_eq!(expr_sexp("a && b || c"), "(|| (&& a b) c)");
}

/// `Option<T>` is spelled as a word. Kite has no `?` sigil anywhere.
#[test]
fn optional_types_are_spelled_as_a_word() {
    let p = ok("fn f(a: Option<int>) -> Option<str> {\n}\n");
    let fns = p.fns();
    assert!(matches!(fns[0].params[0].ty, Type::Optional { .. }));
}

#[test]
fn range_is_loosest() {
    assert_eq!(expr_sexp("0..n + 1"), "(.. 0 (+ n 1))");
    assert_eq!(expr_sexp("0..=n"), "(..= 0 n)");
}

#[test]
fn postfix_binds_tighter_than_prefix() {
    // The negation wraps the whole postfix chain, not just its head.
    assert_eq!(expr_sexp("-x.foo"), "(- (. x foo))");
    assert_eq!(expr_sexp("-f(a).b"), "(- (. (call f a) b))");
    assert_eq!(expr_sexp("!f(x)"), "(! (call f x))");
    assert_eq!(expr_sexp("-a[0]"), "(- (index a 0))");
}

/// `.` always produces a field access. Whether `io.print` is really a module
/// path rather than a field of a local named `io` is a resolution question, so
/// the parser does not try to answer it.
#[test]
fn dotted_names_are_field_accesses() {
    assert_eq!(expr_sexp("io.print"), "(. io print)");
    assert_eq!(expr_sexp("io.print(x)"), "(call (. io print) x)");
    assert_eq!(expr_sexp("a.b.c"), "(. (. a b) c)");
}

#[test]
fn await_applies_after_the_call() {
    assert_eq!(expr_sexp("await f(1)"), "(await (call f 1))");
}

#[test]
fn parenthesised_expressions_regroup() {
    assert_eq!(expr_sexp("(1 + 2) * 3"), "(* (+ 1 2) 3)");
}

#[test]
fn chained_comparison_is_rejected() {
    let p = parse_src("fn f() {\n    let x = a < b < c\n}\n");
    assert!(p.codes().contains(&"E0100"), "{}", p.render());
    assert!(p.render().contains("cannot be chained"), "{}", p.render());
}

/// What was reported is not handed on to be reported again: the checker would
/// otherwise go on to compare the first comparison's `bool` with `c`.
#[test]
fn a_chained_comparison_leaves_an_error_node() {
    let p = parse_src("fn f() {\n    let x = a < b < c\n}\n");
    let Stmt::Let(l) = &p.fns()[0].body.stmts[0] else { panic!("a let") };
    assert!(matches!(l.init, Some(Expr::Error(_))), "{:?}", l.init);
}

// ---- statements -----------------------------------------------------------

#[test]
fn parses_the_phase_one_program() {
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
    let p = ok(src);
    assert_eq!(p.fns().len(), 2);
    let fns = p.fns();
    let main = fns[1];
    assert_eq!(main.body.stmts.len(), 3);
    assert!(matches!(main.body.stmts[0], Stmt::Let(_)));
    assert!(matches!(main.body.stmts[1], Stmt::If(_)));
    assert!(matches!(main.body.stmts[2], Stmt::For(_)));
}

#[test]
fn parses_the_three_for_forms() {
    let p = ok("fn f() {\n  for x in xs {\n  }\n  for c {\n  }\n  for {\n  }\n}\n");
    let fns = p.fns();
    let stmts = &fns[0].body.stmts;
    assert!(matches!(&stmts[0], Stmt::For(f) if matches!(f.header, ForHeader::In { .. })));
    assert!(matches!(&stmts[1], Stmt::For(f) if matches!(f.header, ForHeader::While(_))));
    assert!(matches!(&stmts[2], Stmt::For(f) if matches!(f.header, ForHeader::Loop)));
}

#[test]
fn parses_labelled_loops() {
    let p = ok("fn f() {\n  outer: for x in xs {\n    continue outer\n  }\n}\n");
    let fns = p.fns();
    let Stmt::For(f) = &fns[0].body.stmts[0] else {
        panic!("expected a for")
    };
    assert_eq!(f.label.as_ref().unwrap().name, "outer");
    assert!(matches!(&f.body.stmts[0], Stmt::Continue { label: Some(l), .. } if l.name == "outer"));
}

#[test]
fn parses_else_if_chains() {
    let p = ok("fn f() {\n  if a {\n  } else if b {\n  } else {\n  }\n}\n");
    let fns = p.fns();
    let Stmt::If(i) = &fns[0].body.stmts[0] else {
        panic!("expected an if")
    };
    assert!(matches!(i.else_.as_deref(), Some(ElseBranch::If(_))));
}

#[test]
fn parses_tuple_binding_for_fallible_results() {
    let p = ok("fn f() {\n  let (v, err) = g()\n  check err\n}\n");
    let fns = p.fns();
    let stmts = &fns[0].body.stmts;
    let Stmt::Let(l) = &stmts[0] else { panic!() };
    let Binding::Tuple { elems, .. } = &l.binding else {
        panic!("expected a tuple binding")
    };
    assert_eq!(elems.len(), 2);
    assert!(matches!(&stmts[1], Stmt::Check { .. }));
}

#[test]
fn parses_the_three_return_forms() {
    let p =
        ok("fn f() {\n  return\n}\nfn g() {\n  return v, nil\n}\nfn h() {\n  return _, err\n}\n");
    let fns = p.fns();
    let get = |f: &FnDecl| match &f.body.stmts[0] {
        Stmt::Return(r) => match &r.value {
            None => "none",
            Some(ReturnValue::Single(_)) => "single",
            Some(ReturnValue::Pair { .. }) => "pair",
            Some(ReturnValue::Fail { .. }) => "fail",
        },
        _ => panic!("expected a return"),
    };
    assert_eq!(get(fns[0]), "none");
    assert_eq!(get(fns[1]), "pair");
    assert_eq!(get(fns[2]), "fail");
}

#[test]
fn parses_compound_assignment() {
    let p = ok("fn f() {\n  var n = 0\n  n += 1\n}\n");
    let fns = p.fns();
    let Stmt::Assign(a) = &fns[0].body.stmts[1] else {
        panic!("expected an assignment")
    };
    assert_eq!(a.op, AssignOp::Add);
}

#[test]
fn deferred_let_initialisation_parses() {
    ok("fn f() {\n  let z: int\n  if c {\n    z = 1\n  } else {\n    z = 2\n  }\n}\n");
}

#[test]
fn var_without_initialiser_is_rejected() {
    let p = parse_src("fn f() {\n  var n: int\n}\n");
    assert!(p.codes().contains(&"E0110"), "{}", p.render());
}

#[test]
fn assigning_to_a_non_place_is_rejected() {
    let p = parse_src("fn f() {\n  f(x) = 1\n}\n");
    assert!(p.codes().contains(&"E0114"), "{}", p.render());
}

// ---- recovery -------------------------------------------------------------

/// The specification's requirement: one diagnostic per cause. A single missing
/// brace must not produce a cascade.
#[test]
fn missing_closing_brace_produces_one_error() {
    let p = parse_src("fn main() {\n    let x = 1\n    let y = 2\n");
    assert_eq!(
        p.diags.error_count(),
        1,
        "expected exactly one diagnostic, got:\n{}",
        p.render()
    );
    assert!(p.codes().contains(&"E0101"), "{}", p.render());
}

#[test]
fn a_bad_statement_does_not_stop_later_functions() {
    let p = parse_src("fn a() {\n    let = = =\n}\n\nfn b() {\n    let x = 1\n}\n");
    let names: Vec<_> = p.fns().iter().map(|f| f.name.name.clone()).collect();
    assert!(names.contains(&"b".to_string()), "{:?}\n{}", names, p.render());
}

#[test]
fn one_bad_token_yields_one_error_not_a_cascade() {
    let p = parse_src("fn f() {\n    let x = @\n    let y = 2\n    let z = 3\n}\n");
    assert!(
        p.diags.error_count() <= 2,
        "cascade of {} errors:\n{}",
        p.diags.error_count(),
        p.render()
    );
}

#[test]
fn parser_terminates_on_pathological_input() {
    // Regression guard for the forward-progress assertions in the item and
    // block loops: without them these hang rather than fail.
    for src in ["}", "{", ")", "fn", "fn f(", "fn f() {", "let", "@@@@", ""] {
        let _ = parse_src(src);
    }
}

#[test]
fn error_points_where_the_missing_text_goes() {
    let p = parse_src("fn f() {\n    let x =\n}\n");
    let out = p.render();
    assert!(out.contains("expected an expression"), "{}", out);
}

// ---- Phase 2 declarations -------------------------------------------------

#[test]
fn parses_a_struct_with_pub_and_var_fields() {
    let p = ok("pub struct Rect {\n    pub width: float\n    height: float\n    pub var label: str\n}\n");
    let Item::Struct(s) = &p.file.items[0] else {
        panic!("expected a struct")
    };
    assert!(s.is_pub);
    assert_eq!(s.fields.len(), 3);
    assert!(s.fields[0].is_pub && !s.fields[0].is_var);
    assert!(!s.fields[1].is_pub);
    assert!(s.fields[2].is_var, "`var` field not recorded");
}

#[test]
fn parses_enum_variant_payload_forms() {
    let p = ok("enum Shape {\n    Circle(radius: float)\n    Rect(float, float)\n    Point\n}\n");
    let Item::Enum(e) = &p.file.items[0] else {
        panic!("expected an enum")
    };
    assert_eq!(e.variants.len(), 3);
    assert!(matches!(e.variants[0].payload, VariantPayload::Named(_)));
    assert!(matches!(e.variants[1].payload, VariantPayload::Positional(_)));
    assert!(matches!(e.variants[2].payload, VariantPayload::Unit));
}

#[test]
fn parses_a_trait_with_a_default_method() {
    let p = ok("pub trait Display {\n    fn show(self) -> str\n    fn label(self) -> str {\n        return \"x\"\n    }\n}\n");
    let Item::Trait(tr) = &p.file.items[0] else {
        panic!("expected a trait")
    };
    assert_eq!(tr.methods.len(), 2);
    assert!(tr.methods[0].body.is_none(), "declaration-only method");
    assert!(tr.methods[1].body.is_some(), "default method");
}

#[test]
fn parses_inherent_and_trait_impls() {
    let p = ok("impl Rect {\n    fn area(self) -> float {\n        return 1.0\n    }\n}\nimpl Display for Rect {\n    fn show(self) -> str {\n        return \"r\"\n    }\n}\n");
    let Item::Impl(a) = &p.file.items[0] else { panic!() };
    let Item::Impl(b) = &p.file.items[1] else { panic!() };
    assert!(a.trait_path.is_none());
    assert_eq!(b.trait_path.as_ref().unwrap().name(), "Display");
    assert_eq!(b.self_ty.name(), "Rect");
}

#[test]
fn parses_self_receivers() {
    let p = ok("impl R {\n    fn a(self) {\n    }\n    fn b(var self) {\n    }\n    fn c(x: int) {\n    }\n}\n");
    let Item::Impl(i) = &p.file.items[0] else { panic!() };
    assert!(i.methods[0].self_param.as_ref().is_some_and(|s| !s.is_var));
    assert!(i.methods[1].self_param.as_ref().is_some_and(|s| s.is_var));
    assert!(i.methods[2].self_param.is_none(), "associated function");
}

#[test]
fn parses_generic_parameters_with_bounds() {
    let p = ok("struct Cache<K: Hash, V> {\n    n: int\n}\n");
    let Item::Struct(s) = &p.file.items[0] else { panic!() };
    assert_eq!(s.generics.len(), 2);
    assert_eq!(s.generics[0].bounds.len(), 1);
    assert!(s.generics[1].bounds.is_empty());
}

// ---- struct literals ------------------------------------------------------

#[test]
fn parses_struct_literals_including_functional_update() {
    assert_eq!(expr_sexp("Point{ x: 1, y: 2 }"), "(Point{x: 1 y: 2})");
    assert_eq!(expr_sexp("Point{ ..p, y: 5 }"), "(Point{..p y: 5})");
}

#[test]
fn struct_literal_shorthand_repeats_the_name() {
    assert_eq!(expr_sexp("Point{ x, y }"), "(Point{x: x y: y})");
}

/// The specification's parsing note: a struct literal is not permitted in an
/// `if`/`for`/`match` scrutinee, where `{` opens the body.
#[test]
fn a_brace_in_a_condition_opens_the_body_not_a_literal() {
    let p = ok("fn f() {\n    if p {\n        io.print(1)\n    }\n}\n");
    let fns = p.fns();
    assert!(matches!(fns[0].body.stmts[0], Stmt::If(_)));
}

#[test]
fn a_parenthesised_struct_literal_works_in_a_condition() {
    ok("fn f() {\n    if (Point{ x: 1 }) == p {\n    }\n}\n");
}

// ---- match ----------------------------------------------------------------

#[test]
fn parses_match_with_guards_and_alternation() {
    let p = ok("fn f() {\n    match n {\n        0 => io.print(1),\n        1 | 2 => io.print(2),\n        x if x > 9 => io.print(3),\n        _ => io.print(4),\n    }\n}\n");
    let fns = p.fns();
    let Stmt::Match(m) = &fns[0].body.stmts[0] else {
        panic!("expected a match")
    };
    assert_eq!(m.arms.len(), 4);
    assert!(matches!(m.arms[1].pattern, Pattern::Or { .. }));
    assert!(m.arms[2].guard.is_some());
    assert!(matches!(m.arms[3].pattern, Pattern::Wildcard(_)));
}

#[test]
fn parses_pattern_forms() {
    let p = ok("fn f() {\n    match v {\n        Circle(r) => a(),\n        Rect(w: x, h: y) => b(),\n        Point{ x: 0, y } => c(),\n        (a, b) => d(),\n        4..=9 => e(),\n        nil => g(),\n        -1 => h(),\n    }\n}\n");
    let fns = p.fns();
    let Stmt::Match(m) = &fns[0].body.stmts[0] else { panic!() };
    assert!(matches!(&m.arms[0].pattern, Pattern::Variant { args: PatternArgs::Positional(_), .. }));
    assert!(matches!(&m.arms[1].pattern, Pattern::Variant { args: PatternArgs::Named(_), .. }));
    assert!(matches!(m.arms[2].pattern, Pattern::Struct { .. }));
    assert!(matches!(m.arms[3].pattern, Pattern::Tuple { .. }));
    assert!(matches!(m.arms[4].pattern, Pattern::Range { inclusive: true, .. }));
    assert!(matches!(m.arms[5].pattern, Pattern::Nil(_)));
    assert!(matches!(m.arms[6].pattern, Pattern::Literal(_)));
}

#[test]
fn match_arms_may_use_blocks_and_omit_trailing_commas() {
    ok("fn f() {\n    match n {\n        0 => {\n            io.print(1)\n        }\n        _ => io.print(2)\n    }\n}\n");
}

#[test]
fn parses_match_as_an_expression() {
    let p = ok("fn f() {\n    let d = match s {\n        0 => \"zero\",\n        _ => \"other\",\n    }\n}\n");
    let fns = p.fns();
    let Stmt::Let(l) = &fns[0].body.stmts[0] else { panic!() };
    assert!(matches!(l.init, Some(Expr::Match(_))));
}

#[test]
fn parses_map_literals() {
    assert_eq!(expr_sexp("{\"a\": 1, \"b\": 2}"), "(map \"a\": 1 \"b\": 2)");
}

/// The parser splits an interpolated literal, so nothing downstream re-scans
/// the text. A literal with no hole stays a plain literal.
#[test]
fn interpolation_is_split_at_parse_time() {
    assert_eq!(expr_sexp(r#""a\(x)b""#), r#"(str "a" x "b")"#);
    assert_eq!(expr_sexp(r#""\(x)""#), "(str x)");
    assert_eq!(expr_sexp(r#""\(a + b) tail""#), r#"(str (+ a b) " tail")"#);
    assert_eq!(expr_sexp(r#""\(f(1, 2))""#), "(str (call f 1 2))");
    // Adjacent holes leave no text between them.
    assert_eq!(expr_sexp(r#""\(a)\(b)""#), "(str a b)");
    // A literal with no hole is untouched.
    assert_eq!(expr_sexp(r#""plain""#), r#""plain""#);
    // `\\(` is an escaped backslash, not the start of a hole.
    assert_eq!(expr_sexp(r#""x\\\\(y)""#), r#""x\\\\(y)""#);
}

/// A nested string inside a hole may contain parens without ending the hole.
#[test]
fn a_hole_may_contain_a_string_with_parens() {
    assert_eq!(expr_sexp(r#""\(f(")("))""#), r#"(str (call f ")("))"#);
}

/// Section 2.1: identifiers are compared after NFC normalisation, so two
/// spellings a reader cannot tell apart are one name. `café` here is written
/// once with U+00E9 and once with `e` followed by the combining acute U+0301.
#[test]
fn identifiers_are_normalised_to_nfc() {
    let p = ok("fn main() {\n  let caf\u{e9} = 1\n  io.print(cafe\u{301})\n}\n");
    let f = p.fns()[0];
    let names: Vec<String> = format!("{:?}", f.body)
        .split('"')
        .filter(|s| s.contains("caf"))
        .map(str::to_string)
        .collect();
    assert!(!names.is_empty(), "expected the identifier in the tree");
    for n in &names {
        assert_eq!(n, "caf\u{e9}", "identifier was not normalised: {:?}", n);
    }
}

/// ASCII is already NFC, and is the overwhelming majority of identifiers.
#[test]
fn ascii_identifiers_are_left_alone() {
    assert!(matches!(normalise("total"), std::borrow::Cow::Borrowed(_)));
    assert!(matches!(normalise("caf\u{e9}"), std::borrow::Cow::Borrowed(_)));
    assert!(matches!(normalise("cafe\u{301}"), std::borrow::Cow::Owned(_)));
}

// ---- module-level constants ------------------------------------------------

#[test]
fn a_module_level_let_parses_as_a_constant() {
    let p = ok("pub let LIMIT: int = 40\nlet NAME = \"kite\"\n");
    let consts: Vec<&ConstDecl> = p
        .file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Const(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(consts.len(), 2);
    assert_eq!(consts[0].name.name, "LIMIT");
    assert!(consts[0].is_pub);
    assert!(consts[0].ty.is_some());
    assert_eq!(consts[1].name.name, "NAME");
    assert!(!consts[1].is_pub);
    assert!(consts[1].ty.is_none());
}

/// The declaration is still built, so everything after it resolves against a
/// name that exists. One diagnostic about the `var`, not a cascade about a
/// missing `COUNT`.
#[test]
fn a_module_level_var_is_refused_but_still_declares() {
    let p = parse_src("var COUNT = 0\n");
    assert_eq!(p.codes(), vec!["E0118"]);
    assert!(matches!(p.file.items.first(), Some(Item::Const(c)) if c.name.name == "COUNT"));
}

#[test]
fn a_module_level_let_must_have_a_value() {
    let p = parse_src("let LIMIT: int\n");
    assert!(p.codes().contains(&"E0118"));
}

/// Only the code that names the rule, and the constant still exists.
#[test]
fn a_module_level_let_without_a_value_is_one_error_and_still_declares() {
    let p = parse_src("let LIMIT: int\n\nfn main() {\n}\n");
    assert_eq!(p.codes(), vec!["E0118"], "{}", p.render());
    assert!(matches!(p.file.items.first(), Some(Item::Const(c)) if c.name.name == "LIMIT"));
}

/// `var x: int` is E0110 and nothing else: the binding is still declared, so
/// the lines that use it do not each report an unknown name.
#[test]
fn a_var_without_a_value_is_one_error_and_still_declares() {
    let p = parse_src("fn f() {\n    var x: int\n    x = 1\n}\n");
    assert_eq!(p.codes(), vec!["E0110"], "{}", p.render());
    assert!(matches!(&p.fns()[0].body.stmts[0], Stmt::Var(v) if v.name.name == "x"));
}

// ---- the grammar's corners --------------------------------------------------

/// §5.1 level 7: `&`, `^` and `|` are one left-associative level, so they
/// group in the order they are written. The parser once layered them the way
/// C does, and `a | b & c` meant something else here than in the
/// specification.
#[test]
fn the_bitwise_operators_share_one_level() {
    assert_eq!(expr_sexp("a | b & c"), "(& (| a b) c)");
    assert_eq!(expr_sexp("1 | 6 ^ 3"), "(^ (| 1 6) 3)");
    assert_eq!(expr_sexp("a & b ^ c | d"), "(| (^ (& a b) c) d)");
    // Still tighter than comparison and looser than a shift.
    assert_eq!(expr_sexp("a | b == c"), "(== (| a b) c)");
    assert_eq!(expr_sexp("a & b << c"), "(& a (<< b c))");
}

/// `as` converts between `int` and `float`, so the name after it has no type
/// arguments, and a `<` after it is a comparison.
#[test]
fn a_comparison_may_follow_a_cast() {
    assert_eq!(expr_sexp("f as int < n"), "(< (as f) n)");
    assert_eq!(expr_sexp("n as float > 2.0"), "(> (as n) 2.0)");
    ok("fn f(x: float, n: int) {\n    if x as int < n {\n    }\n}\n");
}

/// `>=` straight after a type argument list is its `>` and an `=`, as `>>`
/// is two `>`.
#[test]
fn a_type_argument_list_may_end_against_an_equals_sign() {
    ok("fn f() {\n    let a: Option<int>= nil\n    let b: Option<Option<int>>= nil\n}\n");
    ok("fn f() {\n    let b: [Option<int>]= []\n}\n");
}

/// `t.0.1` is two tuple indexes.
#[test]
fn a_tuple_index_may_follow_a_tuple_index() {
    assert_eq!(expr_sexp("t.0.1"), "(. (. t 0) 1)");
    assert_eq!(expr_sexp("t.1.0.2"), "(. (. (. t 1) 0) 2)");
}

/// A line ending in a binary operator continues onto the next, `>` and `>>`
/// included — while the `>` that closes a type argument list still ends its
/// line, and a bare `return` ends its own.
#[test]
fn a_line_ending_in_a_binary_operator_continues() {
    assert_eq!(expr_sexp("a >\n        b"), "(> a b)");
    assert_eq!(expr_sexp("a >>\n        b"), "(>> a b)");
    let p = ok("struct S {\n    a: Option<int>\n    b: Map<str, int>\n    c: int\n}\n");
    let Item::Struct(s) = &p.file.items[0] else { panic!() };
    assert_eq!(s.fields.len(), 3);
    let p = ok("fn f() {\n    return\n    g()\n}\n");
    let stmts = &p.fns()[0].body.stmts;
    assert_eq!(stmts.len(), 2, "{:?}", stmts);
    assert!(matches!(&stmts[0], Stmt::Return(r) if r.value.is_none()));
}

/// `a..b..c` has no meaning to give, and says so once.
#[test]
fn ranges_do_not_chain() {
    let p = parse_src("fn f() {\n    let r = 0..3..5\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert!(p.render().contains("ranges cannot be chained"), "{}", p.render());
    let p = parse_src("fn f() {\n    let r = xs[0..3..5]\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
}

/// A range index may leave out either end: the parser supplies the bound a
/// window clamps to anyway.
#[test]
fn a_range_index_may_leave_out_either_end() {
    let max = i64::MAX;
    assert_eq!(expr_sexp("xs[2..]"), format!("(index xs (.. 2 <{}>))", max));
    assert_eq!(expr_sexp("xs[..2]"), "(index xs (.. <0> 2))");
    assert_eq!(expr_sexp("xs[..]"), format!("(index xs (.. <0> <{}>))", max));
    assert_eq!(expr_sexp("xs[..=2]"), "(index xs (..= <0> 2))");
    assert_eq!(expr_sexp("s[a + 1..]"), format!("(index s (.. (+ a 1) <{}>))", max));
    // An ordinary index and a closed range are what they were.
    assert_eq!(expr_sexp("xs[i + 1]"), "(index xs (+ i 1))");
    assert_eq!(expr_sexp("xs[1..3]"), "(index xs (.. 1 3))");
    // An inclusive range has to say what it includes, and only an index may
    // leave an end out.
    let p = parse_src("fn f() {\n    let a = xs[1..=]\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    let p = parse_src("fn f() {\n    let r = 1..\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
}

// ---- interpolation ------------------------------------------------------------

/// A hole holds one expression. Anything after it used to be dropped without
/// a word: `"sum: \(a b)"` printed `sum: 1`.
#[test]
fn tokens_left_in_a_hole_are_an_error() {
    for src in [r#""sum: \(a b)""#, r#""\(a, b)""#, r#""\(a) = \(b junk here )""#] {
        let p = parse_src(&format!("fn f() {{\n    let s = {}\n}}\n", src));
        assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
        assert!(p.render().contains("expected `)` to close the interpolation"), "{}", p.render());
    }
    // Space inside the parentheses is only space.
    assert_eq!(expr_sexp(r#""\( a + b )""#), "(str (+ a b))");
}

/// The ceiling on nested interpolations is reported once. Each hole is
/// scanned again for its own tokens, and a scan that started counting from
/// zero found the ceiling again at every level past it.
#[test]
fn deep_interpolation_is_reported_once() {
    let mut s = String::from("x");
    for _ in 0..70 {
        s = format!("\"\\({})\"", s);
    }
    let codes = parse_deep(format!("fn f() {{\n    let s = {}\n}}\n", s));
    let e0006 = codes.iter().filter(|c| **c == "E0006").count();
    assert_eq!(e0006, 1, "{:?}", codes);
}

/// §2.4: a block string loses the line break after its opening delimiter,
/// its closing delimiter's line, and that line's indentation from every line
/// — with a hole in it as without. The holes are left where they are and the
/// text around them is cut, so the whitespace that goes is simply not in any
/// piece.
#[test]
fn a_block_string_with_holes_is_dedented() {
    let src = "\"\"\"\n        hello \\(n)\n          world \\(m)\n        \"\"\"";
    assert_eq!(expr_sexp(src), r#"(str "hello " n "\n" "  world " m)"#);
    // A hole at the start of a line keeps the line's own indentation beyond
    // the delimiter's.
    let src = "\"\"\"\n        \\(n)\n          \\(m)\n        \"\"\"";
    assert_eq!(expr_sexp(src), r#"(str n "\n" "  " m)"#);
    // A closing delimiter at the end of a line of text means there is no
    // indentation to take off, and only the opening line break goes.
    let src = "\"\"\"\n    a \\(n)\n    b\"\"\"";
    assert_eq!(expr_sexp(src), r#"(str "    a " n "\n    b")"#);
}

// ---- depth -------------------------------------------------------------------

/// Run `f` on a thread with a main thread's stack; a test thread has a
/// quarter of it. The parser itself runs in far less, but a tree as deep as
/// the ceilings allow is dropped by recursion.
fn on_a_main_stack<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("the parser did not survive")
}

fn parse_deep(src: String) -> Vec<&'static str> {
    on_a_main_stack(move || parse_src(&src).codes())
}

/// Every one of these aborted the process, in the parser or in a pass after
/// it. Recursion through a prefix operator counts toward the nesting
/// ceiling, and each link of a left-deep chain — an `else if` among them —
/// toward the chain ceiling, since the tree a chain builds is as deep as a
/// nest.
#[test]
fn deep_input_is_one_diagnostic_not_an_abort() {
    let body = |expr: String| format!("fn main() {{\n    let x = {}\n}}\n", expr);
    let inputs = [
        body(format!("{}1", "-".repeat(30_000))),
        body(format!("{}true", "!".repeat(30_000))),
        format!(
            "fn main() {{\n    if a {{\n    }}{}\n}}\n",
            " else if a {\n    }".repeat(15_000)
        ),
        body(format!("a{}", ".a".repeat(10_000))),
        body(format!("{}1", "1 + ".repeat(20_000))),
        body(format!("1{}", " as int".repeat(20_000))),
        body(format!("f{}", "(1)".repeat(100_000))),
    ];
    for src in inputs {
        let codes = parse_deep(src);
        assert_eq!(codes, vec!["E0102"]);
    }
}

/// A chain is counted apart from nesting, against a ceiling of its own far
/// above anything written or generated in earnest. Charged against the
/// nesting ceiling of 256, a table of three hundred `else if`, or a text
/// joined with three hundred `+`, was refused — programs the compiler had
/// always compiled.
#[test]
fn a_long_chain_is_not_a_deep_nest() {
    let body = |expr: String| format!("fn main() {{\n    let x = {}\n}}\n", expr);
    let links = MAX_CHAIN as usize;
    let chains = [
        format!(
            "fn main() {{\n    if a {{\n    }}{}\n}}\n",
            " else if a {\n    }".repeat(300)
        ),
        body(format!("{}1", "1 + ".repeat(300))),
        body(format!("a{}", " || a".repeat(300))),
        body(format!("s{}", ".trim()".repeat(300))),
        // The longest chain there may be, of each kind: its links, and not
        // one more.
        body(format!("{}1", "1 + ".repeat(links))),
        body(format!("s{}", ".f()".repeat(links / 2))),
        format!(
            "fn main() {{\n    if a {{\n    }}{}\n}}\n",
            " else if a {\n    }".repeat(links)
        ),
    ];
    for src in chains {
        let codes = parse_deep(src);
        assert!(codes.is_empty(), "{:?}", codes);
    }
    // One link more is the one diagnostic, saying which ceiling it is.
    let src = body(format!("{}1", "1 + ".repeat(links + 1)));
    let (codes, out) = on_a_main_stack(move || {
        let p = parse_src(&src);
        (p.codes(), p.render())
    });
    assert_eq!(codes, vec!["E0102"], "{}", out);
    assert!(out.contains(&format!("at most {} links long", links)), "{}", out);
    // The two ceilings are apart: a nest does not spend a chain's links, nor a
    // chain a nest's levels.
    let (open, close) = ("(".repeat(200), ")".repeat(200));
    let nested = format!("{}{}1{}", open, "1 + ".repeat(links - 10), close);
    assert!(parse_deep(body(nested)).is_empty());
}

/// A binding whose value is refused is still a binding, so nothing that uses
/// it is reported again as naming nothing.
#[test]
fn a_binding_whose_value_is_refused_is_still_declared() {
    let links = MAX_CHAIN as usize;
    let src = format!(
        "fn main() {{\n    let s = {}1\n    var t = * 2\n    io.print(s)\n}}\n",
        "1 + ".repeat(links + 1)
    );
    let (codes, kept) = on_a_main_stack(move || {
        let p = parse_src(&src);
        let body = &p.fns()[0].body.stmts;
        let kept = matches!(&body[0], Stmt::Let(LetStmt { init: Some(Expr::Error(_)), .. }))
            && matches!(&body[1], Stmt::Var(VarStmt { init: Expr::Error(_), .. }))
            && matches!(&body[2], Stmt::Expr(Expr::Call { .. }));
        (p.codes(), kept)
    });
    assert_eq!(codes, vec!["E0102", "E0100"]);
    assert!(kept, "the `let` and the `var` are kept, and the line after them read");
    // And at module level.
    let p = parse_src("let LIMIT: int = 1 +\n\nfn main() {\n    io.print(LIMIT)\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert!(matches!(&p.file.items[0], Item::Const(c) if matches!(c.value, Expr::Error(_))));
    assert_eq!(p.fns().len(), 1);
}

// ---- recovery: one diagnostic per cause -------------------------------------

/// A missing `}` is found where the author thought the block had ended — the
/// next declaration — and reported once, at the `{` whose `}` went missing.
/// This was eight errors: every declaration after it read as a statement.
#[test]
fn a_missing_brace_before_a_declaration_is_one_error() {
    let src = "\
fn a(x: int) -> int {
    if x > 0 {
        return 1
    return 0
}

fn b() -> int {
    return 2
}

fn main() {
    io.print(a(1) + b())
}
";
    let p = parse_src(src);
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    let out = p.render();
    // The `if`'s brace, found by the `}` whose indentation gave it away.
    assert!(out.contains("2 │     if x > 0 {"), "{}", out);
    let names: Vec<_> = p.fns().iter().map(|f| f.name.name.clone()).collect();
    assert_eq!(names, vec!["a", "b", "main"]);
}

#[test]
fn a_struct_missing_its_brace_is_one_error() {
    let p = parse_src("struct P {\n    x: int\n\nfn main() {\n    let p = P{ x: 1 }\n}\n");
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    assert!(matches!(&p.file.items[0], Item::Struct(s) if s.fields.len() == 1));
    assert_eq!(p.fns().len(), 1);
}

#[test]
fn an_enum_missing_its_brace_is_one_error() {
    let p = parse_src("enum E {\n    A\n    B\n\nfn main() {\n}\n");
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    assert!(matches!(&p.file.items[0], Item::Enum(e) if e.variants.len() == 2));
    assert_eq!(p.fns().len(), 1);
}

#[test]
fn an_impl_missing_its_brace_is_one_error() {
    let src = "impl P {\n    fn a(self) -> int {\n        return 1\n    }\n\nfn main() {\n}\n";
    let p = parse_src(src);
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    assert_eq!(p.fns().len(), 1);
}

/// At the end of the file, one report for however many blocks are open.
#[test]
fn braces_left_open_at_the_end_are_one_error() {
    let p = parse_src("fn main() {\n    if a {\n        for {\n            g()\n");
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    let p = parse_src("fn main() {\n    let x = 1\n    if x > 0 {\n        io.print(x)\n\n}\n");
    assert_eq!(p.codes(), vec!["E0101"], "{}", p.render());
    assert!(p.render().contains("3 │     if x > 0 {"), "{}", p.render());
}

/// Code that is not indented says nothing about where a brace went missing,
/// and is read as it always was — a `fn` at the margin of an unindented
/// `impl` is a method.
#[test]
fn unindented_code_is_not_mistaken_for_a_missing_brace() {
    let p = ok("impl P {\nfn a(self) {\n}\nfn b(self) {\n}\n}\nstruct Q {\npub x: int\n}\n");
    let Item::Impl(i) = &p.file.items[0] else { panic!() };
    assert_eq!(i.methods.len(), 2);
}

/// A declaration inside a block is one error, and skipped whole.
#[test]
fn a_declaration_inside_a_block_is_one_error() {
    let p = parse_src(
        "fn main() {\n    fn helper(x: int) -> int {\n        return x\n    }\n    let y = 1\n}\n",
    );
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    let p = parse_src(
        "struct P {\n    x: int\n    fn area(self) -> int {\n        return 1\n    }\n}\n",
    );
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
}

/// A mistake inside a multi-line struct literal is that one mistake. The
/// literal's own `}` used to be taken for the end of the function.
#[test]
fn a_bad_field_value_is_one_error() {
    let src = "\
struct Point {
    x: int
    y: int
}

fn main() {
    let p = Point{
        x: 1,
        y: = 2,
    }
    io.print(p.x)
}

fn other() -> int {
    return 1
}
";
    let p = parse_src(src);
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    let main = p.fns()[0];
    assert!(matches!(&main.body.stmts[0], Stmt::Let(_)), "{:?}", main.body.stmts);
    assert_eq!(main.body.stmts.len(), 2);
    assert_eq!(p.fns().len(), 2);
}

/// A comma missing between parameters or arguments is supplied, so the
/// function is still declared and its callers still resolve.
#[test]
fn a_missing_comma_is_one_error() {
    let p = parse_src(
        "fn add(a: int b: int) -> int {\n    let c = a + b\n    return c\n}\n\n\
         fn main() {\n    io.print(add(1, 2))\n}\n",
    );
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert_eq!(p.fns()[0].params.len(), 2);
    assert_eq!(p.fns().len(), 2);
    let p = parse_src("fn main() {\n    io.print(add(1 2))\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
}

/// Struct fields and enum variants are separated by line breaks. A comma is
/// reported once and read as one, rather than losing the member after it.
#[test]
fn a_comma_between_members_is_one_error() {
    let p = parse_src("struct P {\n    x: int,\n    y: int,\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert!(matches!(&p.file.items[0], Item::Struct(s) if s.fields.len() == 2));
    let p = parse_src("enum E {\n    A,\n    B,\n    C\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert!(matches!(&p.file.items[0], Item::Enum(e) if e.variants.len() == 3));
}

/// A declaration that fails in its signature is skipped to the next
/// declaration, not to the first line break inside its body.
#[test]
fn a_broken_signature_skips_its_body() {
    let p = parse_src("fn f(a: int -> int {\n    let c = a\n    return c\n}\n\nfn main() {\n}\n");
    assert_eq!(p.codes(), vec!["E0100"], "{}", p.render());
    assert!(p.fns().iter().any(|f| f.name.name == "main"));
}

// ---- the formatter's view ---------------------------------------------------

/// The `<` and `>` of type argument and generic parameter lists, found by the
/// parser rather than guessed at.
#[test]
fn type_brackets_are_the_ones_the_parser_read_as_types() {
    let brackets = |src: &str| {
        let mut diags = DiagBag::new();
        let tokens = kite_lexer::tokenize(FileId(0), src, &mut diags);
        layout(FileId(0), src, &tokens).map(|l| {
            let at = l.type_brackets.iter();
            at.map(|&at| &src[at as usize..at as usize + 1]).collect::<String>()
        })
    };
    assert_eq!(brackets("fn f<T: A + B>(x: Option<int>) {\n}\n").as_deref(), Some("<><>"));
    assert_eq!(brackets("type B = Box<Box<int>>\n").as_deref(), Some("<<>>"));
    assert_eq!(brackets("fn f() {\n    let a: Option<int>= nil\n}\n").as_deref(), Some("<>"));
    // A comparison is not a type, whatever the tokens around it look like.
    assert_eq!(
        brackets("fn f() {\n    g(a < b, c > d)\n    let x = a > (b - 1)\n}\n").as_deref(),
        Some("")
    );
    // A file that does not parse answers nothing.
    assert_eq!(brackets("fn f( {\n"), None);
}

/// The braces written against a type's name: a struct literal's and a struct
/// pattern's, and not a block's however the line before it ends.
#[test]
fn literal_braces_are_the_ones_the_parser_read_as_literals() {
    let src = "fn f(p: P) {\n    match p {\n        P{ x, .. } => g(P{ x: 1 }),\n    }\n\
               \x20   if a &&\n        b {\n    }\n}\n";
    let mut diags = DiagBag::new();
    let tokens = kite_lexer::tokenize(FileId(0), src, &mut diags);
    let braces = layout(FileId(0), src, &tokens).expect("parses").literal_braces;
    let before: Vec<&str> = braces.iter().map(|&at| &src[at as usize - 1..at as usize]).collect();
    assert_eq!(before, vec!["P", "P"]);
}
