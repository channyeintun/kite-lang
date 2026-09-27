//! Structural equality on aggregates.
//!
//! The specification says two structs are equal when their fields are, and the
//! same for tuples, slices, maps, enums and optionals. Reference identity is a
//! separate operation, spelled `ptr.same`.
//!
//! Wasm has no deep-equality instruction, so the module carries one generated
//! function that compares any aggregate the program compares. It takes the two
//! values as `anyref` and a *kind* — which of those types they are — and works
//! through a list of pairs still to compare rather than calling itself.
//!
//! That list is the point. The first version generated a function per type
//! that called the function for each component, so comparing two lists of a
//! million cells nested a million Wasm frames and ended in the engine's
//! `RangeError`, where the VM and the native runtime — which both walk a
//! worklist — answered `true`. Here a component that needs a comparison of its
//! own is pushed as a cell `{a, b, kind, next}` and taken off again by the same
//! loop, so the depth a comparison can reach is bounded by memory rather than
//! by the stack. Numbers, booleans and strings are compared where they stand
//! and never pushed; a struct of scalars allocates nothing.
//!
//! The function is emitted only when a program compares an aggregate, or keys
//! a map by one. A program that does neither gets none of it.

use crate::*;
use std::collections::HashSet;

/// A compared type, in kind order: its kind is its position.
pub struct EqFn {
    pub ty: TyId,
}

/// Every aggregate type a program compares, closed over the components those
/// comparisons will recurse into.
///
/// Order is deterministic — a `TyId` is an arena index, so sorting by it gives
/// the same module for the same program every time.
pub fn collect(program: &mir::Program, types: &Types) -> Vec<EqFn> {
    let mut wanted: HashSet<TyId> = HashSet::new();

    for f in &program.fns {
        // A map compares its keys with `==` on every read, write and removal,
        // so a key type that needs a generated comparison needs it whether or
        // not the program ever writes `==` itself.
        for l in &f.locals {
            if let TyKind::Map(k, _) = types.kind(l.ty) {
                close_over(*k, types, &mut wanted);
            }
        }
        for block in &f.blocks {
            for stmt in &block.stmts {
                let mir::Inst::Assign {
                    value: mir::Rvalue::Binary { op, lhs, .. },
                    ..
                } = stmt
                else {
                    continue;
                };
                if !matches!(op, kite_hir::BinOp::EqValue | kite_hir::BinOp::NeValue) {
                    continue;
                }
                if let mir::Operand::Local(l) = lhs {
                    close_over(f.locals[l.index()].ty, types, &mut wanted);
                }
            }
        }
    }

    let mut list: Vec<TyId> = wanted.into_iter().collect();
    list.sort_by_key(|t| t.0);
    list.into_iter().map(|ty| EqFn { ty }).collect()
}

/// Add a type and everything comparing it will recurse into.
fn close_over(ty: TyId, types: &Types, out: &mut HashSet<TyId>) {
    if !needs_function(ty, types) || !out.insert(ty) {
        return;
    }
    match types.kind(ty) {
        TyKind::Struct(s) => {
            for f in &types.struct_def(*s).fields {
                close_over(f.ty, types, out);
            }
        }
        TyKind::Enum(e) => {
            for v in &types.enum_def(*e).variants {
                for f in &v.fields {
                    close_over(f.ty, types, out);
                }
            }
        }
        TyKind::Tuple(elems) => {
            for e in elems {
                close_over(*e, types, out);
            }
        }
        TyKind::Slice(elem) => close_over(*elem, types, out),
        TyKind::Optional(inner) => close_over(*inner, types, out),
        TyKind::Map(k, v) => {
            close_over(*k, types, out);
            close_over(*v, types, out);
        }
        _ => {}
    }
}

/// Whether comparing this type needs a generated function, as opposed to a
/// single instruction or a host call.
pub fn needs_function(ty: TyId, types: &Types) -> bool {
    matches!(
        types.kind(ty),
        TyKind::Struct(_)
            | TyKind::Enum(_)
            | TyKind::Tuple(_)
            | TyKind::Slice(_)
            | TyKind::Map(..)
            | TyKind::Optional(_)
            | TyKind::Err
    )
}

/// The comparison's signature: the two values as `anyref`, their kind, and an
/// `i32` verdict. Every aggregate is a GC reference and so an `anyref`; the
/// kind is what says which record to cast them back to.
pub fn signature() -> (Vec<ValType>, Vec<ValType>) {
    (vec![ANY_REF, ANY_REF, ValType::I32], vec![ValType::I32])
}

/// The record one pending comparison waits in: two values, their kind, and
/// the comparison pending before it. Immutable, like everything a cons list is
/// made of: a push allocates one, a pop reads one and lets it go.
pub fn cell_subtype(cell: u32) -> SubType {
    let field = |t: ValType| FieldType {
        element_type: StorageType::Val(t),
        mutable: false,
    };
    struct_subtype(
        vec![
            field(ANY_REF),
            field(ANY_REF),
            field(ValType::I32),
            field(ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(cell),
            })),
        ],
        None,
        true,
    )
}

/// Emit the comparison, and calls to it.
pub struct EqBuilder<'a> {
    pub types: &'a Types,
    pub layout: &'a TypeLayout,
    /// String equality is part of the language runtime, not a host call.
    pub strings: strings::StringRuntime,
    /// The comparison's function index.
    pub base: u32,
    pub fns: &'a [EqFn],
}

// Parameters and fixed locals of the comparison.
const A: u32 = 0;
const B: u32 = 1;
const KIND: u32 = 2;
/// The pending list: a `(ref null $cell)`, null when nothing is pending.
const TOP: u32 = 3;
/// A cursor and a length, for slices and maps.
const I: u32 = 4;
const N: u32 = 5;

impl EqBuilder<'_> {
    /// The kind `ty` is compared as.
    fn kind_of(&self, ty: TyId) -> Option<u32> {
        self.fns.iter().position(|e| e.ty == ty).map(|i| i as u32)
    }

    /// Compare two values of `ty` already on the stack, leaving an `i32`.
    pub fn call(&self, f: &mut Function, ty: TyId) {
        match self.kind_of(ty) {
            Some(kind) => {
                f.instruction(&Instruction::I32Const(kind as i32));
                f.instruction(&Instruction::Call(self.base));
            }
            // `collect` closed over every type that reaches here, so a miss
            // is a compiler bug rather than a program's.
            None => {
                f.instruction(&Instruction::Unreachable);
            }
        }
    }

    /// The comparison: a loop over pending pairs, one case per kind.
    ///
    /// ```text
    /// loop $next
    ///   block $done
    ///     block $kind_n … block $kind_0
    ///       br_table on $kind          ;; default: unreachable
    ///     end  <compare kind 0; push components; br $done>
    ///     …
    ///   end
    ///   nothing pending -> return 1
    ///   pop into $a, $b, $kind ; br $next
    /// end
    /// ```
    ///
    /// A mismatch anywhere returns 0 at once, dropping whatever is pending.
    pub fn build(&self) -> Function {
        let cell = self.layout.eq_cell;
        let n = self.fns.len() as u32;

        // Two typed registers per slice kind and four per map kind, so a loop
        // over elements casts its operands once rather than per element.
        let mut locals: Vec<(u32, ValType)> = vec![
            (
                1,
                ValType::Ref(RefType {
                    nullable: true,
                    heap_type: HeapType::Concrete(cell),
                }),
            ),
            (2, ValType::I32),
        ];
        let mut arrays: Vec<u32> = Vec::with_capacity(self.fns.len());
        let mut next = N + 1;
        for e in self.fns {
            arrays.push(next);
            let want: Vec<u32> = match self.types.kind(e.ty) {
                TyKind::Slice(elem) => self.layout.slice_type(*elem).into_iter().collect(),
                TyKind::Map(..) => self
                    .layout
                    .map_layout(e.ty)
                    .map(|ml| vec![ml.keys, ml.values])
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            for array in want {
                locals.push((
                    2,
                    ValType::Ref(RefType {
                        nullable: true,
                        heap_type: HeapType::Concrete(array),
                    }),
                ));
                next += 2;
            }
        }
        let mut f = Function::new(locals);

        f.instruction(&Instruction::Loop(BlockType::Empty)); // $next
        f.instruction(&Instruction::Block(BlockType::Empty)); // $done
        for _ in 0..n {
            f.instruction(&Instruction::Block(BlockType::Empty));
        }
        f.instruction(&Instruction::Block(BlockType::Empty)); // bad kind
        f.instruction(&Instruction::LocalGet(KIND));
        let targets: Vec<u32> = (1..=n).collect();
        f.instruction(&Instruction::BrTable(targets.into(), 0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::Unreachable);

        for (j, e) in self.fns.iter().enumerate() {
            f.instruction(&Instruction::End);
            // From here the blocks still open are the later kinds and $done.
            let done = n - 1 - j as u32;
            let case = Case {
                builder: self,
                done,
                regs: arrays[j],
            };
            case.body(&mut f, e.ty);
            f.instruction(&Instruction::Br(done));
        }
        f.instruction(&Instruction::End); // $done

        // Take the next pending pair, or answer: nothing left means nothing
        // differed.
        f.instruction(&Instruction::LocalGet(TOP));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        for (field, local) in [(0, A), (1, B), (2, KIND), (3, TOP)] {
            f.instruction(&Instruction::LocalGet(TOP));
            f.instruction(&Instruction::StructGet {
                struct_type_index: cell,
                field_index: field,
            });
            f.instruction(&Instruction::LocalSet(local));
        }
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End); // $next
        f.instruction(&Instruction::Unreachable);
        f.instruction(&Instruction::End);
        f
    }
}

/// One kind's case in the comparison.
struct Case<'a, 'b> {
    builder: &'a EqBuilder<'b>,
    /// Branch depth from the case body to `$done`.
    done: u32,
    /// The first of this kind's typed array registers.
    regs: u32,
}

impl Case<'_, '_> {
    fn types(&self) -> &Types {
        self.builder.types
    }

    fn layout(&self) -> &TypeLayout {
        self.builder.layout
    }

    fn body(&self, f: &mut Function, ty: TyId) {
        match self.types().kind(ty) {
            TyKind::Struct(s) => {
                let record = self.layout().struct_type(*s);
                let shift = self.layout().struct_shift(*s);
                for (i, fd) in self.types().struct_def(*s).fields.iter().enumerate() {
                    self.component(f, fd.ty, record, i as u32 + shift);
                }
            }
            TyKind::Tuple(elems) => {
                let Some(record) = self.layout().tuple_type(ty) else {
                    f.instruction(&Instruction::Unreachable);
                    return;
                };
                for (i, e) in elems.iter().enumerate() {
                    self.component(f, *e, record, i as u32);
                }
            }
            TyKind::Enum(e) => self.enum_body(f, *e),
            TyKind::Optional(inner) => {
                let Some(boxed) = self.layout().option_type(*inner) else {
                    f.instruction(&Instruction::Unreachable);
                    return;
                };
                self.presence(f);
                self.component(f, *inner, boxed, 0);
            }
            // Two errors are equal when their messages are.
            TyKind::Err => {
                self.presence(f);
                self.component(f, TyId::STR, self.layout().error_record, 0);
            }
            TyKind::Slice(elem) => self.slice_body(f, *elem),
            TyKind::Map(..) => self.map_body(f, ty),
            // `collect` only ever asks for the kinds above.
            _ => {
                f.instruction(&Instruction::Unreachable);
            }
        }
    }

    /// `nil == nil` is equal and `nil == x` is not; two present values fall
    /// through to have their contents compared.
    fn presence(&self, f: &mut Function) {
        f.instruction(&Instruction::LocalGet(A));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::LocalGet(B));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::I32Ne);
        differ(f);
        f.instruction(&Instruction::LocalGet(A));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::BrIf(self.done));
    }

    /// Compare field `field` of `record` on both sides.
    fn component(&self, f: &mut Function, ty: TyId, record: u32, field: u32) {
        self.compare(f, ty, |f, side| {
            f.instruction(&Instruction::LocalGet(side));
            f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(record)));
            f.instruction(&Instruction::StructGet {
                struct_type_index: record,
                field_index: field,
            });
        });
    }

    /// Compare two values of `ty`, each put on the stack by `load` given the
    /// side it is from: in place for a scalar or a string, returning 0 when
    /// they differ; pushed for later when they are an aggregate.
    fn compare(&self, f: &mut Function, ty: TyId, load: impl Fn(&mut Function, u32)) {
        load(f, A);
        load(f, B);
        if let Some(kind) = self.builder.kind_of(ty) {
            f.instruction(&Instruction::I32Const(kind as i32));
            f.instruction(&Instruction::LocalGet(TOP));
            f.instruction(&Instruction::StructNew(self.layout().eq_cell));
            f.instruction(&Instruction::LocalSet(TOP));
            return;
        }
        match self.types().kind(ty) {
            TyKind::Str => {
                f.instruction(&Instruction::Call(self.builder.strings.eq()));
                f.instruction(&Instruction::I32Eqz);
            }
            _ => {
                f.instruction(&match val_type_with(ty, self.types(), self.layout()) {
                    ValType::I64 => Instruction::I64Ne,
                    // `NaN` differs from itself here as it does under `==`.
                    ValType::F64 => Instruction::F64Ne,
                    ValType::I32 => Instruction::I32Ne,
                    // A host value, a function or a trait object has no `==`,
                    // and the checker refuses a type containing one.
                    _ => Instruction::Unreachable,
                });
            }
        }
        differ(f);
    }

    /// Different variants are never equal; the same variant compares payloads.
    fn enum_body(&self, f: &mut Function, e: kite_hir::EnumId) {
        let base = self.layout().enum_base_type(e);
        let shift = self.layout().enum_shift(e);
        let tag = |f: &mut Function, side: u32| {
            f.instruction(&Instruction::LocalGet(side));
            f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(base)));
            f.instruction(&Instruction::StructGet {
                struct_type_index: base,
                field_index: shift,
            });
        };
        tag(f, A);
        tag(f, B);
        f.instruction(&Instruction::I32Ne);
        differ(f);

        // The tags agree, so each arm may cast both sides to its variant.
        for (v, variant) in self.types().enum_def(e).variants.iter().enumerate() {
            if variant.fields.is_empty() {
                continue;
            }
            let record = self.layout().variant_type(e, v as u32);
            tag(f, A);
            f.instruction(&Instruction::I32Const(v as i32));
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::If(BlockType::Empty));
            for (i, fd) in variant.fields.iter().enumerate() {
                self.component(f, fd.ty, record, i as u32 + 1 + shift);
            }
            f.instruction(&Instruction::End);
        }
    }

    /// Equal lengths, then equal elements. Only the slices' own lengths are
    /// read — the storage behind them may be longer.
    fn slice_body(&self, f: &mut Function, elem: TyId) {
        let (Some(array), Some(header)) = (
            self.layout().slice_type(elem),
            self.layout().slice_header(elem),
        ) else {
            f.instruction(&Instruction::Unreachable);
            return;
        };
        let (xa, xb) = (self.regs, self.regs + 1);
        for (side, reg) in [(A, xa), (B, xb)] {
            f.instruction(&Instruction::LocalGet(side));
            f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(header)));
            f.instruction(&Instruction::StructGet {
                struct_type_index: header,
                field_index: 0,
            });
            f.instruction(&Instruction::LocalSet(reg));
        }
        let len = |f: &mut Function, side: u32| {
            f.instruction(&Instruction::LocalGet(side));
            f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(header)));
            f.instruction(&Instruction::StructGet {
                struct_type_index: header,
                field_index: 1,
            });
        };
        len(f, A);
        f.instruction(&Instruction::LocalTee(N));
        len(f, B);
        f.instruction(&Instruction::I32Ne);
        differ(f);
        self.each(f, |case, f| {
            case.compare(f, elem, |f, side| {
                f.instruction(&Instruction::LocalGet(if side == A { xa } else { xb }));
                f.instruction(&Instruction::LocalGet(I));
                f.instruction(&Instruction::ArrayGet(array));
            });
        });
    }

    /// Equal lengths, then equal entries position by position: the same keys
    /// with equal values, inserted in the same order. Insertion order is part
    /// of what a Kite map is — iteration, `keys()` and a derived `hash()` all
    /// observe it — so two maps that differ in it are different values. The
    /// other two backends compare their entry vectors the same way.
    fn map_body(&self, f: &mut Function, ty: TyId) {
        let Some(ml) = self.layout().map_layout(ty) else {
            f.instruction(&Instruction::Unreachable);
            return;
        };
        // Keys in the first two registers, values in the next two.
        for (field, first) in [(0, self.regs), (1, self.regs + 2)] {
            for (side, reg) in [(A, first), (B, first + 1)] {
                f.instruction(&Instruction::LocalGet(side));
                f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(ml.record)));
                f.instruction(&Instruction::StructGet {
                    struct_type_index: ml.record,
                    field_index: field,
                });
                f.instruction(&Instruction::LocalSet(reg));
            }
        }
        f.instruction(&Instruction::LocalGet(self.regs));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::LocalTee(N));
        f.instruction(&Instruction::LocalGet(self.regs + 1));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32Ne);
        differ(f);
        self.each(f, |case, f| {
            for (first, array, elem) in [
                (case.regs, ml.keys, ml.key_ty),
                (case.regs + 2, ml.values, ml.value_ty),
            ] {
                case.compare(f, elem, |f, side| {
                    f.instruction(&Instruction::LocalGet(if side == A { first } else { first + 1 }));
                    f.instruction(&Instruction::LocalGet(I));
                    f.instruction(&Instruction::ArrayGet(array));
                });
            }
        });
    }

    /// Run `body` for each index `0..$n`, in `$i`.
    fn each(&self, f: &mut Function, body: impl Fn(&Self, &mut Function)) {
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(I));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(I));
        f.instruction(&Instruction::LocalGet(N));
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));
        body(self, f);
        f.instruction(&Instruction::LocalGet(I));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(I));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    }
}

/// With an `i32` on the stack that is nonzero when two things differ: return
/// 0 from the comparison if it is.
fn differ(f: &mut Function) {
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
}
