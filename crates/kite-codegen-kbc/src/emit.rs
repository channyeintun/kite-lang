//! MIR to bytecode.
//!
//! MIR locals become registers one-for-one. Above them sits an
//! outgoing-argument window, because the call convention requires arguments in
//! consecutive registers.
//!
//! Jumps are emitted against block ids and patched to instruction offsets in a
//! second pass, which keeps the emitter a single forward walk.

use crate::*;
use kite_mir as mir;

pub fn compile(program: &mir::Program) -> Chunk {
    Chunk {
        functions: program.fns.iter().map(compile_fn).collect(),
        strings: program.strings.iter().map(|s| Rc::from(s.as_str())).collect(),
        entry: program.entry.map(|e| e.0),
    }
}

fn compile_fn(func: &mir::Function) -> FnProto {
    // Registers: one per MIR local, then a window wide enough for the largest
    // call in the body.
    let max_args = func
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .map(|s| match s {
            mir::Inst::Assign { value: mir::Rvalue::Call { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::CallBuiltin { args, .. }, .. } => args.len(),
            mir::Inst::Assign { value: mir::Rvalue::StructNew { fields, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::EnumNew { fields, .. }, .. } => fields.len(),
            mir::Inst::Assign { value: mir::Rvalue::SliceNew { elems }, .. } => elems.len(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);

    let mut e = Emitter {
        func,
        code: Vec::new(),
        block_starts: vec![u32::MAX; func.blocks.len()],
        patches: Vec::new(),
        arg_base: func.locals.len() as Reg,
        scratch: (func.locals.len() + max_args) as Reg,
        extra_registers: 0,
    };
    e.run();

    let frame_size = e.scratch as usize + e.extra_registers;
    FnProto {
        name: func.name.clone(),
        param_count: func.param_count,
        frame_size,
        code: e.code,
    }
}

/// A jump whose target block is not yet resolved to an offset.
struct Patch {
    at: usize,
    block: mir::BlockId,
}

struct Emitter<'a> {
    func: &'a mir::Function,
    code: Vec<Op>,
    block_starts: Vec<u32>,
    patches: Vec<Patch>,
    /// First register of the outgoing-argument window.
    arg_base: Reg,
    /// First register above the argument window, used for constant staging.
    scratch: Reg,
    extra_registers: usize,
}

impl<'a> Emitter<'a> {
    fn run(&mut self) {
        let reachable = mir::reachable_blocks(self.func);

        for (i, block) in self.func.blocks.iter().enumerate() {
            self.block_starts[i] = self.code.len() as u32;
            if !reachable[i] {
                // Emit nothing for dead blocks, but keep an offset so any
                // stale reference still lands somewhere defined.
                continue;
            }

            for stmt in &block.stmts {
                self.stmt(stmt);
            }
            self.terminator(&block.term, i);
        }

        for p in &self.patches {
            let target = self.block_starts[p.block.index()];
            match &mut self.code[p.at] {
                Op::Jump { target: t } | Op::JumpIfFalse { target: t, .. } => *t = target,
                other => unreachable!("patch points at {:?}, not a jump", other),
            }
        }
    }

    // ---- statements -------------------------------------------------------

    fn stmt(&mut self, s: &mir::Inst) {
        let (dst, value) = match s {
            mir::Inst::Assign { dst, value } => (reg(*dst), value),
            mir::Inst::SetField { base, index, value } => {
                let obj = self.operand_reg(base, 0);
                let src = self.operand_reg(value, 1);
                self.code.push(Op::SetField { obj, index: *index as u16, src });
                return;
            }
            mir::Inst::SetIndex { base, index, value } => {
                let obj = self.operand_reg(base, 0);
                let idx = self.operand_reg(index, 1);
                let src = self.operand_reg(value, 2);
                self.code.push(Op::SetIndex { obj, index: idx, src });
                return;
            }
            mir::Inst::SlicePush { local, value } => {
                let src = self.operand_reg(value, 0);
                self.code.push(Op::SlicePush { obj: reg(*local), src });
                return;
            }
        };

        match value {
            mir::Rvalue::Use(o) => self.load_into(dst, o),

            mir::Rvalue::Binary { op, lhs, rhs } => {
                let a = self.operand_reg(lhs, 0);
                let b = self.operand_reg(rhs, 1);
                match binop_instruction(*op) {
                    Some(make) => self.code.push(make(dst, a, b)),
                    // MIR has already lowered && and || to branches, so
                    // reaching here would be a lowering bug.
                    None => self.code.push(Op::Unreachable),
                }
            }

            mir::Rvalue::Unary { op, operand } => {
                let a = self.operand_reg(operand, 0);
                self.code.push(unop_instruction(*op)(dst, a));
            }

            mir::Rvalue::Call { callee, args } => {
                self.stage_args(args);
                self.code.push(Op::Call {
                    dst,
                    func: callee.0,
                    base: self.arg_base,
                    argc: args.len() as u8,
                });
            }

            mir::Rvalue::CallBuiltin { builtin, args } => {
                self.stage_args(args);
                self.code.push(Op::CallNative {
                    dst,
                    native: Native::from_builtin(*builtin),
                    base: self.arg_base,
                    argc: args.len() as u8,
                });
            }

            // Field values are staged in the same consecutive window the call
            // convention uses, so no separate mechanism is needed.
            mir::Rvalue::StructNew { struct_id, fields } => {
                self.stage_args(fields);
                self.code.push(Op::NewStruct {
                    dst,
                    struct_id: struct_id.0,
                    base: self.arg_base,
                    count: fields.len() as u8,
                });
            }

            mir::Rvalue::FieldGet { base, index } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::GetField { dst, obj, index: *index as u16 });
            }

            mir::Rvalue::EnumNew { enum_id, variant, fields } => {
                self.stage_args(fields);
                self.code.push(Op::NewEnum {
                    dst,
                    enum_id: enum_id.0,
                    variant: *variant,
                    base: self.arg_base,
                    count: fields.len() as u8,
                });
            }

            mir::Rvalue::TagOf { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::TagOf { dst, obj });
            }

            mir::Rvalue::PairNew { value, error } => {
                let v = self.operand_reg(value, 0);
                let e = self.operand_reg(error, 1);
                self.code.push(Op::NewPair { dst, value: v, error: e });
            }
            mir::Rvalue::PairValue { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::PairValue { dst, obj });
            }
            mir::Rvalue::PairError { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::PairError { dst, obj });
            }
            mir::Rvalue::ErrorNew { message } => {
                let m = self.operand_reg(message, 0);
                self.code.push(Op::NewError { dst, message: m });
            }
            mir::Rvalue::ErrorMessage { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::ErrorMessage { dst, obj });
            }
            mir::Rvalue::IsNil { value } => {
                let obj = self.operand_reg(value, 0);
                self.code.push(Op::IsNil { dst, obj });
            }

            mir::Rvalue::SliceNew { elems } => {
                self.stage_args(elems);
                self.code.push(Op::NewSlice {
                    dst,
                    base: self.arg_base,
                    count: elems.len() as u8,
                });
            }
            mir::Rvalue::IndexGet { base, index } => {
                let obj = self.operand_reg(base, 0);
                let idx = self.operand_reg(index, 1);
                self.code.push(Op::GetIndex { dst, obj, index: idx });
            }
            mir::Rvalue::SliceLen { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::SliceLen { dst, obj });
            }
            mir::Rvalue::SliceGet { base, index } => {
                let obj = self.operand_reg(base, 0);
                let idx = self.operand_reg(index, 1);
                self.code.push(Op::SliceGet { dst, obj, index: idx });
            }
        }
    }

    /// Move arguments into the consecutive window the call convention wants.
    fn stage_args(&mut self, args: &[mir::Operand]) {
        for (i, a) in args.iter().enumerate() {
            let slot = self.arg_base + i as Reg;
            self.load_into(slot, a);
        }
    }

    fn load_into(&mut self, dst: Reg, o: &mir::Operand) {
        let op = match o {
            mir::Operand::Local(l) => Op::Move { dst, src: reg(*l) },
            mir::Operand::Int(v) => Op::LoadInt { dst, value: *v },
            mir::Operand::Float(v) => Op::LoadFloat { dst, value: *v },
            mir::Operand::Bool(v) => Op::LoadBool { dst, value: *v },
            mir::Operand::Str(s) => Op::LoadStr { dst, idx: s.0 },
            mir::Operand::Unit => Op::LoadUnit { dst },
            mir::Operand::Nil => Op::LoadNil { dst },
        };
        // `move rN, rN` is a no-op worth not emitting.
        if let Op::Move { dst: d, src } = &op {
            if d == src {
                return;
            }
        }
        self.code.push(op);
    }

    /// A register holding `o`, materialising a constant into scratch if needed.
    /// `slot` distinguishes the two operands of a binary op so they do not
    /// collide.
    fn operand_reg(&mut self, o: &mir::Operand, slot: usize) -> Reg {
        if let mir::Operand::Local(l) = o {
            return reg(*l);
        }
        let r = self.scratch + slot as Reg;
        self.extra_registers = self.extra_registers.max(slot + 1);
        self.load_into(r, o);
        r
    }

    // ---- terminators ------------------------------------------------------

    fn terminator(&mut self, t: &mir::Terminator, block_index: usize) {
        match t {
            mir::Terminator::Goto(target) => {
                // Falling through to the next block beats jumping to it.
                if target.index() != block_index + 1 {
                    self.emit_jump(Op::Jump { target: 0 }, *target);
                }
            }

            mir::Terminator::Branch { cond, then, else_ } => {
                let c = self.operand_reg(cond, 0);
                // Branch on false to the else block, then fall through or jump
                // to the then block.
                self.emit_jump(Op::JumpIfFalse { cond: c, target: 0 }, *else_);
                if then.index() != block_index + 1 {
                    self.emit_jump(Op::Jump { target: 0 }, *then);
                }
            }

            mir::Terminator::Return(v) => {
                let src = v.as_ref().map(|o| self.operand_reg(o, 0));
                self.code.push(Op::Return { src });
            }

            mir::Terminator::Unreachable => self.code.push(Op::Unreachable),
        }
    }

    fn emit_jump(&mut self, op: Op, block: mir::BlockId) {
        self.patches.push(Patch { at: self.code.len(), block });
        self.code.push(op);
    }
}

fn reg(l: mir::Local) -> Reg {
    debug_assert!(l.0 <= Reg::MAX as u32, "register index overflows u16");
    l.0 as Reg
}
