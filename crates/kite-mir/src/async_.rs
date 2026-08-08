//! The state-machine transform.
//!
//! An `async fn` compiles to two ordinary functions:
//!
//! * a **starter**, which allocates the frame and the task, hands the
//!   scheduler a closure that resumes it, and returns the task; and
//! * a **resume function**, which is the original body with an entry that
//!   jumps to wherever the last suspension left off.
//!
//! Nothing after this pass knows `async` exists. Both backends see ordinary
//! functions, ordinary structs and an ordinary closure, which is the whole
//! reason the transform lives here rather than twice in the backends.
//!
//! MIR is already a control-flow graph, so the hard part of the usual
//! transform — recovering the resume points — is free: a suspension is a block
//! boundary, and the entry block dispatches on a state field to reach it.
//!
//! **Locals are spilled and reloaded around a suspension** rather than being
//! rewritten into frame fields everywhere. It costs one store and one load per
//! local per suspension, and it means the code between suspensions is byte for
//! byte what it would have been without `async`. Which locals are actually
//! live across a given suspension is a dataflow question worth answering
//! later; answering it wrongly is a miscompile, and this cannot be wrong.
//!
//! The [stack-switching proposal](https://github.com/WebAssembly/stack-switching)
//! would let a suspension keep a real stack and delete all of this. Kite's
//! semantics are compatible with either lowering, so adopting it later is a
//! compiler change with no language change.

use crate::*;
use kite_hir::{FieldDef, StructId, TyId, Types};

/// Rewrite every `async fn` into a starter and a resume function.
pub fn transform(program: &mut Program, types: &mut Types) {
    let async_fns: Vec<usize> = program
        .fns
        .iter()
        .enumerate()
        .filter(|(_, f)| f.is_async)
        .map(|(i, _)| i)
        .collect();

    for index in async_fns {
        let resume_id = FnId(program.fns.len() as u32);
        let resume = build(&mut program.fns[index], resume_id, types);
        program.fns.push(resume);
    }
}

/// Field positions in the frame. Everything from `FIRST_LOCAL` on is a
/// spilled local, in the body's own order.
const STATE: u32 = 0;
const TASK: u32 = 1;
const FIRST_LOCAL: u32 = 2;

/// Field positions in a `Task<T>`, as declared by [`Types::task_of`].
const DONE: u32 = 0;
const VALUE: u32 = 1;

/// How many MIR blocks one original block becomes, given `k` suspensions in
/// it: a segment between each pair, plus a test, a suspend and a landing block
/// for each.
fn blocks_for(suspensions: usize) -> u32 {
    (4 * suspensions + 1) as u32
}

fn suspension_count(block: &BasicBlock) -> usize {
    block
        .stmts
        .iter()
        .filter(|i| {
            matches!(
                i,
                Inst::Assign { value: Rvalue::Await { .. } | Rvalue::Yield, .. }
            )
        })
        .count()
}

/// Turn `func` into the starter, and return the resume function.
fn build(func: &mut Function, resume_id: FnId, types: &mut Types) -> Function {
    let span = func.span;
    let task_struct = types.task_of(func.ret, span);
    let task_ty = types.struct_ty(task_struct);
    let frame_struct = declare_frame(func, task_ty, types);
    let frame_ty = types.struct_ty(frame_struct);
    let poll_ty = types.fn_of(Vec::new(), TyId::BOOL);

    let body = std::mem::take(&mut func.blocks);
    let body_locals = std::mem::take(&mut func.locals);

    let resume = resume_function(
        format!("{}$resume", func.name),
        body,
        &body_locals,
        frame_ty,
        frame_struct,
        task_ty,
        types,
        span,
    );

    let started = starter(
        func,
        resume_id,
        &body_locals,
        frame_struct,
        task_struct,
        frame_ty,
        task_ty,
        poll_ty,
        types,
    );
    *func = started;
    resume
}

/// The frame: where the state machine keeps everything across a suspension.
fn declare_frame(func: &Function, task_ty: TyId, types: &mut Types) -> StructId {
    let span = func.span;
    let id = types.declare_struct(format!("{}$frame", func.name), false, span);
    let mut fields = vec![
        FieldDef { name: "$state".into(), ty: TyId::INT, mutable: true, is_pub: false, span },
        FieldDef { name: "$task".into(), ty: task_ty, mutable: true, is_pub: false, span },
    ];
    for (i, l) in func.locals.iter().enumerate() {
        fields.push(FieldDef {
            name: format!("${}", i),
            ty: l.ty,
            mutable: true,
            is_pub: false,
            span,
        });
    }
    types.set_struct_fields(id, fields);
    id
}

/// `f(args) -> Task<T>`: allocate, hand the scheduler a way to resume, return.
///
/// Calling an `async fn` *starts* it. That is what makes two calls followed by
/// two `await`s concurrent, and it is why there is no second keyword for
/// spawning.
#[allow(clippy::too_many_arguments)]
fn starter(
    old: &Function,
    resume_id: FnId,
    body_locals: &[LocalDecl],
    frame_struct: StructId,
    task_struct: StructId,
    frame_ty: TyId,
    task_ty: TyId,
    poll_ty: TyId,
    types: &Types,
) -> Function {
    let param_count = old.param_count;
    let mut locals: Vec<LocalDecl> = Vec::new();
    // Parameters keep their slots, so a caller's argument order is unchanged.
    for l in body_locals.iter().take(param_count) {
        locals.push(LocalDecl { ty: l.ty, name: l.name.clone() });
    }
    let task = Local(locals.len() as u32);
    locals.push(LocalDecl { ty: task_ty, name: Some("task".into()) });
    let frame = Local(locals.len() as u32);
    locals.push(LocalDecl { ty: frame_ty, name: Some("frame".into()) });
    let poll = Local(locals.len() as u32);
    locals.push(LocalDecl { ty: poll_ty, name: Some("poll".into()) });
    let unit = Local(locals.len() as u32);
    locals.push(LocalDecl { ty: TyId::UNIT, name: None });

    let value_ty = types.struct_def(task_struct).fields[VALUE as usize].ty;
    let mut stmts = vec![
        // The task starts unfinished, with a slot for a value nothing may read
        // until it is: `await` tests first, and `task.get` traps.
        Inst::Assign {
            dst: task,
            value: Rvalue::StructNew {
                struct_id: task_struct,
                fields: vec![Operand::Bool(false), Operand::Default(value_ty)],
            },
        },
    ];

    let mut frame_init = vec![Operand::Int(0), Operand::Local(task)];
    for (i, l) in body_locals.iter().enumerate() {
        frame_init.push(if i < param_count {
            Operand::Local(Local(i as u32))
        } else {
            Operand::Default(l.ty)
        });
    }
    stmts.push(Inst::Assign {
        dst: frame,
        value: Rvalue::StructNew { struct_id: frame_struct, fields: frame_init },
    });
    // The resume closure captures the frame — by value, as every capture is,
    // but a frame is a reference and its fields are `var`, so what it records
    // survives from one poll to the next.
    stmts.push(Inst::Assign {
        dst: poll,
        value: Rvalue::ClosureNew { func: resume_id, captures: vec![Operand::Local(frame)] },
    });
    stmts.push(Inst::Assign {
        dst: unit,
        value: Rvalue::CallBuiltin {
            builtin: kite_hir::Builtin::TaskSpawn,
            args: vec![Operand::Local(poll)],
        },
    });

    Function {
        name: old.name.clone(),
        is_async: false,
        exportable: old.exportable,
        param_count,
        locals,
        ret: task_ty,
        blocks: vec![BasicBlock {
            stmts,
            term: Terminator::Return(Some(Operand::Local(task))),
        }],
        span: old.span,
    }
}

/// The body, with an entry that dispatches on the state and a suspension at
/// every `await` that finds its task unfinished.
#[allow(clippy::too_many_arguments)]
fn resume_function(
    name: String,
    body: Vec<BasicBlock>,
    body_locals: &[LocalDecl],
    frame_ty: TyId,
    frame_struct: StructId,
    task_ty: TyId,
    types: &Types,
    span: Span,
) -> Function {
    // The frame is the closure's one capture, so it is parameter 0 and every
    // original local moves up one slot.
    let mut locals: Vec<LocalDecl> = vec![LocalDecl { ty: frame_ty, name: Some("frame".into()) }];
    locals.extend(body_locals.iter().map(|l| LocalDecl { ty: l.ty, name: l.name.clone() }));
    let local_count = body_locals.len();
    let task = Local(locals.len() as u32);
    locals.push(LocalDecl { ty: task_ty, name: Some("task".into()) });
    let value_ty = types
        .task_payload(task_ty)
        .expect("a resume function's task type is a task");

    // Where each original block lands. Block 0 is the dispatcher, so the body
    // starts at 1, and each original block claims as many ids as its
    // suspensions need — the first of them keeps the original's identity, so a
    // jump to it lands before anything the block does.
    let mut starts: Vec<u32> = Vec::with_capacity(body.len());
    let mut next = 1;
    for b in &body {
        starts.push(next);
        next += blocks_for(suspension_count(b));
    }

    let mut builder = Builder {
        blocks: vec![BasicBlock::default(); next as usize],
        locals,
        starts,
        local_count,
        task,
        resume_points: Vec::new(),
        value_ty,
        types,
        frame_struct,
    };
    for (i, block) in body.into_iter().enumerate() {
        builder.rewrite_block(i, block);
    }
    builder.build_dispatch();

    Function {
        name,
        is_async: false,
        exportable: false,
        param_count: 1,
        locals: builder.locals,
        ret: TyId::BOOL,
        blocks: builder.blocks,
        span,
    }
}

struct Builder<'a> {
    blocks: Vec<BasicBlock>,
    locals: Vec<LocalDecl>,
    /// Where each original block's first segment landed.
    starts: Vec<u32>,
    /// How many locals came from the original body.
    local_count: usize,
    task: Local,
    /// The landing block for each state, in state order. State 0 is the entry.
    resume_points: Vec<BlockId>,
    /// What the task's value slot holds, which is what a `return` writes.
    value_ty: TyId,
    types: &'a Types,
    frame_struct: StructId,
}

impl Builder<'_> {
    fn frame(&self) -> Operand {
        Operand::Local(Local(0))
    }

    /// Copy every local into the frame, immediately before a suspension.
    fn spill(&self, stmts: &mut Vec<Inst>) {
        for i in 0..self.local_count {
            stmts.push(Inst::SetField {
                base: self.frame(),
                index: FIRST_LOCAL + i as u32,
                value: Operand::Local(Local(i as u32 + 1)),
            });
        }
    }

    fn reload(&self, stmts: &mut Vec<Inst>) {
        for i in 0..self.local_count {
            stmts.push(Inst::Assign {
                dst: Local(i as u32 + 1),
                value: Rvalue::FieldGet { base: self.frame(), index: FIRST_LOCAL + i as u32 },
            });
        }
    }

    /// One block of the original body, with its suspensions split out.
    fn rewrite_block(&mut self, original: usize, block: BasicBlock) {
        let base = self.starts[original];
        let mut segment = 0u32;
        let mut stmts: Vec<Inst> = Vec::new();

        for inst in block.stmts {
            let (dst, awaited) = match inst {
                Inst::Assign { dst, value: Rvalue::Await { task } } => (Some(dst), Some(task)),
                Inst::Assign { value: Rvalue::Yield, .. } => (None, None),
                other => {
                    stmts.push(self.shift_inst(other));
                    continue;
                }
            };

            let here = BlockId(base + 4 * segment);
            let test = BlockId(here.0 + 1);
            let suspend = BlockId(here.0 + 2);
            let landing = BlockId(here.0 + 3);
            let next = BlockId(here.0 + 4);
            let state = self.resume_points.len() as i64 + 1;
            self.resume_points.push(landing);

            let mut suspend_stmts = Vec::new();
            self.spill(&mut suspend_stmts);
            suspend_stmts.push(Inst::SetField {
                base: self.frame(),
                index: STATE,
                value: Operand::Int(state),
            });

            match awaited {
                // `await t` — a finished task is taken at once, and only a
                // pending one costs a suspension. Awaiting a task that has
                // already completed, or awaiting one twice, never yields.
                //
                // Coming back from a suspension lands on the *test*, not past
                // it: being polled again is no promise that this particular
                // task has finished, only that the scheduler had nothing
                // better to do.
                Some(t) => {
                    let t = self.shift_operand(t);
                    let done = self.temp(TyId::BOOL);
                    self.blocks[here.index()] = BasicBlock {
                        stmts: std::mem::take(&mut stmts),
                        term: Terminator::Goto(test),
                    };
                    self.blocks[test.index()] = BasicBlock {
                        stmts: vec![Inst::Assign {
                            dst: done,
                            value: Rvalue::FieldGet { base: t.clone(), index: DONE },
                        }],
                        term: Terminator::Branch {
                            cond: Operand::Local(done),
                            then: next,
                            else_: suspend,
                        },
                    };
                    // Waiting on a task is waiting for *something to finish*,
                    // which is what parking says. Without it an awaiting task
                    // is runnable on every sweep and achieves nothing, and a
                    // scheduler cannot tell that from progress — so a program
                    // whose only other task is sleeping would spin instead of
                    // letting the clock move.
                    let parked = self.temp(TyId::UNIT);
                    suspend_stmts.push(Inst::Assign {
                        dst: parked,
                        value: Rvalue::CallBuiltin {
                            builtin: kite_hir::Builtin::TaskPark,
                            args: Vec::new(),
                        },
                    });
                    self.blocks[suspend.index()] = BasicBlock {
                        stmts: suspend_stmts,
                        term: Terminator::Return(Some(Operand::Bool(false))),
                    };
                    let mut land = Vec::new();
                    self.reload(&mut land);
                    self.blocks[landing.index()] =
                        BasicBlock { stmts: land, term: Terminator::Goto(test) };
                    // The value is taken in the segment that follows, on the
                    // one path that proved the task has one.
                    if let Some(d) = dst {
                        stmts.push(Inst::Assign {
                            dst: Local(d.0 + 1),
                            value: Rvalue::FieldGet { base: t, index: VALUE },
                        });
                    }
                }
                // `task.yield()` — suspend without asking anything, and carry
                // straight on when the scheduler comes back.
                None => {
                    let mut here_stmts = std::mem::take(&mut stmts);
                    here_stmts.extend(suspend_stmts);
                    self.blocks[here.index()] = BasicBlock {
                        stmts: here_stmts,
                        term: Terminator::Return(Some(Operand::Bool(false))),
                    };
                    // A yield needs no test: nothing is being waited for. The
                    // slots stay unreachable so every block's id is still a
                    // function of the suspension count alone.
                    self.blocks[test.index()] =
                        BasicBlock { stmts: Vec::new(), term: Terminator::Unreachable };
                    self.blocks[suspend.index()] =
                        BasicBlock { stmts: Vec::new(), term: Terminator::Unreachable };
                    let mut land = Vec::new();
                    self.reload(&mut land);
                    self.blocks[landing.index()] =
                        BasicBlock { stmts: land, term: Terminator::Goto(next) };
                }
            }
            segment += 1;
        }

        let here = BlockId(base + 4 * segment);
        let term = self.rewrite_terminator(block.term, &mut stmts);
        self.blocks[here.index()] = BasicBlock { stmts, term };
    }

    /// A `return` becomes "write the task, and tell the scheduler this one is
    /// finished". Everything else keeps its shape with its targets remapped.
    fn rewrite_terminator(&mut self, term: Terminator, stmts: &mut Vec<Inst>) -> Terminator {
        match term {
            Terminator::Goto(b) => Terminator::Goto(BlockId(self.starts[b.index()])),
            Terminator::Branch { cond, then, else_ } => Terminator::Branch {
                cond: self.shift_operand(cond),
                then: BlockId(self.starts[then.index()]),
                else_: BlockId(self.starts[else_.index()]),
            },
            Terminator::Return(value) => {
                let value = match value.map(|v| self.shift_operand(v)) {
                    // A fall-off-the-end return in a function that promised a
                    // value is unreachable — the checker has already said so —
                    // but the slot it writes is typed, and `()` is not a value
                    // of every type.
                    None | Some(Operand::Unit) if self.value_ty != TyId::UNIT => {
                        Operand::Default(self.value_ty)
                    }
                    Some(v) => v,
                    None => Operand::Unit,
                };
                stmts.push(Inst::Assign {
                    dst: self.task,
                    value: Rvalue::FieldGet { base: self.frame(), index: TASK },
                });
                stmts.push(Inst::SetField {
                    base: Operand::Local(self.task),
                    index: VALUE,
                    value,
                });
                stmts.push(Inst::SetField {
                    base: Operand::Local(self.task),
                    index: DONE,
                    value: Operand::Bool(true),
                });
                Terminator::Return(Some(Operand::Bool(true)))
            }
            Terminator::Unreachable => Terminator::Unreachable,
        }
    }

    /// Every original local moved up one slot to make room for the frame.
    fn shift_operand(&self, o: Operand) -> Operand {
        match o {
            Operand::Local(l) => Operand::Local(Local(l.0 + 1)),
            other => other,
        }
    }

    fn shift_inst(&self, inst: Inst) -> Inst {
        let shift = |o: Operand| self.shift_operand(o);
        let shift_rv = |v: Rvalue| -> Rvalue {
            match v {
                Rvalue::Use(o) => Rvalue::Use(shift(o)),
                Rvalue::Binary { op, lhs, rhs } => {
                    Rvalue::Binary { op, lhs: shift(lhs), rhs: shift(rhs) }
                }
                Rvalue::Unary { op, operand } => Rvalue::Unary { op, operand: shift(operand) },
                Rvalue::Call { callee, args } => {
                    Rvalue::Call { callee, args: args.into_iter().map(shift).collect() }
                }
                Rvalue::CallVirtual { trait_id, method, args } => Rvalue::CallVirtual {
                    trait_id,
                    method,
                    args: args.into_iter().map(shift).collect(),
                },
                Rvalue::ClosureNew { func, captures } => Rvalue::ClosureNew {
                    func,
                    captures: captures.into_iter().map(shift).collect(),
                },
                Rvalue::CallClosure { callee, args } => Rvalue::CallClosure {
                    callee: shift(callee),
                    args: args.into_iter().map(shift).collect(),
                },
                Rvalue::ToStr { operand, from } => Rvalue::ToStr { operand: shift(operand), from },
                Rvalue::StrOp { op, args } => {
                    Rvalue::StrOp { op, args: args.into_iter().map(shift).collect() }
                }
                Rvalue::Cast { operand, from, to } => {
                    Rvalue::Cast { operand: shift(operand), from, to }
                }
                Rvalue::CallBuiltin { builtin, args } => {
                    Rvalue::CallBuiltin { builtin, args: args.into_iter().map(shift).collect() }
                }
                Rvalue::CallExtern { index, args } => {
                    Rvalue::CallExtern { index, args: args.into_iter().map(shift).collect() }
                }
                Rvalue::StructNew { struct_id, fields } => Rvalue::StructNew {
                    struct_id,
                    fields: fields.into_iter().map(shift).collect(),
                },
                Rvalue::FieldGet { base, index } => {
                    Rvalue::FieldGet { base: shift(base), index }
                }
                Rvalue::EnumNew { enum_id, variant, fields } => Rvalue::EnumNew {
                    enum_id,
                    variant,
                    fields: fields.into_iter().map(shift).collect(),
                },
                Rvalue::TagOf { base } => Rvalue::TagOf { base: shift(base) },
                Rvalue::VariantGet { base, enum_id, variant, index } => Rvalue::VariantGet {
                    base: shift(base),
                    enum_id,
                    variant,
                    index,
                },
                Rvalue::TupleNew { elems } => {
                    Rvalue::TupleNew { elems: elems.into_iter().map(shift).collect() }
                }
                Rvalue::MapNew { entries } => {
                    Rvalue::MapNew { entries: entries.into_iter().map(shift).collect() }
                }
                Rvalue::MapGet { base, key } => {
                    Rvalue::MapGet { base: shift(base), key: shift(key) }
                }
                Rvalue::MapLen { base } => Rvalue::MapLen { base: shift(base) },
                Rvalue::MapKeys { base } => Rvalue::MapKeys { base: shift(base) },
                Rvalue::MapValues { base } => Rvalue::MapValues { base: shift(base) },
                Rvalue::SliceNew { elems } => {
                    Rvalue::SliceNew { elems: elems.into_iter().map(shift).collect() }
                }
                Rvalue::IsNil { value } => Rvalue::IsNil { value: shift(value) },
                Rvalue::Wrap { value } => Rvalue::Wrap { value: shift(value) },
                Rvalue::Unwrap { value } => Rvalue::Unwrap { value: shift(value) },
                Rvalue::PairNew { value, error } => {
                    Rvalue::PairNew { value: shift(value), error: shift(error) }
                }
                Rvalue::PairValue { base } => Rvalue::PairValue { base: shift(base) },
                Rvalue::PairError { base } => Rvalue::PairError { base: shift(base) },
                Rvalue::ErrorNew { message, value, tag, cause } => Rvalue::ErrorNew {
                    message: shift(message),
                    value: shift(value),
                    tag: shift(tag),
                    cause: shift(cause),
                },
                Rvalue::ErrorCause { base } => Rvalue::ErrorCause { base: shift(base) },
                Rvalue::ErrorTag { base } => Rvalue::ErrorTag { base: shift(base) },
                Rvalue::ErrorAs { base, tag } => Rvalue::ErrorAs { base: shift(base), tag },
                Rvalue::ErrorMessage { base } => Rvalue::ErrorMessage { base: shift(base) },
                Rvalue::IndexGet { base, index } => {
                    Rvalue::IndexGet { base: shift(base), index: shift(index) }
                }
                Rvalue::SliceLen { base } => Rvalue::SliceLen { base: shift(base) },
                Rvalue::SliceRange { base, start, end } => Rvalue::SliceRange {
                    base: shift(base),
                    start: shift(start),
                    end: shift(end),
                },
                Rvalue::SliceGet { base, index } => {
                    Rvalue::SliceGet { base: shift(base), index: shift(index) }
                }
                // Both are replaced above rather than shifted.
                Rvalue::Await { task } => Rvalue::Await { task: shift(task) },
                Rvalue::Yield => Rvalue::Yield,
            }
        };
        match inst {
            Inst::Assign { dst, value } => {
                Inst::Assign { dst: Local(dst.0 + 1), value: shift_rv(value) }
            }
            Inst::SetField { base, index, value } => Inst::SetField {
                base: shift(base),
                index,
                value: shift(value),
            },
            Inst::SetIndex { base, index, value } => Inst::SetIndex {
                base: shift(base),
                index: shift(index),
                value: shift(value),
            },
            Inst::SlicePush { local, value } => {
                Inst::SlicePush { local: Local(local.0 + 1), value: shift(value) }
            }
            Inst::MapSet { local, key, value } => Inst::MapSet {
                local: Local(local.0 + 1),
                key: shift(key),
                value: shift(value),
            },
        }
    }

    fn temp(&mut self, ty: TyId) -> Local {
        let id = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { ty, name: None });
        id
    }

    /// `if state == 1 goto r1 else if state == 2 … else <the body's entry>`.
    ///
    /// A chain of two-way branches rather than a switch, because that is what
    /// MIR has — and both backends already turn a chain into whatever their
    /// target does best.
    fn build_dispatch(&mut self) {
        let state = self.temp(TyId::INT);
        let test = self.temp(TyId::BOOL);
        let mut stmts = vec![Inst::Assign {
            dst: state,
            value: Rvalue::FieldGet { base: self.frame(), index: STATE },
        }];
        // The entry reloads too: on the first poll the parameters are in the
        // frame and nowhere else.
        self.reload(&mut stmts);

        // Assembled backwards, so each test's else-branch is the chain built
        // so far, ending at the body's own entry.
        let mut chain = BlockId(self.starts.first().copied().unwrap_or(0));
        for (i, target) in self.resume_points.clone().iter().enumerate().rev() {
            let block = BlockId(self.blocks.len() as u32);
            self.blocks.push(BasicBlock {
                stmts: vec![Inst::Assign {
                    dst: test,
                    value: Rvalue::Binary {
                        op: kite_hir::BinOp::EqInt,
                        lhs: Operand::Local(state),
                        rhs: Operand::Int(i as i64 + 1),
                    },
                }],
                term: Terminator::Branch {
                    cond: Operand::Local(test),
                    then: *target,
                    else_: chain,
                },
            });
            chain = block;
        }
        self.blocks[0] = BasicBlock { stmts, term: Terminator::Goto(chain) };
        let _ = self.types;
        let _ = self.frame_struct;
    }
}
