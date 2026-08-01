//! HIR to MIR: control flow becomes a graph.

use crate::*;
use kite_hir::TyId;
use kite_hir as hir;
use std::collections::HashMap;

pub fn lower(program: &hir::Program) -> Program {
    let mut out = Program {
        entry: program.entry.map(|e| FnId(e.0)),
        ..Default::default()
    };
    let mut strings = StringPool::default();

    for func in &program.fns {
        let lowered = FnLowerer::new(func, &mut strings).run();
        out.fns.push(lowered);
    }
    out.strings = strings.list;
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

struct FnLowerer<'a> {
    hir_fn: &'a hir::Function,
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
    fn new(hir_fn: &'a hir::Function, strings: &'a mut StringPool) -> Self {
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
            hir::ExprKind::Call { callee, args } => {
                let args = args.iter().map(|a| self.operand(a)).collect();
                Rvalue::Call { callee: FnId(callee.0), args }
            }
            hir::ExprKind::CallBuiltin { builtin, args } => {
                let args = args.iter().map(|a| self.operand(a)).collect();
                Rvalue::CallBuiltin { builtin: *builtin, args }
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

            hir::ExprKind::If { cond, then, else_ } => {
                Operand::Local(self.if_expr(cond, then, else_, e.ty))
            }
            hir::ExprKind::Binary { op, lhs, rhs } if op.is_short_circuit() => {
                Operand::Local(self.short_circuit(*op, lhs, rhs))
            }
            _ => {
                let v = self.rvalue(e);
                let t = self.temp(e.ty);
                self.assign(t, v);
                Operand::Local(t)
            }
        }
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
