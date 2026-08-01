//! HIR to MIR: control flow becomes a graph.

use crate::*;
use kite_hir::TyId as Ty;
use kite_hir::{EnumId, StructId, Types};
use kite_hir as hir;
use std::collections::HashMap;

pub fn lower(program: &hir::Program) -> Program {
    let mut out = Program {
        entry: program.entry.map(|e| FnId(e.0)),
        ..Default::default()
    };
    let mut strings = StringPool::default();

    for func in &program.fns {
        let lowered = FnLowerer::new(func, &program.types, &mut strings).run();
        out.fns.push(lowered);
    }
    out.strings = strings.list;
    out.vtables = program.vtables.clone();
    out
}

#[derive(Default)]
struct StringPool {
    list: Vec<String>,
    index: HashMap<String, u32>,
}

impl StringPool {
    fn intern(&mut self, s: &str) -> StrId {
        if let Some(&i) = self.index.get(s) {
            return StrId(i);
        }
        let i = self.list.len() as u32;
        self.list.push(s.to_string());
        self.index.insert(s.to_string(), i);
        StrId(i)
    }
}

/// Where `break` and `continue` jump to for one enclosing loop.
struct LoopCtx {
    label: Option<String>,
    /// `continue` target — for a range loop this is the increment block, not
    /// the header, so the counter still advances.
    continue_to: BlockId,
    break_to: BlockId,
}

/// Which aggregate a pattern's field belongs to.
///
/// A binding introduced by a pattern needs the field's *real* type, not a
/// placeholder: the bytecode VM has untyped registers and would not notice,
/// but Wasm locals are typed and a wrong one is a validation failure.
#[derive(Clone, Copy)]
enum FieldOwner {
    Variant(EnumId, u32),
    Struct(StructId),
    /// A tuple's elements are positional; the tuple's own type names them.
    Tuple(Ty),
}

struct FnLowerer<'a> {
    hir_fn: &'a hir::Function,
    types: &'a Types,
    strings: &'a mut StringPool,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    /// Set once the current block has a terminator. Further statements go to a
    /// fresh unreachable block rather than after the terminator.
    sealed: bool,
    loops: Vec<LoopCtx>,
}

impl<'a> FnLowerer<'a> {
    fn new(hir_fn: &'a hir::Function, types: &'a Types, strings: &'a mut StringPool) -> Self {
        let locals = hir_fn
            .locals
            .iter()
            .map(|l| LocalDecl {
                ty: l.ty,
                name: Some(l.name.clone()),
            })
            .collect();

        FnLowerer {
            hir_fn,
            types,
            strings,
            locals,
            blocks: vec![BasicBlock::default()],
            current: BlockId(0),
            sealed: false,
            loops: Vec::new(),
        }
    }

    fn run(mut self) -> Function {
        self.block(&self.hir_fn.body);

        // A function that falls off its end returns unit. The checker has
        // already rejected this for functions that declare a return type.
        if !self.sealed {
            self.terminate(Terminator::Return(if self.hir_fn.ret == TyId::UNIT {
                None
            } else {
                Some(Operand::Unit)
            }));
        }

        Function {
            name: self.hir_fn.name.clone(),
            exportable: self.hir_fn.is_pub && self.hir_fn.is_free,
            param_count: self.hir_fn.param_count,
            locals: self.locals,
            ret: self.hir_fn.ret,
            blocks: self.blocks,
            span: self.hir_fn.span,
        }
    }

    // ---- block plumbing ---------------------------------------------------

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock::default());
        id
    }

    fn switch_to(&mut self, b: BlockId) {
        self.current = b;
        self.sealed = false;
    }

    fn emit(&mut self, inst: Inst) {
        if self.sealed {
            // Unreachable: give it a home so indices stay meaningful, and so a
            // later pass can see and drop it.
            let b = self.new_block();
            self.switch_to(b);
        }
        self.blocks[self.current.index()].stmts.push(inst);
    }

    fn terminate(&mut self, t: Terminator) {
        if self.sealed {
            return;
        }
        self.blocks[self.current.index()].term = t;
        self.sealed = true;
    }

    fn temp(&mut self, ty: TyId) -> Local {
        let id = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { ty, name: None });
        id
    }

    fn assign(&mut self, dst: Local, value: Rvalue) {
        self.emit(Inst::Assign { dst, value });
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self, b: &hir::Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &hir::Stmt) {
        match s {
            hir::Stmt::Let { local, init, .. } => {
                if let Some(e) = init {
                    let v = self.rvalue(e);
                    self.assign(Local(local.0), v);
                }
            }
            hir::Stmt::Assign { local, value, .. } => {
                let v = self.rvalue(value);
                self.assign(Local(local.0), v);
            }
            hir::Stmt::Block(b) => self.block(b),
            hir::Stmt::SetIndex { base, index, value, .. } => {
                let b = self.operand(base);
                let i = self.operand(index);
                let v = self.operand(value);
                self.emit(Inst::SetIndex { base: b, index: i, value: v });
            }
            hir::Stmt::MapSet { local, key, value, .. } => {
                let k = self.operand(key);
                let v = self.operand(value);
                self.emit(Inst::MapSet { local: Local(local.0), key: k, value: v });
            }
            hir::Stmt::SlicePush { local, value, .. } => {
                let v = self.operand(value);
                self.emit(Inst::SlicePush { local: Local(local.0), value: v });
            }
            hir::Stmt::ForSlice { var, slice, body, label, .. } => {
                self.for_slice(*var, slice, body, label.as_deref())
            }
            hir::Stmt::SetField { base, index, value, .. } => {
                let b = self.operand(base);
                let v = self.operand(value);
                self.emit(Inst::SetField { base: b, index: *index, value: v });
            }
            hir::Stmt::Expr(e) => {
                // Evaluated for its effects. A call still has to happen.
                let v = self.rvalue(e);
                if matches!(v, Rvalue::Call { .. } | Rvalue::CallBuiltin { .. }) {
                    let t = self.temp(e.ty);
                    self.assign(t, v);
                }
            }
            hir::Stmt::Return { value, .. } => {
                let op = value.as_ref().map(|e| self.operand(e));
                self.terminate(Terminator::Return(op));
            }
            hir::Stmt::If { cond, then, else_, .. } => self.if_stmt(cond, then, else_.as_ref()),
            hir::Stmt::ForRange { var, start, end, inclusive, body, label, .. } => {
                self.for_range(*var, start, end, *inclusive, body, label.as_deref())
            }
            hir::Stmt::While { cond, body, label, .. } => {
                self.while_loop(cond, body, label.as_deref())
            }
            hir::Stmt::Loop { body, label, .. } => self.infinite_loop(body, label.as_deref()),
            hir::Stmt::Break { label, .. } => {
                if let Some(target) = self.find_loop(label.as_deref()).map(|c| c.break_to) {
                    self.terminate(Terminator::Goto(target));
                }
            }
            hir::Stmt::Continue { label, .. } => {
                if let Some(target) = self.find_loop(label.as_deref()).map(|c| c.continue_to) {
                    self.terminate(Terminator::Goto(target));
                }
            }
        }
    }

    fn find_loop(&self, label: Option<&str>) -> Option<&LoopCtx> {
        match label {
            None => self.loops.last(),
            Some(l) => self
                .loops
                .iter()
                .rev()
                .find(|c| c.label.as_deref() == Some(l)),
        }
    }

    fn if_stmt(&mut self, cond: &hir::Expr, then: &hir::Block, else_: Option<&hir::Block>) {
        let c = self.operand(cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join_bb = self.new_block();

        self.terminate(Terminator::Branch {
            cond: c,
            then: then_bb,
            else_: else_bb,
        });

        self.switch_to(then_bb);
        self.block(then);
        self.terminate(Terminator::Goto(join_bb));

        self.switch_to(else_bb);
        if let Some(e) = else_ {
            self.block(e);
        }
        self.terminate(Terminator::Goto(join_bb));

        self.switch_to(join_bb);
    }

    /// ```text
    ///   i = start;  bound = end
    ///   goto header
    ///   header: cond = i < bound ; branch cond ? body : exit
    ///   body:   <body> ; goto step
    ///   step:   i = i + 1 ; goto header      <- `continue` lands here
    ///   exit:                                 <- `break` lands here
    /// ```
    /// Putting the increment in its own block is the whole reason loops survive
    /// HIR: a `continue` that jumped straight to the header would never advance
    /// the counter.
    fn for_range(
        &mut self,
        var: hir::LocalId,
        start: &hir::Expr,
        end: &hir::Expr,
        inclusive: bool,
        body: &hir::Block,
        label: Option<&str>,
    ) {
        let counter = Local(var.0);

        let start_op = self.operand(start);
        self.assign(counter, Rvalue::Use(start_op));

        // The bound is evaluated once, before the loop, so a call in the bound
        // position runs exactly one time.
        let bound = self.temp(TyId::INT);
        let end_op = self.operand(end);
        self.assign(bound, Rvalue::Use(end_op));

        let header = self.new_block();
        let body_bb = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Goto(header));

        self.switch_to(header);
        let cond = self.temp(TyId::BOOL);
        self.assign(
            cond,
            Rvalue::Binary {
                op: if inclusive { BinOp::LeInt } else { BinOp::LtInt },
                lhs: Operand::Local(counter),
                rhs: Operand::Local(bound),
            },
        );
        self.terminate(Terminator::Branch {
            cond: Operand::Local(cond),
            then: body_bb,
            else_: exit,
        });

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            continue_to: step,
            break_to: exit,
        });

        self.switch_to(body_bb);
        self.block(body);
        self.terminate(Terminator::Goto(step));

        self.loops.pop();

        self.switch_to(step);
        self.assign(
            counter,
            Rvalue::Binary {
                op: BinOp::AddInt,
                lhs: Operand::Local(counter),
                rhs: Operand::Int(1),
            },
        );
        self.terminate(Terminator::Goto(header));

        self.switch_to(exit);
    }

    /// `for x in xs` becomes an index walk. The slice and its length are
    /// evaluated once, before the loop, so neither is recomputed per iteration.
    fn for_slice(
        &mut self,
        var: hir::LocalId,
        slice: &hir::Expr,
        body: &hir::Block,
        label: Option<&str>,
    ) {
        let seq = self.temp(slice.ty);
        let s = self.operand(slice);
        self.assign(seq, Rvalue::Use(s));

        let len = self.temp(Ty::INT);
        self.assign(len, Rvalue::SliceLen { base: Operand::Local(seq) });

        let idx = self.temp(Ty::INT);
        self.assign(idx, Rvalue::Use(Operand::Int(0)));

        let header = self.new_block();
        let body_bb = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Goto(header));

        self.switch_to(header);
        let cond = self.temp(Ty::BOOL);
        self.assign(
            cond,
            Rvalue::Binary {
                op: BinOp::LtInt,
                lhs: Operand::Local(idx),
                rhs: Operand::Local(len),
            },
        );
        self.terminate(Terminator::Branch {
            cond: Operand::Local(cond),
            then: body_bb,
            else_: exit,
        });

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            continue_to: step,
            break_to: exit,
        });

        self.switch_to(body_bb);
        self.assign(
            Local(var.0),
            Rvalue::IndexGet {
                base: Operand::Local(seq),
                index: Operand::Local(idx),
            },
        );
        self.block(body);
        self.terminate(Terminator::Goto(step));

        self.loops.pop();

        self.switch_to(step);
        self.assign(
            idx,
            Rvalue::Binary {
                op: BinOp::AddInt,
                lhs: Operand::Local(idx),
                rhs: Operand::Int(1),
            },
        );
        self.terminate(Terminator::Goto(header));

        self.switch_to(exit);
    }

    fn while_loop(&mut self, cond: &hir::Expr, body: &hir::Block, label: Option<&str>) {
        let header = self.new_block();
        let body_bb = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Goto(header));

        self.switch_to(header);
        let c = self.operand(cond);
        self.terminate(Terminator::Branch {
            cond: c,
            then: body_bb,
            else_: exit,
        });

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            continue_to: header,
            break_to: exit,
        });

        self.switch_to(body_bb);
        self.block(body);
        self.terminate(Terminator::Goto(header));

        self.loops.pop();
        self.switch_to(exit);
    }

    fn infinite_loop(&mut self, body: &hir::Block, label: Option<&str>) {
        let header = self.new_block();
        let exit = self.new_block();

        self.terminate(Terminator::Goto(header));

        self.loops.push(LoopCtx {
            label: label.map(str::to_string),
            continue_to: header,
            break_to: exit,
        });

        self.switch_to(header);
        self.block(body);
        self.terminate(Terminator::Goto(header));

        self.loops.pop();
        self.switch_to(exit);
    }

    // ---- expressions ------------------------------------------------------

    /// Lower to an rvalue, which may be a compound operation.
    fn rvalue(&mut self, e: &hir::Expr) -> Rvalue {
        match &e.kind {
            hir::ExprKind::Binary { op, lhs, rhs } if op.is_short_circuit() => {
                let t = self.short_circuit(*op, lhs, rhs);
                Rvalue::Use(Operand::Local(t))
            }
            hir::ExprKind::Binary { op, lhs, rhs } => {
                let l = self.operand(lhs);
                let r = self.operand(rhs);
                Rvalue::Binary { op: *op, lhs: l, rhs: r }
            }
            hir::ExprKind::Unary { op, operand } => {
                let o = self.operand(operand);
                Rvalue::Unary { op: *op, operand: o }
            }
            // Monomorphisation has already run, so every call is concrete and
            // `targs` is empty.
            hir::ExprKind::Call { callee, args, .. } => {
                let args = args.iter().map(|a| self.operand(a)).collect();
                Rvalue::Call { callee: FnId(callee.0), args }
            }
            hir::ExprKind::CallVirtual { trait_id, method, args } => {
                let args = args.iter().map(|a| self.operand(a)).collect();
                Rvalue::CallVirtual { trait_id: *trait_id, method: *method, args }
            }
            hir::ExprKind::ClosureNew { func, captures, .. } => Rvalue::ClosureNew {
                func: FnId(func.0),
                captures: captures.iter().map(|c| self.operand(c)).collect(),
            },

            hir::ExprKind::CallClosure { callee, args } => Rvalue::CallClosure {
                callee: self.operand(callee),
                args: args.iter().map(|a| self.operand(a)).collect(),
            },

            hir::ExprKind::StrOp { op, args } => Rvalue::StrOp {
                op: *op,
                args: args.iter().map(|a| self.operand(a)).collect(),
            },

            hir::ExprKind::Cast { value, to } => Rvalue::Cast {
                operand: self.operand(value),
                from: value.ty,
                to: *to,
            },

            hir::ExprKind::ToStr { value } => {
                Rvalue::ToStr { operand: self.operand(value), from: value.ty }
            }

            // A trait object carries its value unchanged; the receiver's own
            // type tag is what dispatch reads.
            hir::ExprKind::ToDyn { value, .. } => Rvalue::Use(self.operand(value)),
            hir::ExprKind::CallBuiltin { builtin, args } => {
                let args = args.iter().map(|a| self.operand(a)).collect();
                Rvalue::CallBuiltin { builtin: *builtin, args }
            }
            hir::ExprKind::PairNew { value, error } => {
                let v = self.operand(value);
                let e = self.operand(error);
                Rvalue::PairNew { value: v, error: e }
            }
            hir::ExprKind::PairValue { base } => {
                let b = self.operand(base);
                Rvalue::PairValue { base: b }
            }
            hir::ExprKind::PairError { base } => {
                let b = self.operand(base);
                Rvalue::PairError { base: b }
            }
            hir::ExprKind::ErrorNew { message } => {
                let m = self.operand(message);
                Rvalue::ErrorNew { message: m }
            }
            hir::ExprKind::ErrorMessage { base } => {
                let b = self.operand(base);
                Rvalue::ErrorMessage { base: b }
            }
            hir::ExprKind::Wrap { value } => {
                let v = self.operand(value);
                Rvalue::Wrap { value: v }
            }
            hir::ExprKind::Unwrap { value } => {
                let v = self.operand(value);
                Rvalue::Unwrap { value: v }
            }
            hir::ExprKind::IsNil { value } => {
                let v = self.operand(value);
                Rvalue::IsNil { value: v }
            }
            hir::ExprKind::MapNew { entries } => {
                let entries = entries.iter().map(|a| self.operand(a)).collect();
                Rvalue::MapNew { entries }
            }
            hir::ExprKind::MapGet { base, key } => {
                let b = self.operand(base);
                let k = self.operand(key);
                Rvalue::MapGet { base: b, key: k }
            }
            hir::ExprKind::MapLen { base } => {
                let b = self.operand(base);
                Rvalue::MapLen { base: b }
            }
            hir::ExprKind::TupleNew { elems } => {
                let elems = elems.iter().map(|a| self.operand(a)).collect();
                Rvalue::TupleNew { elems }
            }
            hir::ExprKind::SliceNew { elems } => {
                let elems = elems.iter().map(|a| self.operand(a)).collect();
                Rvalue::SliceNew { elems }
            }
            hir::ExprKind::Index { base, index } => {
                let b = self.operand(base);
                let i = self.operand(index);
                Rvalue::IndexGet { base: b, index: i }
            }
            hir::ExprKind::SliceLen { base } => {
                let b = self.operand(base);
                Rvalue::SliceLen { base: b }
            }
            hir::ExprKind::SliceGet { base, index } => {
                let b = self.operand(base);
                let i = self.operand(index);
                Rvalue::SliceGet { base: b, index: i }
            }
            hir::ExprKind::EnumNew { enum_id, variant, fields } => {
                let fields = fields.iter().map(|a| self.operand(a)).collect();
                Rvalue::EnumNew { enum_id: *enum_id, variant: *variant, fields }
            }
            hir::ExprKind::StructNew { struct_id, fields } => {
                let fields = fields.iter().map(|a| self.operand(a)).collect();
                Rvalue::StructNew { struct_id: *struct_id, fields }
            }
            hir::ExprKind::FieldGet { base, index } => {
                let b = self.operand(base);
                Rvalue::FieldGet { base: b, index: *index }
            }
            _ => Rvalue::Use(self.operand(e)),
        }
    }

    /// Lower to a simple operand, introducing a temporary if the expression is
    /// compound.
    fn operand(&mut self, e: &hir::Expr) -> Operand {
        match &e.kind {
            hir::ExprKind::Int(v) => Operand::Int(*v),
            hir::ExprKind::Float(v) => Operand::Float(*v),
            hir::ExprKind::Bool(v) => Operand::Bool(*v),
            hir::ExprKind::Str(s) => Operand::Str(self.strings.intern(s)),
            hir::ExprKind::Local(l) => Operand::Local(Local(l.0)),
            hir::ExprKind::Error => Operand::Unit,
            hir::ExprKind::Nil => Operand::Nil,

            hir::ExprKind::If { cond, then, else_ } => {
                Operand::Local(self.if_expr(cond, then, else_, e.ty))
            }
            hir::ExprKind::Binary { op, lhs, rhs } if op.is_short_circuit() => {
                Operand::Local(self.short_circuit(*op, lhs, rhs))
            }
            hir::ExprKind::Match { scrutinee, arms } => {
                Operand::Local(self.match_expr(scrutinee, arms, e.ty))
            }
            hir::ExprKind::Block(b) => {
                self.block(b);
                Operand::Unit
            }
            _ => {
                let v = self.rvalue(e);
                let t = self.temp(e.ty);
                self.assign(t, v);
                Operand::Local(t)
            }
        }
    }

    // ---- match ------------------------------------------------------------

    /// Arms are tested in order, each falling through to the next on failure.
    ///
    /// ```text
    ///   test_0: <pattern test> ? bind_0 : test_1
    ///   bind_0: <bind names> ; <guard> ? body_0 : test_1
    ///   body_0: result = <body> ; goto join
    ///   ...
    ///   fail:   unreachable        <- exhaustiveness proved this is dead
    ///   join:
    /// ```
    ///
    /// A decision tree that shares tests across arms is an optimisation MIR can
    /// add later; sequential testing is what makes the semantics obvious.
    fn match_expr(
        &mut self,
        scrutinee: &hir::Expr,
        arms: &[hir::MatchArm],
        result_ty: Ty,
    ) -> Local {
        let subject = self.operand(scrutinee);
        let result = self.temp(result_ty);
        let join = self.new_block();

        // The checker proved the arms cover every value, so falling past the
        // last one cannot happen.
        let fail = self.new_block();

        for (i, arm) in arms.iter().enumerate() {
            let next = if i + 1 < arms.len() {
                self.new_block()
            } else {
                fail
            };

            let body_bb = self.new_block();
            self.test_pattern(&arm.pattern, &subject, body_bb, next);

            self.switch_to(body_bb);
            // Bindings are written only once the pattern has matched, so a
            // failed arm never leaves a half-written local behind.
            self.bind_pattern(&arm.pattern, &subject);

            if let Some(g) = &arm.guard {
                let guarded = self.new_block();
                let c = self.operand(g);
                self.terminate(Terminator::Branch {
                    cond: c,
                    then: guarded,
                    else_: next,
                });
                self.switch_to(guarded);
            }

            let v = self.operand(&arm.body);
            self.assign(result, Rvalue::Use(v));
            self.terminate(Terminator::Goto(join));

            if next != fail {
                self.switch_to(next);
            }
        }

        self.switch_to(fail);
        self.terminate(Terminator::Unreachable);

        self.switch_to(join);
        result
    }

    /// Branch to `on_match` when `pattern` accepts `subject`, else `on_fail`.
    fn test_pattern(
        &mut self,
        pattern: &hir::Pattern,
        subject: &Operand,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        if pattern.is_irrefutable() {
            self.terminate(Terminator::Goto(on_match));
            return;
        }

        match pattern {
            hir::Pattern::Int(v) => self.test_eq(subject, Operand::Int(*v), BinOp::EqInt, on_match, on_fail),
            hir::Pattern::Float(v) => {
                self.test_eq(subject, Operand::Float(*v), BinOp::EqFloat, on_match, on_fail)
            }
            hir::Pattern::Bool(v) => {
                self.test_eq(subject, Operand::Bool(*v), BinOp::EqBool, on_match, on_fail)
            }
            hir::Pattern::Str(s) => {
                let id = self.strings.intern(s);
                self.test_eq(subject, Operand::Str(id), BinOp::EqStr, on_match, on_fail)
            }

            hir::Pattern::IntRange { start, end, inclusive } => {
                let lo = self.temp(Ty::BOOL);
                self.assign(
                    lo,
                    Rvalue::Binary {
                        op: BinOp::GeInt,
                        lhs: subject.clone(),
                        rhs: Operand::Int(*start),
                    },
                );
                let upper = self.new_block();
                self.terminate(Terminator::Branch {
                    cond: Operand::Local(lo),
                    then: upper,
                    else_: on_fail,
                });

                self.switch_to(upper);
                let hi = self.temp(Ty::BOOL);
                self.assign(
                    hi,
                    Rvalue::Binary {
                        op: if *inclusive { BinOp::LeInt } else { BinOp::LtInt },
                        lhs: subject.clone(),
                        rhs: Operand::Int(*end),
                    },
                );
                self.terminate(Terminator::Branch {
                    cond: Operand::Local(hi),
                    then: on_match,
                    else_: on_fail,
                });
            }

            hir::Pattern::Variant { enum_id, variant, fields } => {
                let tag = self.temp(Ty::INT);
                self.assign(tag, Rvalue::TagOf { base: subject.clone() });
                let hit = self.temp(Ty::BOOL);
                self.assign(
                    hit,
                    Rvalue::Binary {
                        op: BinOp::EqInt,
                        lhs: Operand::Local(tag),
                        rhs: Operand::Int(*variant as i64),
                    },
                );

                // Nested patterns are tested only once the tag matches, so
                // reading a payload is always safe.
                let refutable: Vec<(usize, &hir::Pattern)> = fields
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !p.is_irrefutable())
                    .collect();

                if refutable.is_empty() {
                    self.terminate(Terminator::Branch {
                        cond: Operand::Local(hit),
                        then: on_match,
                        else_: on_fail,
                    });
                    return;
                }

                let payload_bb = self.new_block();
                self.terminate(Terminator::Branch {
                    cond: Operand::Local(hit),
                    then: payload_bb,
                    else_: on_fail,
                });
                self.switch_to(payload_bb);
                self.test_fields(
                    subject,
                    &refutable,
                    FieldOwner::Variant(*enum_id, *variant),
                    on_match,
                    on_fail,
                );
            }

            hir::Pattern::Struct { struct_id, fields } => {
                let refutable: Vec<(usize, &hir::Pattern)> = fields
                    .iter()
                    .filter(|(_, p)| !p.is_irrefutable())
                    .map(|(i, p)| (*i as usize, p))
                    .collect();
                if refutable.is_empty() {
                    self.terminate(Terminator::Goto(on_match));
                    return;
                }
                self.test_fields(
                    subject,
                    &refutable,
                    FieldOwner::Struct(*struct_id),
                    on_match,
                    on_fail,
                );
            }

            // Any alternative matching is enough.
            hir::Pattern::Or(alts) => {
                for (i, alt) in alts.iter().enumerate() {
                    let next = if i + 1 < alts.len() {
                        self.new_block()
                    } else {
                        on_fail
                    };
                    self.test_pattern(alt, subject, on_match, next);
                    if next != on_fail {
                        self.switch_to(next);
                    }
                }
            }

            hir::Pattern::Tuple { ty, elems } => {
                let refutable: Vec<(usize, &hir::Pattern)> = elems
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !p.is_irrefutable())
                    .collect();
                if refutable.is_empty() {
                    self.terminate(Terminator::Goto(on_match));
                    return;
                }
                self.test_fields(subject, &refutable, FieldOwner::Tuple(*ty), on_match, on_fail);
            }

            hir::Pattern::Nil => {
                let c = self.temp(Ty::BOOL);
                self.assign(c, Rvalue::IsNil { value: subject.clone() });
                self.terminate(Terminator::Branch {
                    cond: Operand::Local(c),
                    then: on_match,
                    else_: on_fail,
                });
            }

            hir::Pattern::Wildcard | hir::Pattern::Binding { .. } => {
                self.terminate(Terminator::Goto(on_match));
            }
        }
    }

    fn test_fields(
        &mut self,
        subject: &Operand,
        fields: &[(usize, &hir::Pattern)],
        owner: FieldOwner,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        for (n, (index, sub)) in fields.iter().enumerate() {
            let ty = self.field_type(owner, *index as u32);
            let slot = self.temp(ty);
            let read = self.read_field(subject, owner, *index as u32);
            self.assign(slot, read);
            let target = if n + 1 < fields.len() {
                self.new_block()
            } else {
                on_match
            };
            self.test_pattern(sub, &Operand::Local(slot), target, on_fail);
            if target != on_match {
                self.switch_to(target);
            }
        }
    }

    fn test_eq(
        &mut self,
        subject: &Operand,
        constant: Operand,
        op: BinOp,
        on_match: BlockId,
        on_fail: BlockId,
    ) {
        let c = self.temp(Ty::BOOL);
        self.assign(
            c,
            Rvalue::Binary { op, lhs: subject.clone(), rhs: constant },
        );
        self.terminate(Terminator::Branch {
            cond: Operand::Local(c),
            then: on_match,
            else_: on_fail,
        });
    }

    /// Write the pattern's bindings, once it is known to have matched.
    fn bind_pattern(&mut self, pattern: &hir::Pattern, subject: &Operand) {
        match pattern {
            hir::Pattern::Binding { local, unwrap } => {
                let value = if *unwrap {
                    Rvalue::Unwrap { value: subject.clone() }
                } else {
                    Rvalue::Use(subject.clone())
                };
                self.assign(Local(local.0), value);
            }
            hir::Pattern::Variant { enum_id, variant, fields } => {
                for (i, sub) in fields.iter().enumerate() {
                    self.bind_field(sub, subject, FieldOwner::Variant(*enum_id, *variant), i as u32);
                }
            }
            hir::Pattern::Struct { struct_id, fields } => {
                for (i, sub) in fields {
                    self.bind_field(sub, subject, FieldOwner::Struct(*struct_id), *i);
                }
            }
            hir::Pattern::Tuple { ty, elems } => {
                for (i, sub) in elems.iter().enumerate() {
                    self.bind_field(sub, subject, FieldOwner::Tuple(*ty), i as u32);
                }
            }
            // Every alternative of an or-pattern must bind the same names, so
            // binding through the first is enough.
            hir::Pattern::Or(alts) => {
                if let Some(first) = alts.first() {
                    self.bind_pattern(first, subject);
                }
            }
            _ => {}
        }
    }

    fn bind_field(
        &mut self,
        sub: &hir::Pattern,
        subject: &Operand,
        owner: FieldOwner,
        index: u32,
    ) {
        if matches!(sub, hir::Pattern::Wildcard) {
            return;
        }
        let ty = self.field_type(owner, index);
        let slot = self.temp(ty);
        let read = self.read_field(subject, owner, index);
        self.assign(slot, read);
        self.bind_pattern(sub, &Operand::Local(slot));
    }

    /// Read a field, naming the variant when the subject is an enum so a
    /// backend that subtypes its variants knows what to cast to.
    fn read_field(&self, subject: &Operand, owner: FieldOwner, index: u32) -> Rvalue {
        match owner {
            FieldOwner::Variant(enum_id, variant) => Rvalue::VariantGet {
                base: subject.clone(),
                enum_id,
                variant,
                index,
            },
            FieldOwner::Struct(_) | FieldOwner::Tuple(_) => {
                Rvalue::FieldGet { base: subject.clone(), index }
            }
        }
    }

    fn field_type(&self, owner: FieldOwner, index: u32) -> Ty {
        let fields = match owner {
            FieldOwner::Variant(e, v) => &self.types.enum_def(e).variants[v as usize].fields,
            FieldOwner::Struct(s) => &self.types.struct_def(s).fields,
            FieldOwner::Tuple(ty) => {
                return match self.types.kind(ty) {
                    kite_hir::TyKind::Tuple(elems) => {
                        elems.get(index as usize).copied().unwrap_or(Ty::ERROR)
                    }
                    _ => Ty::ERROR,
                }
            }
        };
        fields.get(index as usize).map(|f| f.ty).unwrap_or(Ty::ERROR)
    }

    /// `a && b` must not evaluate `b` when `a` is false, so it becomes a
    /// branch rather than an instruction.
    fn short_circuit(&mut self, op: BinOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Local {
        let result = self.temp(TyId::BOOL);
        let l = self.operand(lhs);
        self.assign(result, Rvalue::Use(l.clone()));

        let rhs_bb = self.new_block();
        let join = self.new_block();

        // `&&` evaluates the right side when the left is true; `||` when false.
        let (then, else_) = match op {
            BinOp::And => (rhs_bb, join),
            BinOp::Or => (join, rhs_bb),
            _ => unreachable!("not a short-circuit operator"),
        };
        self.terminate(Terminator::Branch { cond: l, then, else_ });

        self.switch_to(rhs_bb);
        let r = self.operand(rhs);
        self.assign(result, Rvalue::Use(r));
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
        result
    }

    fn if_expr(
        &mut self,
        cond: &hir::Expr,
        then: &hir::Expr,
        else_: &hir::Expr,
        ty: TyId,
    ) -> Local {
        let result = self.temp(ty);
        let c = self.operand(cond);

        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join = self.new_block();

        self.terminate(Terminator::Branch {
            cond: c,
            then: then_bb,
            else_: else_bb,
        });

        self.switch_to(then_bb);
        let t = self.operand(then);
        self.assign(result, Rvalue::Use(t));
        self.terminate(Terminator::Goto(join));

        self.switch_to(else_bb);
        let e = self.operand(else_);
        self.assign(result, Rvalue::Use(e));
        self.terminate(Terminator::Goto(join));

        self.switch_to(join);
        result
    }
}
