//! Maps: the helpers that write one, and that collapse a literal's repeats.
//!
//! A map is a record of two arrays, keys and values, in insertion order, and
//! a value like any other: `m[k] = v` builds new arrays rather than writing
//! into ones another local may share. Both jobs below loop over the entries,
//! and both used to be written out inline in the function that asked. That
//! made each `m[k] = v` a loop in its caller, and a caller with thousands of
//! them — a long map literal of computed entries is lowered as a literal and
//! then one `m[k] = v` per entry — became a function of thousands of hot
//! loops, which the engine then recompiled whole: a literal of nine thousand
//! entries took minutes. As functions of their own, one per map shape, the
//! loops are the only thing that gets hot.

use crate::*;

/// A map operation with a helper function of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum MapOp {
    /// `m[k] = v`: `set(map, key, value) -> map`, the map to keep.
    Set,
    /// A literal's entries, collapsed so that a key named twice is one entry
    /// at its first position with its last value: `dedup(keys, values) ->
    /// map`. Only for a literal whose keys are not all constants, which are
    /// collapsed as it is compiled (see [`constant_entries`]).
    Dedup,
}

/// The helpers a program calls, and where they start in the function index
/// space.
pub struct MapHelpers {
    list: Vec<(TyId, MapOp)>,
    pub base: u32,
}

impl MapHelpers {
    /// Every (map type, operation) a program uses, in a stable order.
    pub fn collect(program: &mir::Program, types: &Types, base: u32) -> MapHelpers {
        let mut list: Vec<(TyId, MapOp)> = Vec::new();
        for f in &program.fns {
            let map_of = |l: &mir::Local| {
                let ty = f.locals[l.index()].ty;
                matches!(types.kind(ty), TyKind::Map(..)).then_some(ty)
            };
            for b in &f.blocks {
                for s in &b.stmts {
                    let found = match s {
                        mir::Inst::MapSet { local, .. } => map_of(local).map(|t| (t, MapOp::Set)),
                        mir::Inst::Assign {
                            dst,
                            value: mir::Rvalue::MapNew { entries },
                        } => {
                            let keys: Vec<&mir::Operand> = entries.iter().step_by(2).collect();
                            match constant_entries(&keys, program) {
                                Some(_) => None,
                                None => map_of(dst).map(|t| (t, MapOp::Dedup)),
                            }
                        }
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
        list.sort_by_key(|(t, op)| (t.0, *op));
        MapHelpers { list, base }
    }

    /// The function index of a helper, when the program uses it.
    pub fn index(&self, map: TyId, op: MapOp) -> Option<u32> {
        self.list
            .iter()
            .position(|k| *k == (map, op))
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
        for (map, op) in &self.list {
            let Some(ml) = layout.map_layout(*map) else {
                section.ty().function(vec![], vec![]);
                out.push(next);
                next += 1;
                continue;
            };
            let record = nullable(ml.record);
            let (params, results) = match op {
                MapOp::Set => (
                    vec![
                        record,
                        val_type_with(ml.key_ty, types, layout),
                        val_type_with(ml.value_ty, types, layout),
                    ],
                    vec![record],
                ),
                MapOp::Dedup => (vec![nullable(ml.keys), nullable(ml.values)], vec![record]),
            };
            section.ty().function(params, results);
            out.push(next);
            next += 1;
        }
        out
    }

    /// Emit each helper's body, in the order [`Self::add_types`] declared them.
    pub fn emit(&self, code: &mut CodeSection, keys: KeyEquality<'_>) {
        for (map, op) in &self.list {
            let Some(ml) = keys.layout.map_layout(*map) else {
                let mut f = Function::new(Vec::new());
                f.instruction(&Instruction::Unreachable);
                f.instruction(&Instruction::End);
                code.function(&f);
                continue;
            };
            code.function(&match op {
                MapOp::Set => set(ml, keys),
                MapOp::Dedup => dedup(ml, keys),
            });
        }
    }
}

/// What comparing two keys needs: the same comparison `==` makes.
#[derive(Clone, Copy)]
pub struct KeyEquality<'a> {
    pub types: &'a Types,
    pub layout: &'a TypeLayout,
    pub strings: strings::StringRuntime,
    pub eq: &'a eq::EqBuilder<'a>,
}

fn nullable(index: u32) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(index),
    })
}

/// `set(map, key, value) -> map`: the entry for `key` replaced, keeping its
/// position, or added at the end — in new arrays either way, since the old
/// ones may be another local's too.
fn set(ml: MapLayout, keys: KeyEquality<'_>) -> Function {
    // Parameters: 0 map, 1 key, 2 value. Locals: 3 pos, 4 len, 5 the new
    // keys, 6 the new values.
    let (map, key, value, pos, len, new_keys, new_values) = (0, 1, 2, 3, 4, 5, 6);
    let mut f = Function::new(vec![
        (2, ValType::I32),
        (1, nullable(ml.keys)),
        (1, nullable(ml.values)),
    ]);
    let field = |f: &mut Function, field: u32| {
        f.instruction(&Instruction::LocalGet(map));
        f.instruction(&Instruction::StructGet {
            struct_type_index: ml.record,
            field_index: field,
        });
    };
    field(&mut f, 0);
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalSet(len));

    // Scan for the key, leaving `pos` at its index or at the length.
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(pos));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(pos));
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    field(&mut f, 0);
    f.instruction(&Instruction::LocalGet(pos));
    f.instruction(&Instruction::ArrayGet(ml.keys));
    f.instruction(&Instruction::LocalGet(key));
    key_equality(&mut f, ml.key_ty, keys);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(pos));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(pos));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    // Both arrays copied into ones as long as the old, or one longer when the
    // key was not there, and the entry written at `pos`.
    for (field_index, array, fresh, written) in [
        (0, ml.keys, new_keys, key),
        (1, ml.values, new_values, value),
    ] {
        f.instruction(&Instruction::LocalGet(len));
        f.instruction(&Instruction::LocalGet(pos));
        f.instruction(&Instruction::LocalGet(len));
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::ArrayNewDefault(array));
        f.instruction(&Instruction::LocalTee(fresh));
        // array.copy takes dest, dest_offset, src, src_offset, len.
        f.instruction(&Instruction::I32Const(0));
        field(&mut f, field_index);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(len));
        f.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: array,
            array_type_index_src: array,
        });
        f.instruction(&Instruction::LocalGet(fresh));
        f.instruction(&Instruction::LocalGet(pos));
        f.instruction(&Instruction::LocalGet(written));
        f.instruction(&Instruction::ArraySet(array));
    }
    f.instruction(&Instruction::LocalGet(new_keys));
    f.instruction(&Instruction::LocalGet(new_values));
    f.instruction(&Instruction::StructNew(ml.record));
    f.instruction(&Instruction::End);
    f
}

/// `dedup(keys, values) -> map`: a literal's arrays, which it has just made
/// and nothing else holds, collapsed in place so that a key named twice is
/// one entry at its first position with its last value — the VM's rule — and
/// then cut to the entries kept.
///
/// One pass over the entries, each looked for among those already kept.
fn dedup(ml: MapLayout, keys: KeyEquality<'_>) -> Function {
    // Parameters: 0 keys, 1 values. Locals: 2 n, 3 i, 4 j, 5 kept, 6 and 7
    // the shorter arrays.
    let (ks, vs, n, i, j, kept, short_keys, short_values) = (0, 1, 2, 3, 4, 5, 6, 7);
    let mut f = Function::new(vec![
        (4, ValType::I32),
        (1, nullable(ml.keys)),
        (1, nullable(ml.values)),
    ]);
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalSet(n));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    // j = the kept entry with this key, or `kept` when there is none.
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(j));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::LocalGet(kept));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::ArrayGet(ml.keys));
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::ArrayGet(ml.keys));
    key_equality(&mut f, ml.key_ty, keys);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(j));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    // A new key is kept at the end of what has been kept; either way the
    // value is the latest one. `j <= i` throughout, so nothing unread is
    // overwritten.
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::LocalGet(kept));
    f.instruction(&Instruction::I32Eq);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::ArrayGet(ml.keys));
    f.instruction(&Instruction::ArraySet(ml.keys));
    f.instruction(&Instruction::LocalGet(kept));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(kept));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(vs));
    f.instruction(&Instruction::LocalGet(j));
    f.instruction(&Instruction::LocalGet(vs));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::ArrayGet(ml.values));
    f.instruction(&Instruction::ArraySet(ml.values));

    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(i));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    // A map's length is its arrays' length, so drop the tail.
    f.instruction(&Instruction::LocalGet(kept));
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    for (array, whole, short) in [(ml.keys, ks, short_keys), (ml.values, vs, short_values)] {
        f.instruction(&Instruction::LocalGet(kept));
        f.instruction(&Instruction::ArrayNewDefault(array));
        f.instruction(&Instruction::LocalTee(short));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(whole));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(kept));
        f.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: array,
            array_type_index_src: array,
        });
        f.instruction(&Instruction::LocalGet(short));
        f.instruction(&Instruction::LocalSet(whole));
    }
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(ks));
    f.instruction(&Instruction::LocalGet(vs));
    f.instruction(&Instruction::StructNew(ml.record));
    f.instruction(&Instruction::End);
    f
}

/// Compare two keys already on the stack, leaving an `i32`.
///
/// A key is compared the way `==` compares it, so an aggregate key — a
/// struct, an enum, a tuple, an optional, a slice — goes through the same
/// generated function `==` on that type calls. This used to fall through to
/// `i32.eq` for anything that was not a number or a string, which is not an
/// instruction a reference can be given, and every map keyed by a struct
/// produced a module the validator refused.
pub fn key_equality(f: &mut Function, key_ty: TyId, keys: KeyEquality<'_>) {
    if matches!(keys.types.kind(key_ty), TyKind::Str) {
        f.instruction(&Instruction::Call(keys.strings.eq()));
        return;
    }
    if eq::needs_function(key_ty, keys.types) {
        keys.eq.call(f, key_ty);
        return;
    }
    let inst = match val_type_with(key_ty, keys.types, keys.layout) {
        ValType::I64 => Instruction::I64Eq,
        ValType::F64 => Instruction::F64Eq,
        ValType::I32 => Instruction::I32Eq,
        // A host value or a function has no `==`, and the checker refuses
        // both as a key (E0201). Nothing reaches here.
        _ => Instruction::Unreachable,
    };
    f.instruction(&inst);
}

/// A map literal's entries collapsed as it is compiled, when every key is a
/// constant: for each distinct key, in the order it first appears, the index
/// of that first key and of the last value given it — the VM's rule for a key
/// named twice. `None` when any key is something else — a local, an
/// aggregate, a float, whose `==` is not its bits — which may repeat at run
/// time and is collapsed there by the `dedup` helper.
///
/// Dropping a value named before a later one for the same key skips no work:
/// a MIR operand is a local or a constant, already computed.
///
/// Decided with a map, at any length. It used to be a scan of the keys kept
/// so far, given up past 4,096 keys as not worth it — and a table of nine
/// thousand distinct names then ran the run-time pass, quadratic in the
/// entries, for nothing: 42 seconds where the VM took a quarter of one.
pub fn constant_entries(
    keys: &[&mir::Operand],
    program: &mir::Program,
) -> Option<Vec<(usize, usize)>> {
    #[derive(PartialEq, Eq, Hash)]
    enum Key<'a> {
        Int(i64),
        Bool(bool),
        Str(&'a str),
    }
    let mut first: std::collections::HashMap<Key, usize> =
        std::collections::HashMap::with_capacity(keys.len());
    let mut entries: Vec<(usize, usize)> = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        let key = match k {
            mir::Operand::Int(v) => Key::Int(*v),
            mir::Operand::Bool(v) => Key::Bool(*v),
            mir::Operand::Str(s) => Key::Str(&program.strings[s.0 as usize]),
            _ => return None,
        };
        match first.entry(key) {
            std::collections::hash_map::Entry::Occupied(at) => entries[*at.get()].1 = i,
            std::collections::hash_map::Entry::Vacant(at) => {
                at.insert(entries.len());
                entries.push((i, i));
            }
        }
    }
    Some(entries)
}
