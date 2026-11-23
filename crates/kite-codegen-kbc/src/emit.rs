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

pub fn compile(program: &mir::Program, types: &kite_hir::Types) -> Chunk {
    // A virtual call names its trait; the emitter needs a table index. Both
    // sides walk `program.vtables` in the same order, so position is the index.
    let traits: Vec<u32> = program.vtables.iter().map(|v| v.trait_id.0).collect();
    Chunk {
        functions: {
            // Which parameters each function keeps, and which calls are
            // quiet, decide which reads of a field only glance at it.
            let kept = mir::Kept::of(program);
            program.fns.iter().map(|f| compile_fn(f, &traits, types, &kept)).collect()
        },
        externs: program
            .externs
            .iter()
            .map(|e| format!("{}.{}", e.host, e.name))
            .collect(),
        extern_sigs: program.extern_sigs.clone(),
        strings: program.strings.iter().map(|s| Rc::from(s.as_str())).collect(),
        entry: program.entry.map(|e| e.0),
        vtables: program
            .vtables
            .iter()
            .map(|v| {
                let mut rows: Vec<(u32, Vec<u32>)> = v
                    .entries
                    .iter()
                    .map(|e| (e.tag.encode(), e.methods.iter().map(|m| m.0).collect()))
                    .collect();
                // `lookup` binary-searches, so the rows must be sorted by tag.
                rows.sort_by_key(|(t, _)| *t);
                VTable { rows }
            })
            .collect(),
        mutable_structs: program.mutable_structs.clone(),
    }
}

/// Registers a frame may have: a register index is a [`Reg`], so one past
/// the largest.
pub const MAX_REGISTERS: usize = Reg::MAX as usize + 1;

/// Scratch registers above the argument window: the most constant operands
/// one instruction stages at once, which is an error's four.
const SCRATCH: usize = 4;

/// Something in a program the bytecode encoding cannot represent.
#[derive(Clone, Debug)]
pub struct Limit {
    /// The function it is in, as named in MIR.
    pub function: String,
    pub span: kite_span::Span,
    pub what: String,
}

/// Every function whose frame would not fit the register encoding.
///
/// A register index is sixteen bits, and a frame holds every MIR local, then
/// the widest argument window the body stages, then a little scratch. A frame
/// wider than that used to be emitted anyway, with each index silently cut to
/// sixteen bits — so one local aliased another and the VM read a register that
/// was not there. The driver asks this first, and reports what it finds as a
/// diagnostic rather than emitting bytecode that means something else.
///
/// The argument window is also where a literal's elements are staged, so this
/// bounds a struct or tuple literal too: a count is as wide as a register
/// index. A slice or map literal stages at most [`mir::LITERAL_WINDOW`] items
/// and adds the rest one at a time, so its length is not a limit.
pub fn limits(program: &mir::Program) -> Vec<Limit> {
    let mut found = Vec::new();
    for func in &program.fns {
        let needed = func.locals.len() + widest_window(func) + SCRATCH;
        if needed > MAX_REGISTERS {
            found.push(Limit {
                function: func.name.clone(),
                span: func.span,
                what: format!(
                    "a frame of {} registers, more than the {} the bytecode can address",
                    needed, MAX_REGISTERS
                ),
            });
        }
    }
    found
}

/// The most consecutive registers any one instruction in the body stages:
/// the arguments of the widest call, or the elements of the widest literal.
fn widest_window(func: &mir::Function) -> usize {
    func
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .map(|s| match s {
            mir::Inst::Assign { value: mir::Rvalue::Call { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::CallClosure { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::ClosureNew { captures: args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::StrOp { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::CallVirtual { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::CallBuiltin { args, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::CallExtern { args, .. }, .. } => args.len(),
            mir::Inst::Assign { value: mir::Rvalue::StructNew { fields, .. }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::EnumNew { fields, .. }, .. } => fields.len(),
            mir::Inst::Assign { value: mir::Rvalue::TupleNew { elems }, .. } => elems.len(),
            mir::Inst::Assign { value: mir::Rvalue::SliceNew { elems: items }, .. }
            | mir::Inst::Assign { value: mir::Rvalue::MapNew { entries: items }, .. } => {
                items.len().min(mir::LITERAL_WINDOW)
            }
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

fn compile_fn(
    func: &mir::Function,
    traits: &[u32],
    types: &kite_hir::Types,
    kept: &mir::Kept,
) -> FnProto {
    // Registers: one per MIR local, then a window wide enough for the largest
    // call in the body.
    let max_args = widest_window(func);
    // `limits` is asked first, so reaching here with a frame too wide to
    // address is a driver that skipped it — a compiler bug, and one that must
    // not become registers silently aliasing each other.
    assert!(
        func.locals.len() + max_args + SCRATCH <= MAX_REGISTERS,
        "`{}` needs more registers than the bytecode can address; \
         `kite_codegen_kbc::limits` reports this before compiling",
        func.name
    );

    let mut e = Emitter {
        func,
        traits,
        types,
        code: Vec::new(),
        block_starts: vec![u32::MAX; func.blocks.len()],
        patches: Vec::new(),
        arg_base: func.locals.len() as Reg,
        scratch: (func.locals.len() + max_args) as Reg,
        extra_registers: 0,
        emptied: emptied_after(func, kept, types),
        taken: taken_locals(func),
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
    /// Trait ids in table order, for turning a `TraitId` into a table index.
    traits: &'a [u32],
    /// Asked only where an optional's payload is an `error`, the one payload
    /// whose own value can be nil.
    types: &'a kite_hir::Types,
    code: Vec<Op>,
    block_starts: Vec<u32>,
    patches: Vec<Patch>,
    /// First register of the outgoing-argument window.
    arg_base: Reg,
    /// First register above the argument window, used for constant staging.
    scratch: Reg,
    extra_registers: usize,
    /// By block, the statements after which a local that glanced at a
    /// field's slice or map is emptied, and the locals: see [`emptied_after`].
    emptied: Vec<Vec<(usize, Reg)>>,
    /// The locals a `FieldTake` gives a field's slice or map to, emptied once
    /// the write-back has put it back.
    taken: Vec<bool>,
}

/// After which statement of each block each local that only glanced at a
/// field's slice or map (`kite_mir::held_reads`) is read for the last time.
///
/// The VM's sharing is `Rc`'s count, so a register still holding what it read
/// out of `q.items` would make the field's slice look shared, and the next
/// `q.items.push(v)` a copy of it: `if q.items.len() < n { q.items.push(v) }`
/// was quadratic for that reason alone. Emptying the register after its last
/// read leaves the field the one holder. Only a local some length, element or
/// lookup reads is emptied — that is a slice's or a map's; one compared and
/// nothing else is left alone, which costs a copy, not a wrong answer. A
/// slice or map passed to a quiet call that keeps nothing of it is emptied
/// too: the call has moved it out of the staging window and given it back.
fn emptied_after(
    func: &mir::Function,
    kept: &mir::Kept,
    types: &kite_hir::Types,
) -> Vec<Vec<(usize, Reg)>> {
    let mut container = vec![false; func.locals.len()];
    for b in &func.blocks {
        for s in &b.stmts {
            if let mir::Inst::Assign { value, .. } = s {
                if let mir::Rvalue::Call { args, .. } = value {
                    for a in args {
                        if let mir::Operand::Local(l) = a {
                            if matches!(
                                types.kind(func.locals[l.index()].ty),
                                kite_hir::TyKind::Slice(_) | kite_hir::TyKind::Map(..)
                            ) {
                                container[l.index()] = true;
                            }
                        }
                    }
                    continue;
                }
                let base = match value {
                    mir::Rvalue::SliceLen { base }
                    | mir::Rvalue::MapLen { base }
                    | mir::Rvalue::MapKeys { base }
                    | mir::Rvalue::MapValues { base }
                    | mir::Rvalue::IndexGet { base, .. }
                    | mir::Rvalue::SliceGet { base, .. }
                    | mir::Rvalue::MapGet { base, .. } => base,
                    _ => continue,
                };
                if let mir::Operand::Local(l) = base {
                    container[l.index()] = true;
                }
            }
        }
    }
    let mut out = vec![Vec::new(); func.blocks.len()];
    for (l, at) in mir::held_reads(func, kept).into_iter().enumerate() {
        if let (Some((b, s)), true) = (at, container[l]) {
            out[b].push((s, l as Reg));
        }
    }
    out
}

/// Which locals a `FieldTake` assigns.
fn taken_locals(func: &mir::Function) -> Vec<bool> {
    let mut out = vec![false; func.locals.len()];
    for b in &func.blocks {
        for s in &b.stmts {
            if let mir::Inst::Assign { dst, value: mir::Rvalue::FieldTake { .. } } = s {
                out[dst.index()] = true;
            }
        }
    }
    out
}

impl<'a> Emitter<'a> {
    fn vtable_index(&self, trait_id: kite_hir::TraitId) -> u32 {
        self.traits.iter().position(|t| *t == trait_id.0).unwrap_or(0) as u32
    }

    fn run(&mut self) {
        let reachable = mir::reachable_blocks(self.func);

        for (i, block) in self.func.blocks.iter().enumerate() {
            self.block_starts[i] = self.code.len() as u32;
            if !reachable[i] {
                // Emit nothing for dead blocks, but keep an offset so any
                // stale reference still lands somewhere defined.
                continue;
            }

            for (si, stmt) in block.stmts.iter().enumerate() {
                self.stmt(stmt);
                for k in 0..self.emptied[i].len() {
                    let (at, r) = self.emptied[i][k];
                    if at == si {
                        self.code.push(Op::LoadUnit { dst: r });
                    }
                }
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
                // A write-back: the field holds it again, and the local is
                // dead, so it should not be a second count on the next take.
                if let mir::Operand::Local(l) = value {
                    if self.taken[l.index()] {
                        self.code.push(Op::LoadUnit { dst: reg(*l) });
                    }
                }
                return;
            }
            mir::Inst::SetIndex { base, index, value } => {
                let obj = self.operand_reg(base, 0);
                let idx = self.operand_reg(index, 1);
                let src = self.operand_reg(value, 2);
                self.code.push(Op::SetIndex { obj, index: idx, src });
                return;
            }
            mir::Inst::MapSet { local, key, value } => {
                let k = self.operand_reg(key, 0);
                let v = self.operand_reg(value, 1);
                self.code.push(Op::MapSet { obj: reg(*local), key: k, src: v });
                return;
            }
            mir::Inst::MapRemove { local, key } => {
                let k = self.operand_reg(key, 0);
                self.code.push(Op::MapRemove { obj: reg(*local), key: k });
                return;
            }
            mir::Inst::SlicePush { local, value } => {
                let src = self.operand_reg(value, 0);
                self.code.push(Op::SlicePush { obj: reg(*local), src });
                return;
            }
        };

        match value {
            // Both are replaced by the state-machine transform before any
            // backend sees them, and the driver reports one that was not
            // (`kite_mir::internal_errors`) before asking for bytecode. An
            // embedder that skipped the question gets a trap here, not a
            // compiler that panics.
            mir::Rvalue::Await { .. } | mir::Rvalue::Yield => self.code.push(Op::Unreachable),
            // The VM's sharing is `Rc`'s count, so it keeps no flag: the answer
            // is never read here except by `Unspill`, which ignores it.
            mir::Rvalue::Owned { .. } => self.code.push(Op::LoadBool { dst, value: false }),
            // Read the field and empty it, so the local is the one count left
            // and its next `push` goes straight in. Nothing reads the field
            // again before the next spill writes it.
            // Taken the same way, for the write through a field that
            // follows: the write-back puts it back.
            mir::Rvalue::Unspill { base, index, .. } | mir::Rvalue::FieldTake { base, index } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::GetField { dst, obj, index: *index as u16 });
                let empty = self.operand_reg(&mir::Operand::Unit, 1);
                self.code.push(Op::SetField { obj, index: *index as u16, src: empty });
            }
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
                    argc: width(args.len()),
                });
            }

            mir::Rvalue::ClosureNew { func, captures } => {
                self.stage_args(captures);
                self.code.push(Op::Closure {
                    dst,
                    func: func.0,
                    base: self.arg_base,
                    count: width(captures.len()),
                });
            }

            mir::Rvalue::CallClosure { callee, args } => {
                let c = self.operand_reg(callee, 0);
                self.stage_args(args);
                self.code.push(Op::CallClosure {
                    dst,
                    callee: c,
                    base: self.arg_base,
                    argc: width(args.len()),
                });
            }

            mir::Rvalue::Cast { operand, to, .. } => {
                let a = self.operand_reg(operand, 0);
                // A cast to the type a value already has is a no-op, which the
                // checker allows so that generic code can write one anyway.
                self.code.push(if *to == kite_hir::TyId::FLOAT {
                    Op::IntToFloat { dst, src: a }
                } else {
                    Op::FloatToInt { dst, src: a }
                });
            }

            mir::Rvalue::StrOp { op, args } => {
                self.stage_args(args);
                self.code.push(Op::StrOp {
                    dst,
                    op: *op,
                    base: self.arg_base,
                    argc: width(args.len()),
                });
            }

            mir::Rvalue::ToStr { operand, .. } => {
                let a = self.operand_reg(operand, 0);
                self.code.push(Op::ToStr { dst, src: a });
            }

            mir::Rvalue::CallVirtual { trait_id, method, args } => {
                self.stage_args(args);
                self.code.push(Op::CallVirtual {
                    dst,
                    table: self.vtable_index(*trait_id),
                    method: *method,
                    base: self.arg_base,
                    argc: width(args.len()),
                });
            }

            mir::Rvalue::CallBuiltin { builtin, args } => {
                self.stage_args(args);
                self.code.push(Op::CallNative {
                    dst,
                    native: Native::from_builtin(*builtin),
                    base: self.arg_base,
                    argc: width(args.len()),
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
                    count: width(fields.len()),
                });
            }

            // The VM stores an enum's payload in the same slot layout as a
            // struct's fields, so both read the same way. Only the Wasm
            // backend, which subtypes its variants, needs the distinction.
            mir::Rvalue::FieldGet { base, index }
            | mir::Rvalue::VariantGet { base, index, .. } => {
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
                    count: width(fields.len()),
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
            mir::Rvalue::ErrorNew { message, value, tag, cause } => {
                let m = self.operand_reg(message, 0);
                let v = self.operand_reg(value, 1);
                let t = self.operand_reg(tag, 2);
                let c = self.operand_reg(cause, 3);
                self.code.push(Op::NewError { dst, message: m, value: v, tag: t, cause: c });
            }
            mir::Rvalue::ErrorMessage { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::ErrorMessage { dst, obj });
            }
            mir::Rvalue::ErrorCause { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::ErrorCause { dst, obj });
            }
            mir::Rvalue::ErrorTag { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::ErrorTag { dst, obj });
            }
            mir::Rvalue::ErrorAs { base, tag } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::ErrorAs { dst, obj, tag: *tag });
            }
            // The VM's values carry their own type, so an optional needs no
            // box: wrapping and unwrapping are moves — except of an `error`,
            // whose nil would otherwise read as an absent optional. A task
            // that finished in time with a nil error is still a result
            // `task.timeout` has, and the other two backends box it.
            mir::Rvalue::Wrap { value } => match value {
                mir::Operand::Local(l) if self.is_err(self.func.locals[l.index()].ty) => {
                    self.code.push(Op::WrapErr { dst, src: reg(*l) });
                }
                _ => self.load_into(dst, value),
            },
            mir::Rvalue::Unwrap { value } => match value {
                mir::Operand::Local(l) if self.is_err(self.func.locals[dst as usize].ty) => {
                    self.code.push(Op::UnwrapErr { dst, src: reg(*l) });
                }
                _ => self.load_into(dst, value),
            },

            mir::Rvalue::IsNil { value } => {
                let obj = self.operand_reg(value, 0);
                self.code.push(Op::IsNil { dst, obj });
            }

            mir::Rvalue::MapNew { entries } => {
                let first = &entries[..entries.len().min(mir::LITERAL_WINDOW)];
                self.stage_args(first);
                self.code.push(Op::NewMap {
                    dst,
                    base: self.arg_base,
                    count: width(first.len()),
                });
                for entry in entries[first.len()..].chunks(2) {
                    let key = self.operand_reg(&entry[0], 0);
                    let src = self.operand_reg(&entry[1], 1);
                    self.code.push(Op::MapSet { obj: dst, key, src });
                }
            }
            mir::Rvalue::MapGet { base, key } => {
                let obj = self.operand_reg(base, 0);
                let k = self.operand_reg(key, 1);
                let err = self.payload_is_err(base);
                self.code.push(Op::MapGet { dst, obj, key: k, err });
            }
            mir::Rvalue::MapLen { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::MapLen { dst, obj });
            }
            mir::Rvalue::CallExtern { index, args } => {
                self.stage_args(args);
                self.code.push(Op::CallExtern {
                    dst,
                    index: *index,
                    base: self.arg_base,
                    argc: width(args.len()),
                });
            }
            mir::Rvalue::MapKeys { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::MapKeys { dst, obj });
            }
            mir::Rvalue::MapValues { base } => {
                let obj = self.operand_reg(base, 0);
                self.code.push(Op::MapValues { dst, obj });
            }
            mir::Rvalue::TupleNew { elems } => {
                self.stage_args(elems);
                self.code.push(Op::NewTuple {
                    dst,
                    base: self.arg_base,
                    count: width(elems.len()),
                });
            }

            // A literal past the window is built a window at a time, as the
            // other backends build it: the first window staged, and each item
            // after it added to what has been built. Staged whole, a literal
            // of constants took a register per element, and seventy thousand
            // of them was a frame the bytecode cannot address. Writing into
            // the literal before its last items are read is safe because none
            // of them can be it: no slice or map holds its own type.
            mir::Rvalue::SliceNew { elems } => {
                let first = &elems[..elems.len().min(mir::LITERAL_WINDOW)];
                self.stage_args(first);
                self.code.push(Op::NewSlice {
                    dst,
                    base: self.arg_base,
                    count: width(first.len()),
                });
                for elem in &elems[first.len()..] {
                    let src = self.operand_reg(elem, 0);
                    self.code.push(Op::SlicePush { obj: dst, src });
                }
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
                let err = self.payload_is_err(base);
                self.code.push(Op::SliceGet { dst, obj, index: idx, err });
            }
            mir::Rvalue::SliceRange { base, start, end } => {
                let obj = self.operand_reg(base, 0);
                let s = self.operand_reg(start, 1);
                let e = self.operand_reg(end, 2);
                self.code.push(Op::SliceRange { dst, obj, start: s, end: e });
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

    fn is_err(&self, ty: kite_hir::TyId) -> bool {
        matches!(self.types.kind(ty), kite_hir::TyKind::Err)
    }

    /// Whether the slice or map in `base` holds `error`s.
    fn payload_is_err(&self, base: &mir::Operand) -> bool {
        let mir::Operand::Local(l) = base else {
            return false;
        };
        match self.types.kind(self.func.locals[l.index()].ty) {
            kite_hir::TyKind::Slice(t) | kite_hir::TyKind::Map(_, t) => self.is_err(*t),
            _ => false,
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
            // The VM's registers carry their own tags, so one nil serves for
            // every unreadable slot. Wasm, whose slots are typed, needs the
            // type this operand carries.
            mir::Operand::Nil | mir::Operand::Default(_) => Op::LoadNil { dst },
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
    // `limits` has bounded the frame, so this cannot fail on a program the
    // driver let through.
    Reg::try_from(l.0).expect("register index overflows u16")
}

/// An element or argument count, which is as wide as a register index.
fn width(n: usize) -> u16 {
    u16::try_from(n).expect("a count wider than a frame; `limits` reports this")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kite_hir::TyId;

    fn function(locals: usize) -> mir::Function {
        let span = kite_span::Span::new(kite_span::FileId(0), 0, 0);
        mir::Function {
            name: "wide".into(),
            is_async: false,
            exportable: false,
            param_count: 0,
            locals: (0..locals)
                .map(|_| mir::LocalDecl { ty: TyId::INT, name: None })
                .collect(),
            ret: TyId::UNIT,
            blocks: vec![mir::BasicBlock {
                stmts: vec![mir::Inst::Assign {
                    dst: mir::Local(locals as u32 - 1),
                    value: mir::Rvalue::Use(mir::Operand::Int(1)),
                }],
                term: mir::Terminator::Return(None),
            }],
            span,
        }
    }

    /// A register index is sixteen bits. A frame wider than that was emitted
    /// with its indices cut short, so the last local here landed on top of an
    /// early one; now it is reported before anything is emitted.
    #[test]
    fn a_frame_too_wide_to_address_is_reported() {
        let program = mir::Program { fns: vec![function(70_000)], ..Default::default() };
        let found = limits(&program);
        assert_eq!(found.len(), 1, "{:?}", found);
        assert_eq!(found[0].function, "wide");

        let fits = mir::Program { fns: vec![function(1_000)], ..Default::default() };
        assert!(limits(&fits).is_empty());
        let chunk = compile(&fits, &kite_hir::Types::new());
        assert!(chunk.functions[0].frame_size <= MAX_REGISTERS);
    }
}
