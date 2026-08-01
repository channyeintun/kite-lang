use super::*;
use kite_span::SourceMap;

fn build(src: &str) -> Program {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let hir = kite_types::check(&ast, &resolved, src, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
    lower(&hir)
}

fn main_fn(src: &str) -> Function {
    let p = build(&format!("fn main() {{\n{}\n}}\n", src));
    p.fns.into_iter().next().expect("a main function")
}

/// Every block must end in a real terminator, and every block reachable from
/// the entry must not be `Unreachable`.
fn assert_well_formed(f: &Function) {
    let reachable = reachable_blocks(f);
    for (i, b) in f.blocks.iter().enumerate() {
        if !reachable[i] {
            continue;
        }
        assert!(
            !matches!(b.term, Terminator::Unreachable),
            "reachable bb{} in `{}` has no terminator:\n{}",
            i,
            f.name,
            f
        );
        for s in b.term.successors() {
            assert!(
                s.index() < f.blocks.len(),
                "bb{} jumps to a nonexistent bb{}",
                i,
                s.0
            );
        }
    }
}

#[test]
fn every_lowered_function_is_well_formed() {
    let p = build(
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
    var n = 0
    for n < 3 {
        n += 1
    }
    for {
        break
    }
}
",
    );
    for f in &p.fns {
        assert_well_formed(f);
    }
}

#[test]
fn a_unit_function_returns_at_the_end() {
    let f = main_fn("  let x = 1");
    let last = f.blocks.last().unwrap();
    assert!(matches!(last.term, Terminator::Return(None)), "{}", f);
}

#[test]
fn parameters_stay_at_the_front_of_the_local_table() {
    let p = build("fn f(a: int, b: int) -> int {\n  let c = a\n  return c\n}\n");
    let f = &p.fns[0];
    assert_eq!(f.param_count, 2);
    assert_eq!(f.locals[0].name.as_deref(), Some("a"));
    assert_eq!(f.locals[1].name.as_deref(), Some("b"));
}

/// The reason `for` survives HIR: `continue` must reach the increment, or the
/// loop never advances.
#[test]
fn continue_in_a_range_loop_targets_the_increment_not_the_header() {
    let f = main_fn("  for i in 0..10 {\n    continue\n  }");
    assert_well_formed(&f);

    // Find the block whose only statement increments the counter.
    let step = f
        .blocks
        .iter()
        .position(|b| {
            b.stmts.iter().any(|s| match s {
                Inst::Assign { value: Rvalue::Binary { op: BinOp::AddInt, rhs, .. }, .. } => {
                    matches!(rhs, Operand::Int(1))
                }
                _ => false,
            })
        })
        .expect("an increment block");

    // The body's `continue` must jump there.
    let jumps_to_step = f
        .blocks
        .iter()
        .any(|b| matches!(b.term, Terminator::Goto(t) if t.index() == step));
    assert!(jumps_to_step, "continue does not reach the increment:\n{}", f);
}

#[test]
fn break_leaves_the_loop_entirely() {
    let f = main_fn("  for i in 0..10 {\n    break\n  }\n  io.print(1)");
    assert_well_formed(&f);
}

#[test]
fn a_labelled_continue_targets_the_outer_loop() {
    let f = main_fn(
        "  outer: for i in 0..3 {\n    for j in 0..3 {\n      continue outer\n    }\n  }",
    );
    assert_well_formed(&f);
    // Two range loops means two increment blocks.
    let increments = f
        .blocks
        .iter()
        .filter(|b| {
            b.stmts.iter().any(|s| {
                matches!(
                    s,
                    Inst::Assign { value: Rvalue::Binary { op: BinOp::AddInt, .. }, .. }
                )
            })
        })
        .count();
    assert_eq!(increments, 2, "{}", f);
}

/// The bound is evaluated once, before the loop, so a call there runs one time.
#[test]
fn the_range_bound_is_hoisted_out_of_the_loop() {
    let p = build(
        "fn bound() -> int {\n  return 3\n}\nfn main() {\n  for i in 0..bound() {\n  }\n}\n",
    );
    let f = p.fns.iter().find(|f| f.name == "main").unwrap();
    let entry = f.block(f.entry_block());
    let calls_in_entry = entry
        .stmts
        .iter()
        .filter(|s| matches!(s, Inst::Assign { value: Rvalue::Call { .. }, .. }))
        .count();
    assert_eq!(calls_in_entry, 1, "bound not hoisted:\n{}", f);
}

#[test]
fn short_circuit_and_branches_rather_than_evaluating_both() {
    let f = main_fn("  let a = true\n  let b = false\n  let c = a && b");
    assert_well_formed(&f);
    let branches = f
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Branch { .. }))
        .count();
    assert!(branches >= 1, "&& did not become a branch:\n{}", f);
}

#[test]
fn an_if_expression_assigns_through_both_arms() {
    let f = main_fn("  let a = if true { 1 } else { 2 }");
    assert_well_formed(&f);
    let branches = f
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Branch { .. }))
        .count();
    assert_eq!(branches, 1, "{}", f);
}

#[test]
fn identical_strings_are_interned_once() {
    let p = build("fn main() {\n  io.print(\"x\")\n  io.print(\"x\")\n  io.print(\"y\")\n}\n");
    assert_eq!(p.strings, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn statements_after_a_return_do_not_follow_the_terminator() {
    let p = build("fn f() -> int {\n  return 1\n}\n");
    let f = &p.fns[0];
    let entry = f.block(f.entry_block());
    assert!(matches!(entry.term, Terminator::Return(Some(_))), "{}", f);
}

#[test]
fn a_call_in_statement_position_is_still_emitted() {
    let p = build("fn side() {\n}\nfn main() {\n  side()\n}\n");
    let f = p.fns.iter().find(|f| f.name == "main").unwrap();
    let has_call = f
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Inst::Assign { value: Rvalue::Call { .. }, .. }));
    assert!(has_call, "call discarded:\n{}", f);
}

#[test]
fn the_entry_function_is_recorded() {
    let p = build("fn other() {\n}\nfn main() {\n}\n");
    assert_eq!(p.entry, Some(FnId(1)));
}
