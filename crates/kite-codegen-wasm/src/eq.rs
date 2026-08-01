//! Structural equality on aggregates.
//!
//! The specification says two structs are equal when their fields are, and the
//! same for tuples, slices, enums and optionals. Reference identity is a
//! separate operation, spelled `ptr.same`.
//!
//! Wasm has no deep-equality instruction, so each aggregate type that is
//! compared gets a generated function. They may call each other — a struct
//! holding a slice of itself is legal — which is why every one is declared
//! before any body is emitted.
//!
//! The functions are emitted only for types a program actually compares. A
//! program that never writes `==` on an aggregate gets none of them.

use crate::*;
use std::collections::HashSet;

/// A generated comparison, in emission order. The function index is
/// `base + position`.
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
        for block in &f.blocks {
            for stmt in &block.stmts {
                let mir::Inst::Assign { value: mir::Rvalue::Binary { op, lhs, .. }, .. } = stmt
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
            | TyKind::Optional(_)
            | TyKind::Err
    )
}

/// The signature of a comparison: two values of the type, an `i32` verdict.
pub fn signature(ty: TyId, types: &Types, layout: &TypeLayout) -> (Vec<ValType>, Vec<ValType>) {
    let v = val_type_with(ty, types, layout);
    (vec![v, v], vec![ValType::I32])
}

/// Emit one comparison function.
pub struct EqBuilder<'a> {
    pub types: &'a Types,
    pub layout: &'a TypeLayout,
    /// Where each host function ended up, since a string comparison reaches
    /// for one and the import list is only as long as a module needs.
    pub hosts: &'a Hosts,
    /// Where the generated comparisons start in the function index space.
    pub base: u32,
    pub fns: &'a [EqFn],
}

impl EqBuilder<'_> {
    /// The function index comparing `ty`.
    pub fn index_of(&self, ty: TyId) -> Option<u32> {
        self.fns
            .iter()
            .position(|e| e.ty == ty)
            .map(|i| self.base + i as u32)
    }

    pub fn build(&self, ty: TyId) -> Function {
        match self.types.kind(ty) {
            TyKind::Struct(s) => self.struct_eq(*s),
            TyKind::Tuple(_) => self.tuple_eq(ty),
            TyKind::Enum(e) => self.enum_eq(*e),
            TyKind::Slice(elem) => self.slice_eq(ty, *elem),
            TyKind::Optional(inner) => self.optional_eq(ty, *inner),
            TyKind::Err => self.error_eq(),
            // `collect` only ever asks for the kinds above.
            _ => {
                let mut f = Function::new(Vec::new());
                f.instruction(&Instruction::Unreachable);
                f.instruction(&Instruction::End);
                f
            }
        }
    }

    /// Compare one field of two records already in locals 0 and 1, and return
    /// `false` from the enclosing function if they differ.
    ///
    /// Emitting an early return per field rather than folding with `and` is
    /// what makes comparison short-circuit: a mismatch in the first field of a
    /// large struct does not read the rest.
    fn compare_fields(
        &self,
        f: &mut Function,
        pairs: &[(TyId, u32, u32)],
        load: impl Fn(&mut Function, u32, u32, u32),
    ) {
        for (ty, record, field) in pairs {
            load(f, 0, *record, *field);
            load(f, 1, *record, *field);
            self.compare_values(f, *ty);
            f.instruction(&Instruction::I32Eqz);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::I32Const(0));
            f.instruction(&Instruction::Return);
            f.instruction(&Instruction::End);
        }
    }

    /// Compare two values of `ty` already on the stack, leaving an `i32`.
    fn compare_values(&self, f: &mut Function, ty: TyId) {
        match self.types.kind(ty) {
            TyKind::Int => f.instruction(&Instruction::I64Eq),
            TyKind::Float => f.instruction(&Instruction::F64Eq),
            TyKind::Bool => f.instruction(&Instruction::I32Eq),
            // A `str` is an index into the host's table, and two different
            // indices may name the same text, so this is a host call rather
            // than an integer comparison.
            TyKind::Str => f.instruction(&Instruction::Call(self.hosts.at(host::STR_EQ))),
            _ => match self.index_of(ty) {
                Some(i) => f.instruction(&Instruction::Call(i)),
                // Unreachable: `collect` closed over every component.
                None => f.instruction(&Instruction::Unreachable),
            },
        };
    }

    fn struct_eq(&self, s: kite_hir::StructId) -> Function {
        let mut f = Function::new(Vec::new());
        let record = self.layout.struct_type(s);
        let shift = self.layout.struct_shift(s);
        let pairs: Vec<(TyId, u32, u32)> = self
            .types
            .struct_def(s)
            .fields
            .iter()
            .enumerate()
            .map(|(i, fd)| (fd.ty, record, i as u32 + shift))
            .collect();
        self.compare_fields(&mut f, &pairs, struct_get);
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::End);
        f
    }

    fn tuple_eq(&self, ty: TyId) -> Function {
        let mut f = Function::new(Vec::new());
        let TyKind::Tuple(elems) = self.types.kind(ty) else {
            unreachable!("tuple_eq is only called for a tuple")
        };
        match self.layout.tuple_type(ty) {
            Some(record) => {
                let pairs: Vec<(TyId, u32, u32)> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (*e, record, i as u32))
                    .collect();
                self.compare_fields(&mut f, &pairs, struct_get);
                f.instruction(&Instruction::I32Const(1));
            }
            None => {
                f.instruction(&Instruction::Unreachable);
            }
        }
        f.instruction(&Instruction::End);
        f
    }

    /// Different variants are never equal; the same variant compares payloads.
    fn enum_eq(&self, e: kite_hir::EnumId) -> Function {
        let mut f = Function::new(Vec::new());
        let base = self.layout.enum_base_type(e);
        let shift = self.layout.enum_shift(e);

        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::StructGet { struct_type_index: base, field_index: shift });
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::StructGet { struct_type_index: base, field_index: shift });
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        // The tags now agree, so each arm may cast both sides to its variant.
        for (v, variant) in self.types.enum_def(e).variants.iter().enumerate() {
            if variant.fields.is_empty() {
                continue;
            }
            let record = self.layout.variant_type(e, v as u32);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::StructGet { struct_type_index: base, field_index: shift });
            f.instruction(&Instruction::I32Const(v as i32));
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::If(BlockType::Empty));
            let pairs: Vec<(TyId, u32, u32)> = variant
                .fields
                .iter()
                .enumerate()
                .map(|(i, fd)| (fd.ty, record, i as u32 + 1 + shift))
                .collect();
            self.compare_fields(&mut f, &pairs, cast_then_get);
            f.instruction(&Instruction::End);
        }

        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::End);
        f
    }

    /// Equal lengths, then equal elements. The loop stops at the first
    /// difference, which matters for the long slices this is worth using on.
    fn slice_eq(&self, ty: TyId, elem: TyId) -> Function {
        let Some(array) = self.layout.slice_type(elem) else {
            let mut f = Function::new(Vec::new());
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            return f;
        };
        let _ = ty;
        // One local: the cursor.
        let mut f = Function::new(vec![(1, ValType::I32)]);
        let i = 2;

        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));

        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(array));
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(array));
        self.compare_values(&mut f, elem);
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);

        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::End);
        f
    }

    /// `nil == nil` is true, `nil == x` is false, and two present values
    /// compare by their payloads.
    fn optional_eq(&self, ty: TyId, inner: TyId) -> Function {
        let mut f = Function::new(Vec::new());
        let Some(boxed) = self.layout.option_type(inner) else {
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            return f;
        };
        let _ = ty;

        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::RefIsNull);
        // Different presence: not equal. Both absent: equal.
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        struct_get(&mut f, 0, boxed, 0);
        struct_get(&mut f, 1, boxed, 0);
        self.compare_values(&mut f, inner);
        f.instruction(&Instruction::End);
        f
    }

    /// Two errors are equal when their messages are. `nil` is the no-error
    /// value, so presence is compared first.
    fn error_eq(&self) -> Function {
        let mut f = Function::new(Vec::new());
        let record = self.layout.error_record;

        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        struct_get(&mut f, 0, record, 0);
        struct_get(&mut f, 1, record, 0);
        f.instruction(&Instruction::Call(self.hosts.at(host::STR_EQ)));
        f.instruction(&Instruction::End);
        f
    }
}

/// Read a field of the record in `local`.
fn struct_get(f: &mut Function, local: u32, record: u32, field: u32) {
    f.instruction(&Instruction::LocalGet(local));
    f.instruction(&Instruction::StructGet {
        struct_type_index: record,
        field_index: field,
    });
}

/// Read a field of a variant, casting first. The tag has already been tested,
/// so the cast cannot fail.
fn cast_then_get(f: &mut Function, local: u32, record: u32, field: u32) {
    f.instruction(&Instruction::LocalGet(local));
    f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(record)));
    f.instruction(&Instruction::StructGet {
        struct_type_index: record,
        field_index: field,
    });
}
