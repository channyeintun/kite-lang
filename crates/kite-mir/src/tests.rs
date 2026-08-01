use super::*;
use kite_span::SourceMap;

/// A lowered program together with the arena its `TyId`s belong to, so a
/// failing assertion can still print readable MIR.
struct Built {
    mir: Program,
    types: Types,
}

impl Built {
    fn func(&self, name: &str) -> &Function {
        self.mir
            .fns
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{}`", name))
    }

    fn main(&self) -> &Function {
        self.func("main")
    }

    fn show(&self) -> String {
        self.mir.render(&self.types).to_string()
    }

    /// Every reachable block must end in a real terminator, and every jump must
    /// name a block that exists.
    fn assert_well_formed(&self, f: &Function) {
        let reachable = reachable_blocks(f);
        for (i, block) in f.blocks.iter().enumerate() {
            if !reachable[i] {
                continue;
            }
            assert!(
                !matches!(block.term, Terminator::Unreachable),
                "reachable bb{} in `{}` has no terminator:\n{}",
                i,
                f.name,
                self.show()
            );
            for s in block.term.successors() {
                assert!(
                    s.index() < f.blocks.len(),
                    "bb{} jumps to a nonexistent bb{}",
                    i,
                    s.0
                );
            }
        }
    }

    /// Blocks containing an `AddInt` — the increment a range loop's `continue`
    /// must reach.
    fn increment_blocks(&self, f: &Function) -> Vec<usize> {
        f.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.stmts.iter().any(|s| {
                    matches!(
                        s,
                        Inst::Assign { value: Rvalue::Binary { op: BinOp::AddInt, .. }, .. }
                    )
                })
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn branch_count(&self, f: &Function) -> usize {
        f.blocks
            .iter()
            .filter(|b| matches!(b.term, Terminator::Branch { .. }))
            .count()
    }
}

fn build(src: &str) -> Built {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
    let mir = lower(&hir);
    Built { mir, types: hir.types }
}

fn main_only(src: &str) -> Built {
    build(&format!("fn main() {{\n{}\n}}\n", src))
}

#[test]
fn every_lowered_function_is_well_formed() {
    let b = build(
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
    for f in &b.mir.fns {
        b.assert_well_formed(f);
    }
}

#[test]
fn a_unit_function_returns_at_the_end() {
    let b = main_only("  let x = 1");
    let last = b.main().blocks.last().unwrap();
    assert!(matches!(last.term, Terminator::Return(None)), "{}", b.show());
}

#[test]
fn parameters_stay_at_the_front_of_the_local_table() {
    let b = build("fn f(a: int, b: int) -> int {\n  let c = a\n  return c\n}\n");
    let f = b.func("f");
    assert_eq!(f.param_count, 2);
    assert_eq!(f.locals[0].name.as_deref(), Some("a"));
    assert_eq!(f.locals[1].name.as_deref(), Some("b"));
}

/// The reason `for` survives HIR: `continue` must reach the increment, or the
/// loop never advances.
#[test]
fn continue_in_a_range_loop_targets_the_increment_not_the_header() {
    let b = main_only("  for i in 0..10 {\n    continue\n  }");
    let f = b.main();
    b.assert_well_formed(f);

    let steps = b.increment_blocks(f);
    assert_eq!(steps.len(), 1, "expected one increment block:\n{}", b.show());
    let step = steps[0];

    let jumps_to_step = f
        .blocks
        .iter()
        .any(|blk| matches!(blk.term, Terminator::Goto(t) if t.index() == step));
    assert!(
        jumps_to_step,
        "continue does not reach the increment:\n{}",
        b.show()
    );
}

#[test]
fn break_leaves_the_loop_entirely() {
    let b = main_only("  for i in 0..10 {\n    break\n  }\n  io.print(1)");
    b.assert_well_formed(b.main());
}

#[test]
fn a_labelled_continue_targets_the_outer_loop() {
    let b = main_only(
        "  outer: for i in 0..3 {\n    for j in 0..3 {\n      continue outer\n    }\n  }",
    );
    let f = b.main();
    b.assert_well_formed(f);
    assert_eq!(
        b.increment_blocks(f).len(),
        2,
        "two range loops means two increments:\n{}",
        b.show()
    );
}

/// The bound is evaluated once, before the loop, so a call there runs one time.
#[test]
fn the_range_bound_is_hoisted_out_of_the_loop() {
    let b = build(
        "fn bound() -> int {\n  return 3\n}\nfn main() {\n  for i in 0..bound() {\n  }\n}\n",
    );
    let f = b.main();
    let entry = f.block(f.entry_block());
    let calls_in_entry = entry
        .stmts
        .iter()
        .filter(|s| matches!(s, Inst::Assign { value: Rvalue::Call { .. }, .. }))
        .count();
    assert_eq!(calls_in_entry, 1, "bound not hoisted:\n{}", b.show());
}

#[test]
fn short_circuit_and_branches_rather_than_evaluating_both() {
    let b = main_only("  let a = true\n  let other = false\n  let c = a && other");
    let f = b.main();
    b.assert_well_formed(f);
    assert!(
        b.branch_count(f) >= 1,
        "&& did not become a branch:\n{}",
        b.show()
    );
}

#[test]
fn an_if_expression_assigns_through_both_arms() {
    let b = main_only("  let a = if true { 1 } else { 2 }");
    let f = b.main();
    b.assert_well_formed(f);
    assert_eq!(b.branch_count(f), 1, "{}", b.show());
}

#[test]
fn identical_strings_are_interned_once() {
    let b = build("fn main() {\n  io.print(\"x\")\n  io.print(\"x\")\n  io.print(\"y\")\n}\n");
    assert_eq!(b.mir.strings, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn statements_after_a_return_do_not_follow_the_terminator() {
    let b = build("fn f() -> int {\n  return 1\n}\n");
    let f = b.func("f");
    let entry = f.block(f.entry_block());
    assert!(
        matches!(entry.term, Terminator::Return(Some(_))),
        "{}",
        b.show()
    );
}

#[test]
fn a_call_in_statement_position_is_still_emitted() {
    let b = build("fn side() {\n}\nfn main() {\n  side()\n}\n");
    let has_call = b
        .main()
        .blocks
        .iter()
        .flat_map(|blk| &blk.stmts)
        .any(|s| matches!(s, Inst::Assign { value: Rvalue::Call { .. }, .. }));
    assert!(has_call, "call discarded:\n{}", b.show());
}

#[test]
fn the_entry_function_is_recorded() {
    let b = build("fn other() {\n}\nfn main() {\n}\n");
    assert_eq!(b.mir.entry, Some(FnId(1)));
}
