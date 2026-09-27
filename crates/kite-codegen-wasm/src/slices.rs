//! Slices: the header a slice value is, and the helpers that write into one.
//!
//! A Kite slice is a *value* — `let a = xs; xs.push(1)` leaves `a` as it was —
//! and the obvious way to get that on WasmGC is the one this backend started
//! with: a bare array, copied whole on every `push` and every `xs[i] = v`.
//! Correct, and quadratic: a hundred thousand pushes took twenty seconds where
//! the bytecode VM took twenty milliseconds.
//!
//! So a slice is a header, `{buf, len}`, over a buffer that may be longer than
//! the slice, and a write goes straight into the buffer when nothing else can
//! see it. What decides "nothing else can see it" is a flag per mutated local
//! (see `Emitter::release`): set when the function made the header and buffer
//! itself, cleared the moment the reference is copied anywhere. The VM decides
//! the same thing with `Rc::make_mut`'s reference count; a GC target has no
//! count to ask, but it has the compiler, which can see every place a local is
//! read.
//!
//! The writes themselves are small functions, one per element type and
//! operation a program uses, rather than inline code: a push is fifty bytes of
//! logic, and an island pushes in dozens of places.

use crate::*;

/// A slice operation with a helper function of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SliceOp {
    /// `xs[i]`: the element, trapping unless `0 <= i < len`.
    At,
    /// `xs.push(v)`, answering the header to keep.
    Push,
    /// `xs[i] = v`, answering the header to keep.
    Set,
}

/// The helpers a program calls, and where they start in the function index
/// space.
pub struct SliceHelpers {
    list: Vec<(TyId, SliceOp)>,
    pub base: u32,
}

impl SliceHelpers {
    /// Every (element type, operation) a program uses, in a stable order.
    pub fn collect(program: &mir::Program, types: &Types, base: u32) -> SliceHelpers {
        let mut list: Vec<(TyId, SliceOp)> = Vec::new();
        for f in &program.fns {
            let elem_of = |l: &mir::Local| match types.kind(f.locals[l.index()].ty) {
                TyKind::Slice(e) => Some(*e),
                _ => None,
            };
            for b in &f.blocks {
                for s in &b.stmts {
                    let found = match s {
                        mir::Inst::SlicePush { local, .. } => {
                            elem_of(local).map(|e| (e, SliceOp::Push))
                        }
                        mir::Inst::SetIndex {
                            base: mir::Operand::Local(l),
                            ..
                        } => elem_of(l).map(|e| (e, SliceOp::Set)),
                        mir::Inst::Assign {
                            value:
                                mir::Rvalue::IndexGet {
                                    base: mir::Operand::Local(l),
                                    ..
                                },
                            ..
                        } => elem_of(l).map(|e| (e, SliceOp::At)),
                        _ => None,
                    };
                    if let Some(k) = found {
                        if !list.contains(&k) {
                            list.push(k);
                        }
                    }
                }
            }
        }
        list.sort_by_key(|(e, op)| (e.0, *op));
        SliceHelpers { list, base }
    }

    /// The function index of a helper, when the program uses it.
    pub fn index(&self, elem: TyId, op: SliceOp) -> Option<u32> {
        self.list
            .iter()
            .position(|k| *k == (elem, op))
            .map(|i| self.base + i as u32)
    }

    /// Declare each helper's signature, returning their type indices.
    pub fn add_types(
        &self,
        section: &mut TypeSection,
        mut next: u32,
        types: &Types,
        layout: &TypeLayout,
    ) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.list.len());
        for (elem, op) in &self.list {
            let (Some(header), elem_ty) =
                (layout.slice_header(*elem), val_type_with(*elem, types, layout))
            else {
                section.ty().function(vec![], vec![]);
                out.push(next);
                next += 1;
                continue;
            };
            let h = ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(header),
            });
            let (params, results) = match op {
                SliceOp::At => (vec![h, ValType::I64], vec![elem_ty]),
                SliceOp::Push => (vec![h, ValType::I32, elem_ty], vec![h]),
                SliceOp::Set => (vec![h, ValType::I32, ValType::I64, elem_ty], vec![h]),
            };
            section.ty().function(params, results);
            out.push(next);
            next += 1;
        }
        out
    }

    /// Emit each helper's body, in the order [`Self::add_types`] declared them.
    pub fn emit(&self, code: &mut CodeSection, layout: &TypeLayout) {
        for (elem, op) in &self.list {
            let (Some(array), Some(header)) = (layout.slice_type(*elem), layout.slice_header(*elem))
            else {
                let mut f = Function::new(Vec::new());
                f.instruction(&Instruction::Unreachable);
                f.instruction(&Instruction::End);
                code.function(&f);
                continue;
            };
            let parts = Parts { array, header };
            code.function(&match op {
                SliceOp::At => parts.at(),
                SliceOp::Push => parts.push(),
                SliceOp::Set => parts.set(),
            });
        }
    }
}

#[derive(Clone, Copy)]
struct Parts {
    array: u32,
    header: u32,
}

impl Parts {
    fn len(self, f: &mut Function, local: u32) {
        f.instruction(&Instruction::LocalGet(local));
        f.instruction(&Instruction::StructGet {
            struct_type_index: self.header,
            field_index: 1,
        });
    }

    fn buf(self, f: &mut Function, local: u32) {
        f.instruction(&Instruction::LocalGet(local));
        f.instruction(&Instruction::StructGet {
            struct_type_index: self.header,
            field_index: 0,
        });
    }

    /// Trap unless the i64 in `index` is below the header's length.
    ///
    /// Compared as 64-bit and unsigned, against the slice's length rather than
    /// the buffer's: a negative index is huge unsigned, and an index past
    /// `i32::MAX` is out of range rather than wrapped round to a small one —
    /// `xs[4294967296]` used to read `xs[0]`.
    fn check(self, f: &mut Function, index: u32) {
        f.instruction(&Instruction::LocalGet(index));
        self.len(f, 0);
        f.instruction(&Instruction::I64ExtendI32U);
        f.instruction(&Instruction::I64GeU);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::Unreachable);
        f.instruction(&Instruction::End);
    }

    /// Replace the header in local 0 with a new one of length `n` over a
    /// buffer holding the current contents, as long as the i32 on the stack.
    fn copy_into_new(self, f: &mut Function, n: u32, buf: u32) {
        f.instruction(&Instruction::ArrayNewDefault(self.array));
        f.instruction(&Instruction::LocalTee(buf));
        // array.copy takes dest, dest_offset, src, src_offset, len.
        f.instruction(&Instruction::I32Const(0));
        self.buf(f, 0);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: self.array,
            array_type_index_src: self.array,
        });
        f.instruction(&Instruction::LocalGet(buf));
        f.instruction(&Instruction::RefAsNonNull);
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::StructNew(self.header));
        f.instruction(&Instruction::LocalSet(0));
    }

    fn buf_local(self) -> (u32, ValType) {
        (
            1,
            ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(self.array),
            }),
        )
    }

    /// `at(header, index) -> element`.
    fn at(self) -> Function {
        let mut f = Function::new(Vec::new());
        self.check(&mut f, 1);
        self.buf(&mut f, 0);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::I32WrapI64);
        f.instruction(&Instruction::ArrayGet(self.array));
        f.instruction(&Instruction::End);
        f
    }

    /// `push(header, owned, value) -> header`.
    ///
    /// In place when the caller owns the header and the buffer has room: the
    /// value goes into the next slot and the length moves, with nothing
    /// allocated. Otherwise the contents move to a buffer twice as long, which
    /// is what makes a loop of pushes linear.
    /// The caller keeps whichever header comes back, and owns it.
    fn push(self) -> Function {
        // Parameters: 0 header, 1 owned, 2 value. Locals: 3 n, 4 buf.
        let (n, buf) = (3, 4);
        let mut f = Function::new(vec![(1, ValType::I32), self.buf_local()]);
        // Copy when the buffer is full or not the caller's alone.
        self.len(&mut f, 0);
        f.instruction(&Instruction::LocalTee(n));
        self.buf(&mut f, 0);
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::I32Or);
        f.instruction(&Instruction::If(BlockType::Empty));
        // Twice the length and four more: 4, 12, 28, … so a slice built up
        // from empty reallocates a logarithmic number of times.
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::I32Const(4));
        f.instruction(&Instruction::I32Add);
        self.copy_into_new(&mut f, n, buf);
        f.instruction(&Instruction::End);
        // Room now, and ours: write, then lengthen.
        self.buf(&mut f, 0);
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::LocalGet(2));
        f.instruction(&Instruction::ArraySet(self.array));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::StructSet {
            struct_type_index: self.header,
            field_index: 1,
        });
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::End);
        f
    }

    /// `set(header, owned, index, value) -> header`.
    ///
    /// The index is checked first, as the VM checks it before its
    /// `make_mut`. A header the caller does not own is copied — exactly as
    /// long as the slice, since a write does not suggest a push to follow.
    fn set(self) -> Function {
        // Parameters: 0 header, 1 owned, 2 index, 3 value. Locals: 4 n, 5 buf.
        let (n, buf) = (4, 5);
        let mut f = Function::new(vec![(1, ValType::I32), self.buf_local()]);
        self.check(&mut f, 2);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Empty));
        self.len(&mut f, 0);
        f.instruction(&Instruction::LocalTee(n));
        self.copy_into_new(&mut f, n, buf);
        f.instruction(&Instruction::End);
        self.buf(&mut f, 0);
        f.instruction(&Instruction::LocalGet(2));
        f.instruction(&Instruction::I32WrapI64);
        f.instruction(&Instruction::LocalGet(3));
        f.instruction(&Instruction::ArraySet(self.array));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::End);
        f
    }
}

/// The operands of an instruction through which a slice's reference may be
/// kept: stored, passed, captured, boxed or returned. See
/// `Emitter::release`.
///
/// Deliberately exhaustive, with no catch-all: an operand position this does
/// not know about would be a way for a shared buffer to be written in place,
/// so a new MIR construct fails to compile here until someone decides.
pub fn escaping_operands(stmt: &mir::Inst) -> Vec<&mir::Operand> {
    use mir::Rvalue as R;
    let value = match stmt {
        mir::Inst::Assign { value, .. } => value,
        mir::Inst::SetField { value, .. } => return vec![value],
        // The base is the slice being written, which is not an escape.
        mir::Inst::SetIndex { value, .. } => return vec![value],
        mir::Inst::SlicePush { value, .. } => return vec![value],
        mir::Inst::MapSet { key, value, .. } => return vec![key, value],
        // A removal compares its key and keeps nothing.
        mir::Inst::MapRemove { .. } => return Vec::new(),
    };
    match value {
        R::Use(o) | R::Wrap { value: o } => vec![o],
        R::Call { args, .. }
        | R::CallVirtual { args, .. }
        | R::CallBuiltin { args, .. }
        | R::CallExtern { args, .. }
        | R::StrOp { args, .. } => args.iter().collect(),
        R::ClosureNew { captures, .. } => captures.iter().collect(),
        R::CallClosure { callee, args } => std::iter::once(callee).chain(args).collect(),
        R::StructNew { fields, .. } | R::EnumNew { fields, .. } => fields.iter().collect(),
        R::TupleNew { elems } | R::SliceNew { elems } => elems.iter().collect(),
        R::MapNew { entries } => entries.iter().collect(),
        R::PairNew { value, error } => vec![value, error],
        R::ErrorNew {
            message,
            value,
            tag,
            cause,
        } => vec![message, value, tag, cause],
        // Reads that keep nothing: a comparison, a length, an element, a
        // window (which copies), a test, a lookup by key.
        R::Binary { .. }
        | R::Unary { .. }
        | R::ToStr { .. }
        | R::Cast { .. }
        | R::FieldGet { .. }
        | R::TagOf { .. }
        | R::VariantGet { .. }
        | R::MapGet { .. }
        | R::MapLen { .. }
        | R::MapKeys { .. }
        | R::MapValues { .. }
        | R::IsNil { .. }
        | R::Unwrap { .. }
        | R::PairValue { .. }
        | R::PairError { .. }
        | R::ErrorMessage { .. }
        | R::ErrorCause { .. }
        | R::ErrorTag { .. }
        | R::ErrorAs { .. }
        | R::IndexGet { .. }
        | R::SliceLen { .. }
        | R::SliceGet { .. }
        | R::SliceRange { .. }
        | R::Await { .. }
        | R::Yield => Vec::new(),
    }
}
