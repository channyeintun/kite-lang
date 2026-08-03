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
