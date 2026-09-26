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

/// Lowered *and* run through the state-machine transform, which is what the
/// driver does for a program with `async` in it.
fn lower_async(src: &str) -> (Program, Types) {
    let mut b = build(src);
    crate::asyncify(&mut b.mir, &mut b.types);
    (b.mir, b.types)
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

// ---- the state-machine transform ------------------------------------------

/// An `async fn` becomes two functions: the starter a caller invokes, and the
/// resume function the scheduler polls.
#[test]
fn an_async_function_becomes_a_starter_and_a_resume_function() {
    let (program, _types) = lower_async(
        "async fn work() -> int {\n  task.yield()\n  return 7\n}\nasync fn main() {\n  io.print(await work())\n}\n",
    );
    let names: Vec<&str> = program.fns.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"work"), "{:?}", names);
    assert!(names.contains(&"work$resume"), "{:?}", names);
    assert!(names.contains(&"main$resume"), "{:?}", names);
}

/// Nothing past this pass knows `async` exists — which is the whole reason it
/// lives in MIR rather than twice in the backends.
#[test]
fn no_await_survives_the_transform() {
    let (program, _types) = lower_async(
        "async fn work() -> int {\n  return 7\n}\nasync fn main() {\n  io.print(await work())\n}\n",
    );
    for f in &program.fns {
        assert!(!f.is_async, "`{}` is still marked async", f.name);
        for b in &f.blocks {
            for s in &b.stmts {
                assert!(
                    !matches!(
                        s,
                        Inst::Assign { value: Rvalue::Await { .. } | Rvalue::Yield, .. }
                    ),
                    "a suspension survived in `{}`",
                    f.name
                );
            }
        }
    }
}

/// A resume function takes its frame and answers "am I finished?", which is
/// the entire contract the scheduler needs.
#[test]
fn a_resume_function_takes_a_frame_and_returns_whether_it_finished() {
    let (program, _types) = lower_async(
        "async fn work() -> int {\n  task.yield()\n  return 7\n}\nasync fn main() {\n  io.print(await work())\n}\n",
    );
    let resume = program
        .fns
        .iter()
        .find(|f| f.name == "work$resume")
        .expect("a resume function");
    assert_eq!(resume.param_count, 1, "the frame is its only parameter");
    assert_eq!(resume.ret, TyId::BOOL);
}

/// Every block a rewritten terminator names must exist. This is the invariant
/// A `break` with no loop to leave — the checker's to refuse, as it does for
/// one inside a closure whose body sits in a loop — used to be dropped, so the
/// closure ran on as if it were not there. Lowering now records it as the
/// compiler's error, and the block traps.
#[test]
fn a_break_with_no_loop_is_an_internal_error_not_a_silence() {
    let span = kite_span::Span::new(kite_span::FileId(0), 0, 0);
    let program = kite_hir::Program {
        fns: vec![kite_hir::Function {
            name: "lifted".into(),
            is_free: false,
            generic_count: 0,
            is_pub: false,
            is_async: false,
            param_count: 0,
            locals: Vec::new(),
            ret: TyId::UNIT,
            body: kite_hir::Block {
                stmts: vec![kite_hir::Stmt::Break { label: None, span }],
            },
            span,
        }],
        ..Default::default()
    };
    let mir = lower(&program);
    let found = internal_errors(&mir);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert_eq!(found[0].function, "lifted");
    assert!(found[0].what.contains("`break`"), "{}", found[0].what);
    assert!(matches!(mir.fns[0].blocks[0].term, Terminator::Unreachable));
}

/// An `await` in a function the state-machine transform did not rewrite — a
/// closure's lifted body, which is never `async` — reached every backend, and
/// each panicked on it. It is reported before any backend is asked.
#[test]
fn an_await_the_transform_left_is_an_internal_error() {
    let span = kite_span::Span::new(kite_span::FileId(0), 0, 0);
    let mut program = Program::default();
    program.fns.push(Function {
        name: "main#closure0".into(),
        is_async: false,
        exportable: false,
        param_count: 1,
        locals: vec![
            LocalDecl { ty: TyId::INT, name: None },
            LocalDecl { ty: TyId::INT, name: None },
        ],
        ret: TyId::UNIT,
        blocks: vec![BasicBlock {
            stmts: vec![Inst::Assign {
                dst: Local(1),
                value: Rvalue::Await { task: Operand::Local(Local(0)) },
            }],
            term: Terminator::Return(None),
        }],
        span,
    });
    let found = internal_errors(&program);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert_eq!(found[0].function, "main#closure0");

    // Everything the driver lowers passes, async included.
    let (lowered, _) = lower_async("async fn work() -> int {\n  return 1\n}\nasync fn main() {\n  io.print(await work())\n}\n");
    assert!(internal_errors(&lowered).is_empty());
}

/// `for i in a..=max` stopped only once the counter passed the bound, and the
/// increment that would pass `int`'s maximum overflowed. The step now leaves
/// when the counter equals the bound, before incrementing.
#[test]
fn an_inclusive_range_leaves_before_incrementing_past_its_bound() {
    let b = main_only("  for i in 0..=3 {\n    io.print(i)\n  }");
    let f = b.main();
    b.assert_well_formed(f);
    let leaves_on_equal = f.blocks.iter().any(|blk| {
        blk.stmts.iter().any(|s| {
            matches!(s, Inst::Assign { value: Rvalue::Binary { op: BinOp::EqInt, .. }, .. })
        }) && matches!(blk.term, Terminator::Branch { .. })
    });
    assert!(leaves_on_equal, "no equality test before the increment:\n{}", b.show());
}

/// A discarded value still has to be computed when computing it can trap.
#[test]
fn a_discarded_index_or_division_is_still_evaluated() {
    let b = build(
        "fn id(x: int) -> int {\n  return x\n}\n\
         fn main() {\n  let xs = [1, 2, 3]\n  _ = xs[10]\n  _ = id(1) / id(0)\n  xs[5]\n}\n",
    );
    let f = b.main();
    let count = |want: fn(&Rvalue) -> bool| {
        f.blocks
            .iter()
            .flat_map(|blk| &blk.stmts)
            .filter(|s| matches!(s, Inst::Assign { value, .. } if want(value)))
            .count()
    };
    assert_eq!(count(|v| matches!(v, Rvalue::IndexGet { .. })), 2, "{}", b.show());
    assert_eq!(
        count(|v| matches!(v, Rvalue::Binary { op: BinOp::DivInt, .. })),
        1,
        "{}",
        b.show()
    );
}

/// most easily broken by the block arithmetic, so it is asserted directly.
#[test]
fn every_block_the_transform_names_exists() {
    let (program, _types) = lower_async(
        "async fn work(n: int) -> int {\n  var total = 0\n  for i in 0..n {\n    if i == 2 {\n      task.yield()\n    }\n    total = total + i\n  }\n  return total\n}\nasync fn main() {\n  io.print(await work(4))\n}\n",
    );
    for f in &program.fns {
        for (i, b) in f.blocks.iter().enumerate() {
            for s in b.term.successors() {
                assert!(
                    s.index() < f.blocks.len(),
                    "`{}` block {} jumps to {:?}, which does not exist",
                    f.name,
                    i,
                    s
                );
            }
        }
    }
}
