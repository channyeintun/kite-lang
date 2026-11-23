//! Maps: the entries in insertion order, a hash index beside them, and the
//! helpers that read and write them.
//!
//! The specification calls `{K: V}` a hash map with a deterministic order. On
//! this backend a map was for a long time its order alone — a record of two
//! immutable arrays, scanned on every lookup and rebuilt whole on every write
//! — so a loop over a map of `n` entries cost `n²` where the VM and native,
//! which have kept an index since, cost `n`. A map is now what it is on those
//! two (see the VM's `map` module, whose rules this follows exactly):
//!
//! - The record holds the entries in insertion order, in a key array and a
//!   value array with room to spare, and how many slots are used (`len`) and
//!   how many of those hold an entry (`live`). Iteration, `keys()`,
//!   `values()` and `==` read the slots in order and nothing else, so nothing
//!   observable depends on the index.
//! - The index is two `i32` arrays: per bucket (a power of two of them) the
//!   first slot whose key hashes there, and per slot the next slot in its
//!   bucket — always a later one, so each bucket lists its entries in
//!   insertion order and the first equal key a lookup meets is the earliest.
//!   That is "first match wins", which matters only where a `var` field has
//!   turned one key into another.
//! - A map of a few entries has no index: below [`SMALL`] a scan is as fast
//!   as hashing. The index is built the first time a lookup, a write or a
//!   removal finds a larger map without one, which may be a map another local
//!   shares: building it changes nothing any holder can see, so that is no
//!   write in the value sense.
//! - A removal from an indexed map leaves a hole, so that it costs a constant
//!   rather than moving every later entry down; from a small one it moves them
//!   down. So holes only exist while there is an index, and a slot is a hole
//!   when its `next` is [`HOLE`]. Readers that walk the slots close the holes
//!   up first (`pack`), which keeps the order of what remains, drops the
//!   index, and changes nothing any holder can see either.
//! - The hash agrees with `==`: equal keys hash alike. See [`HashGen`].
//!
//! A map is a value, and its record is written in place only when the local
//! writing it owns it — the same owned flag the `slices` module describes —
//! and copied first otherwise. A record's arrays are never shared with
//! another record, so a write through one is seen through that one alone.
//!
//! All of it is small functions, one per map type and operation a program
//! uses, rather than code at each site. Each `m[k] = v` used to be a loop in
//! the function that wrote it, and a long map literal of computed entries —
//! lowered as a literal and then one `m[k] = v` per entry — became a function
//! of thousands of hot loops, which the engine recompiled whole. A program
//! with no map has none of these, and pays nothing for them.

use crate::*;

/// A map record's fields.
pub const KEYS: u32 = 0;
pub const VALUES: u32 = 1;
/// Slots used, holes included: where the next entry goes.
pub const LEN: u32 = 2;
/// Entries: what `m.len()` answers.
pub const LIVE: u32 = 3;
/// The index's buckets, or null when the map has none.
pub const HEADS: u32 = 4;
/// Per slot, the next slot in its bucket; null exactly when `HEADS` is.
pub const NEXT: u32 = 5;
/// Whether the map in a struct field is that field's alone. See the `slices`
/// module, whose header keeps the same mark for the same reason.
pub const MARK: u32 = 6;

/// The most entries a map holds before it is given an index: the VM's number.
const SMALL: i32 = 8;
/// The end of a bucket's list.
const END: i32 = -1;
/// A removed entry's slot, while the map is indexed.
const HOLE: i32 = -2;
const MIN_BUCKETS: i32 = 16;
/// How deep into a key the hash looks. See [`HashGen`].
const HASH_DEPTH: u32 = 3;

/// A map operation with a helper function of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum MapOp {
    /// `find(map, key) -> slot`, or -1.
    Find,
    /// `m[k]`: `get(map, key) -> Option<V>`.
    Get,
    /// `m[k] = v`: `set(map, owned, key, value) -> map`, the map to keep.
    Set,
    /// `m.remove(k)`: `remove(map, owned, key) -> map`, the map to keep.
    Remove,
    /// `copy(map, skip, extra) -> map`: a packed copy of the entries but the
    /// one in slot `skip`, with room for `extra` more, and no index.
    Copy,
    /// `pack(map)`: close up the holes in place, and drop the index.
    Pack,
    /// `index(map)`: build the index in place, over a map with no holes.
    Index,
    /// A literal's entries, collapsed so that a key named twice is one entry
    /// at its first position with its last value: `dedup(keys, values) ->
    /// map`. Only for a literal whose keys are not all constants, which are
    /// collapsed as it is compiled (see [`constant_entries`]).
    Dedup,
}

impl MapOp {
    /// The helpers this one calls.
    fn needs(self) -> &'static [MapOp] {
        match self {
            MapOp::Get => &[MapOp::Find],
            MapOp::Set => &[MapOp::Find, MapOp::Copy, MapOp::Index],
            MapOp::Remove => &[MapOp::Find, MapOp::Copy, MapOp::Pack],
            MapOp::Find => &[MapOp::Index],
            MapOp::Dedup => &[MapOp::Set],
            MapOp::Copy | MapOp::Pack | MapOp::Index => &[],
        }
    }
}

/// One helper: an operation on a map type, or the hash of a key type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Helper {
    Op(TyId, MapOp),
    Hash(TyId),
}

impl Helper {
    fn order(&self) -> (u8, u32, Option<MapOp>) {
        match self {
            Helper::Op(t, op) => (0, t.0, Some(*op)),
            Helper::Hash(t) => (1, t.0, None),
        }
    }
}

/// The helpers a program calls, and where they start in the function index
/// space.
pub struct MapHelpers {
    list: Vec<Helper>,
    pub base: u32,
}

impl MapHelpers {
    /// Every (map type, operation) a program uses, with the operations and
    /// key hashes those call, in a stable order.
    pub fn collect(program: &mir::Program, types: &Types, base: u32) -> MapHelpers {
        let mut ops: Vec<(TyId, MapOp)> = Vec::new();
        for f in &program.fns {
            let map_of = |l: &mir::Local| {
                let ty = f.locals[l.index()].ty;
                matches!(types.kind(ty), TyKind::Map(..)).then_some(ty)
            };
            for b in &f.blocks {
                for s in &b.stmts {
                    let found = match s {
                        mir::Inst::MapSet { local, .. } => map_of(local).map(|t| (t, MapOp::Set)),
                        mir::Inst::MapRemove { local, .. } => {
                            map_of(local).map(|t| (t, MapOp::Remove))
                        }
                        mir::Inst::Assign {
                            value:
                                mir::Rvalue::MapGet {
                                    base: mir::Operand::Local(l),
                                    ..
                                },
                            ..
                        } => map_of(l).map(|t| (t, MapOp::Get)),
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
                        if !ops.contains(&k) {
                            ops.push(k);
                        }
                    }
                }
            }
        }
        let mut at = 0;
        while at < ops.len() {
            let (t, op) = ops[at];
            for need in op.needs() {
                if !ops.contains(&(t, *need)) {
                    ops.push((t, *need));
                }
            }
            at += 1;
        }
        let mut list: Vec<Helper> = ops.iter().map(|&(t, op)| Helper::Op(t, op)).collect();
        for &(t, op) in &ops {
            let TyKind::Map(k, _) = *types.kind(t) else {
                continue;
            };
            if op != MapOp::Index {
                continue;
            }
            for h in [Some(k), reaches_str(k, types, 0).then_some(TyId::STR)]
                .into_iter()
                .flatten()
            {
                if !list.contains(&Helper::Hash(h)) {
                    list.push(Helper::Hash(h));
                }
            }
        }
        list.sort_by_key(Helper::order);
        MapHelpers { list, base }
    }

    fn at(&self, h: Helper) -> Option<u32> {
        self.list
            .iter()
            .position(|k| *k == h)
            .map(|i| self.base + i as u32)
    }

    /// The function index of a helper, when the program uses it.
    pub fn index(&self, map: TyId, op: MapOp) -> Option<u32> {
        self.at(Helper::Op(map, op))
    }

    /// `pack(map)`, when the program can leave holes in a map of this type —
    /// only a removal can — and so a reader of its slots must close them up.
    pub fn pack(&self, map: TyId) -> Option<u32> {
        self.index(map, MapOp::Pack)
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
        for h in &self.list {
            let (params, results) = match *h {
                Helper::Hash(k) => (vec![val_type_with(k, types, layout)], vec![ValType::I32]),
                Helper::Op(map, op) => match layout.map_layout(map) {
                    None => (vec![], vec![]),
                    Some(ml) => {
                        let record = nullable(ml.record);
                        let key = val_type_with(ml.key_ty, types, layout);
                        let value = val_type_with(ml.value_ty, types, layout);
                        match op {
                            MapOp::Find => (vec![record, key], vec![ValType::I32]),
                            MapOp::Get => match answer(ml.value_ty, types, layout) {
                                Some((result, _)) => (vec![record, key], vec![result]),
                                None => (vec![], vec![]),
                            },
                            MapOp::Set => {
                                (vec![record, ValType::I32, key, value], vec![record])
                            }
                            MapOp::Remove => {
                                (vec![record, ValType::I32, key], vec![record, ValType::I32])
                            }
                            MapOp::Copy => {
                                (vec![record, ValType::I32, ValType::I32], vec![record])
                            }
                            MapOp::Pack | MapOp::Index => (vec![record], vec![]),
                            MapOp::Dedup => {
                                (vec![nullable(ml.keys), nullable(ml.values)], vec![record])
                            }
                        }
                    }
                },
            };
            section.ty().function(params, results);
            out.push(next);
            next += 1;
        }
        out
    }

    /// Emit each helper's body, in the order [`Self::add_types`] declared them.
    pub fn emit(&self, code: &mut CodeSection, keys: KeyEquality<'_>) {
        for h in &self.list {
            let body = match *h {
                Helper::Hash(k) if k == TyId::STR => Some(hash_str(keys.layout)),
                Helper::Hash(k) => {
                    let mut g = HashGen::new(keys, self.at(Helper::Hash(TyId::STR)), 1);
                    g.code.push(Instruction::LocalGet(0));
                    g.value(k, 0);
                    Some(g.finish())
                }
                Helper::Op(map, op) => keys.layout.map_layout(map).and_then(|ml| {
                    let parts = Parts {
                        ml,
                        keys,
                        i32s: keys.layout.str_array,
                        helpers: self,
                        map,
                    };
                    match op {
                        MapOp::Find => parts.find(),
                        MapOp::Get => parts.lookup(),
                        MapOp::Set => parts.set(),
                        MapOp::Remove => parts.remove(),
                        MapOp::Copy => Some(parts.copy()),
                        MapOp::Pack => Some(parts.pack()),
                        MapOp::Index => parts.index(),
                        MapOp::Dedup => parts.dedup(),
                    }
                }),
            };
            code.function(&body.unwrap_or_else(|| {
                let mut f = Function::new(Vec::new());
                f.instruction(&Instruction::Unreachable);
                f.instruction(&Instruction::End);
                f
            }));
        }
    }
}

/// The record a map type is, with its fields: two arrays of the entries,
/// their counts, the index and the field mark. Every field is mutable, and
/// only ever written by a helper that owns the record, or that changes
/// nothing a holder can see (building the index, closing up holes).
pub fn record_fields(ml: MapLayout, i32s: u32) -> Vec<FieldType> {
    let field = |t: ValType| FieldType {
        element_type: StorageType::Val(t),
        mutable: true,
    };
    let fields = vec![
        field(nullable(ml.keys)),
        field(nullable(ml.values)),
        field(ValType::I32),
        field(ValType::I32),
        field(nullable(i32s)),
        field(nullable(i32s)),
        field(ValType::I32),
    ];
    debug_assert_eq!(fields.len() as u32, MARK + 1);
    fields
}

/// Push a new record over `keys` and `values` (already on the stack) holding
/// `n` entries, with no index and no mark.
pub fn new_record(f: &mut Function, ml: MapLayout, n: i32) {
    f.instruction(&Instruction::I32Const(n));
    f.instruction(&Instruction::I32Const(n));
    f.instruction(&null());
    f.instruction(&null());
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::StructNew(ml.record));
}

/// What a lookup that may miss answers with, for a payload of `payload`:
/// the optional's type, and the box to wrap a found value in.
///
/// `Option<Option<T>>` is `Option<T>` — the checker flattens it — so when
/// the payload is already optional the found value *is* the answer and is
/// not boxed again. A map of optional values or a slice of them used to
/// look for a box around an optional, find none, and trap.
pub fn answer(payload: TyId, types: &Types, layout: &TypeLayout) -> Option<(ValType, Option<u32>)> {
    if matches!(types.kind(payload), TyKind::Optional(_)) {
        return Some((val_type_with(payload, types, layout), None));
    }
    let boxed = layout.option_type(payload)?;
    Some((nullable(boxed), Some(boxed)))
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

fn null() -> Instruction<'static> {
    Instruction::RefNull(HeapType::Abstract {
        shared: false,
        ty: wasm_encoder::AbstractHeapType::None,
    })
}

/// One map type's helpers, being emitted.
#[derive(Clone, Copy)]
struct Parts<'a> {
    ml: MapLayout,
    keys: KeyEquality<'a>,
    /// The `i32` array the index is made of: the string array, which every
    /// module declares.
    i32s: u32,
    helpers: &'a MapHelpers,
    map: TyId,
}

impl Parts<'_> {
    fn get(self, f: &mut Function, local: u32, field: u32) {
        f.instruction(&Instruction::LocalGet(local));
        f.instruction(&Instruction::StructGet {
            struct_type_index: self.ml.record,
            field_index: field,
        });
    }

    /// Set `field` of the record in `local` to what `value` pushes.
    fn put(self, f: &mut Function, local: u32, field: u32, value: impl FnOnce(&mut Function)) {
        f.instruction(&Instruction::LocalGet(local));
        value(f);
        f.instruction(&Instruction::StructSet {
            struct_type_index: self.ml.record,
            field_index: field,
        });
    }

    /// Add `by` to the count in `field`.
    fn bump(self, f: &mut Function, local: u32, field: u32, by: i32) {
        self.put(f, local, field, |f| {
            self.get(f, local, field);
            f.instruction(&Instruction::I32Const(by));
            f.instruction(&Instruction::I32Add);
        });
    }

    fn op(self, op: MapOp) -> Option<u32> {
        self.helpers.index(self.map, op)
    }

    fn hash(self) -> Option<u32> {
        self.helpers.at(Helper::Hash(self.ml.key_ty))
    }

    fn key_equality(self, f: &mut Function) {
        key_equality(f, self.ml.key_ty, self.keys);
    }

    /// With a key on the stack, its bucket in the index of the record in
    /// `local`.
    fn bucket(self, f: &mut Function, local: u32, hash: u32) {
        f.instruction(&Instruction::Call(hash));
        self.get(f, local, HEADS);
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::I32And);
    }

    /// `while i < n { body; i += 1 }`, with `i` starting where it is.
    fn upto(f: &mut Function, i: u32, n: impl Fn(&mut Function), body: impl FnOnce(&mut Function)) {
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(i));
        n(f);
        f.instruction(&Instruction::I32GeS);
        f.instruction(&Instruction::BrIf(1));
        body(f);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    }

    /// `find(map, key) -> slot`: the slot holding `key`, the earliest should
    /// two hold equal keys, or -1. A scan of a small map; otherwise a walk of
    /// the key's bucket, after building the index if the map has none.
    fn find(self) -> Option<Function> {
        let (index, hash) = (self.op(MapOp::Index)?, self.hash()?);
        // Parameters: 0 map, 1 key. Locals: 2 slot.
        let (m, key, i) = (0, 1, 2);
        let mut f = Function::new(vec![(1, ValType::I32)]);
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        self.get(&mut f, m, LEN);
        f.instruction(&Instruction::I32Const(SMALL));
        f.instruction(&Instruction::I32GtS);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::Call(index));
        f.instruction(&Instruction::Else);
        // Small, so no holes: every slot below `len` is an entry.
        Self::upto(
            &mut f,
            i,
            |f| self.get(f, m, LEN),
            |f| {
                self.get(f, m, KEYS);
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::ArrayGet(self.ml.keys));
                f.instruction(&Instruction::LocalGet(key));
                self.key_equality(f);
                f.instruction(&Instruction::If(BlockType::Empty));
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::Return);
                f.instruction(&Instruction::End);
            },
        );
        f.instruction(&Instruction::I32Const(END));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);

        // The bucket's list, in insertion order. A hole is never on one.
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::LocalGet(key));
        self.bucket(&mut f, m, hash);
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::BrIf(1));
        self.get(&mut f, m, KEYS);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.ml.keys));
        f.instruction(&Instruction::LocalGet(key));
        self.key_equality(&mut f);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::I32Const(END));
        f.instruction(&Instruction::End);
        Some(f)
    }

    /// `get(map, key) -> Option<V>`.
    fn lookup(self) -> Option<Function> {
        let find = self.op(MapOp::Find)?;
        let (result, wrap) = answer(self.ml.value_ty, self.keys.types, self.keys.layout)?;
        // Parameters: 0 map, 1 key. Locals: 2 slot.
        let mut f = Function::new(vec![(1, ValType::I32)]);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(1));
        f.instruction(&Instruction::Call(find));
        f.instruction(&Instruction::LocalTee(2));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::If(BlockType::Result(result)));
        f.instruction(&null());
        f.instruction(&Instruction::Else);
        self.get(&mut f, 0, VALUES);
        f.instruction(&Instruction::LocalGet(2));
        f.instruction(&Instruction::ArrayGet(self.ml.values));
        if let Some(boxed) = wrap {
            f.instruction(&Instruction::StructNew(boxed));
        }
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        Some(f)
    }

    /// Replace the record in `m` with a packed copy of its own, with room for
    /// `extra` more entries, unless `owned` says it is `m`'s alone already.
    fn own(self, f: &mut Function, m: u32, owned: u32, extra: i32) -> Option<()> {
        let copy = self.op(MapOp::Copy)?;
        f.instruction(&Instruction::LocalGet(owned));
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::I32Const(-1));
        f.instruction(&Instruction::I32Const(extra));
        f.instruction(&Instruction::Call(copy));
        f.instruction(&Instruction::LocalSet(m));
        f.instruction(&Instruction::End);
        Some(())
    }

    /// `set(map, owned, key, value) -> map`: the entry for `key` given
    /// `value`, keeping its position and its stored key — `-0.0` equals
    /// `0.0`, and writing the new key over the stored one would change what
    /// iterating the map prints — or a new entry on the end.
    fn set(self) -> Option<Function> {
        let (find, index, hash) = (self.op(MapOp::Find)?, self.op(MapOp::Index)?, self.hash()?);
        // Parameters: 0 map, 1 owned, 2 key, 3 value. Locals: 4 slot, then
        // the new entry's; 5 a bucket, then a new capacity, then a slot; 6 a
        // slot; 7, 8, 9 grown arrays.
        let (m, owned, key, value, slot, b, i) = (0, 1, 2, 3, 4, 5, 6);
        let (grown_keys, grown_values, grown_next) = (7, 8, 9);
        let mut f = Function::new(vec![
            (3, ValType::I32),
            (1, nullable(self.ml.keys)),
            (1, nullable(self.ml.values)),
            (1, nullable(self.i32s)),
        ]);
        self.own(&mut f, m, owned, 1)?;
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::LocalGet(key));
        f.instruction(&Instruction::Call(find));
        f.instruction(&Instruction::LocalTee(slot));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32GeS);
        f.instruction(&Instruction::If(BlockType::Empty));
        self.get(&mut f, m, VALUES);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::LocalGet(value));
        f.instruction(&Instruction::ArraySet(self.ml.values));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        // A new entry goes in slot `len`. When the arrays are full, each moves
        // to one twice as long and four more: 4, 12, 28, … so a map built up
        // from empty reallocates a logarithmic number of times.
        self.get(&mut f, m, LEN);
        f.instruction(&Instruction::LocalTee(slot));
        self.get(&mut f, m, KEYS);
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::I32Const(4));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(b));
        for (field, array, grown) in [
            (KEYS, self.ml.keys, grown_keys),
            (VALUES, self.ml.values, grown_values),
            (NEXT, self.i32s, grown_next),
        ] {
            // An index's `next` grows with the slots; a map with no index has
            // none.
            if field == NEXT {
                self.get(&mut f, m, NEXT);
                f.instruction(&Instruction::RefIsNull);
                f.instruction(&Instruction::I32Eqz);
                f.instruction(&Instruction::If(BlockType::Empty));
            }
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::ArrayNewDefault(array));
            f.instruction(&Instruction::LocalTee(grown));
            // array.copy takes dest, dest_offset, src, src_offset, len.
            f.instruction(&Instruction::I32Const(0));
            self.get(&mut f, m, field);
            f.instruction(&Instruction::I32Const(0));
            f.instruction(&Instruction::LocalGet(slot));
            f.instruction(&Instruction::ArrayCopy {
                array_type_index_dst: array,
                array_type_index_src: array,
            });
            self.put(&mut f, m, field, |f| {
                f.instruction(&Instruction::LocalGet(grown));
            });
            if field == NEXT {
                f.instruction(&Instruction::End);
            }
        }
        f.instruction(&Instruction::End);

        for (field, array, written) in [(KEYS, self.ml.keys, key), (VALUES, self.ml.values, value)] {
            self.get(&mut f, m, field);
            f.instruction(&Instruction::LocalGet(slot));
            f.instruction(&Instruction::LocalGet(written));
            f.instruction(&Instruction::ArraySet(array));
        }
        self.bump(&mut f, m, LEN, 1);
        self.bump(&mut f, m, LIVE, 1);

        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        // No index yet: one is due once the map is no longer small.
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Const(SMALL));
        f.instruction(&Instruction::I32GeS);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::Call(index));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::Else);
        // Indexed: the new slot goes on the end of its bucket's list, which
        // keeps the list in insertion order.
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Const(END));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::LocalGet(key));
        self.bucket(&mut f, m, hash);
        f.instruction(&Instruction::LocalSet(b));
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalTee(i));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::If(BlockType::Empty));
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::Else);
        // Walk to the last slot of the list; `b` holds each next in turn.
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalTee(b));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::End);
        // More slots than buckets: a bigger index, over the entries with
        // their holes closed up.
        self.get(&mut f, m, LEN);
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::I32GtS);
        f.instruction(&Instruction::If(BlockType::Empty));
        if let Some(pack) = self.op(MapOp::Pack) {
            f.instruction(&Instruction::LocalGet(m));
            f.instruction(&Instruction::Call(pack));
        }
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::Call(index));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::End);
        Some(f)
    }

    /// `remove(map, owned, key) -> (map, owned)`. A key that is not there is
    /// not a mistake — dropping state for a row that never had any is the
    /// ordinary case — and leaves the local holding the map it had, owned or
    /// not, with nothing copied: `ptr.same` can tell, and on the VM and
    /// natively a miss keeps the map's identity too. A hit on a map the local
    /// does not own is a copy without the entry.
    ///
    /// A small map moves the later entries down; an indexed one takes the
    /// slot off its bucket's list and leaves a hole, which every reader of
    /// the slots skips or closes up, so the order of what remains is the
    /// order moving them down gave. When the holes are more than the
    /// entries, they are closed up here.
    fn remove(self) -> Option<Function> {
        let (find, hash, pack) = (self.op(MapOp::Find)?, self.hash()?, self.op(MapOp::Pack)?);
        // Parameters: 0 map, 1 owned, 2 key. Locals: 3 slot, 4 bucket, 5 the
        // slot before, 6 the one after it.
        let (m, owned, key, slot, b, i, t) = (0, 1, 2, 3, 4, 5, 6);
        let copy = self.op(MapOp::Copy)?;
        let mut f = Function::new(vec![(4, ValType::I32)]);
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::LocalGet(key));
        f.instruction(&Instruction::Call(find));
        f.instruction(&Instruction::LocalTee(slot));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::LocalGet(owned));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(owned));
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Call(copy));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Empty));
        // Small: [slot + 1, len) moves down to `slot`. Either part may be
        // empty, and a zero-length `array.copy` is a no-op.
        for (field, array) in [(KEYS, self.ml.keys), (VALUES, self.ml.values)] {
            self.get(&mut f, m, field);
            f.instruction(&Instruction::LocalGet(slot));
            self.get(&mut f, m, field);
            f.instruction(&Instruction::LocalGet(slot));
            f.instruction(&Instruction::I32Const(1));
            f.instruction(&Instruction::I32Add);
            self.get(&mut f, m, LEN);
            f.instruction(&Instruction::LocalGet(slot));
            f.instruction(&Instruction::I32Sub);
            f.instruction(&Instruction::I32Const(1));
            f.instruction(&Instruction::I32Sub);
            f.instruction(&Instruction::ArrayCopy {
                array_type_index_dst: array,
                array_type_index_src: array,
            });
        }
        self.bump(&mut f, m, LEN, -1);
        self.bump(&mut f, m, LIVE, -1);
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);

        // Indexed: off its list. The stored key is hashed, which is the hash
        // it was linked by.
        self.get(&mut f, m, KEYS);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::ArrayGet(self.ml.keys));
        self.bucket(&mut f, m, hash);
        f.instruction(&Instruction::LocalSet(b));
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalTee(i));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));
        self.get(&mut f, m, HEADS);
        f.instruction(&Instruction::LocalGet(b));
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::Else);
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::LocalTee(t));
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::End);
        self.get(&mut f, m, NEXT);
        f.instruction(&Instruction::LocalGet(slot));
        f.instruction(&Instruction::I32Const(HOLE));
        f.instruction(&Instruction::ArraySet(self.i32s));
        self.bump(&mut f, m, LIVE, -1);

        // holes > SMALL && holes > live, with holes = len - live.
        self.get(&mut f, m, LEN);
        self.get(&mut f, m, LIVE);
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::LocalTee(t));
        f.instruction(&Instruction::I32Const(SMALL));
        f.instruction(&Instruction::I32GtS);
        f.instruction(&Instruction::LocalGet(t));
        self.get(&mut f, m, LIVE);
        f.instruction(&Instruction::I32GtS);
        f.instruction(&Instruction::I32And);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::Call(pack));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::End);
        Some(f)
    }

    /// Whether slot `i` of the record in `m` is a hole, as an `i32`: only an
    /// indexed map has any.
    fn is_hole(self, f: &mut Function, m: u32, i: u32) {
        self.get(f, m, NEXT);
        f.instruction(&Instruction::RefIsNull);
        f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Else);
        self.get(f, m, NEXT);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::I32Const(HOLE));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::End);
    }

    /// `copy(map, skip, extra) -> map`: a new record with arrays of its own
    /// holding the entries in order but the one in slot `skip`, with room
    /// for `extra` more, packed and with no index.
    fn copy(self) -> Function {
        // Parameters: 0 map, 1 skip, 2 extra. Locals: 3 i, 4 j, 5 keys,
        // 6 values.
        let (m, skip, extra, i, j, ks, vs) = (0, 1, 2, 3, 4, 5, 6);
        let mut f = Function::new(vec![
            (2, ValType::I32),
            (1, nullable(self.ml.keys)),
            (1, nullable(self.ml.values)),
        ]);
        for (array, into) in [(self.ml.keys, ks), (self.ml.values, vs)] {
            self.get(&mut f, m, LIVE);
            f.instruction(&Instruction::LocalGet(extra));
            f.instruction(&Instruction::I32Add);
            f.instruction(&Instruction::ArrayNewDefault(array));
            f.instruction(&Instruction::LocalSet(into));
        }
        Self::upto(
            &mut f,
            i,
            |f| self.get(f, m, LEN),
            |f| {
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::LocalGet(skip));
                f.instruction(&Instruction::I32Ne);
                self.is_hole(f, m, i);
                f.instruction(&Instruction::I32Eqz);
                f.instruction(&Instruction::I32And);
                f.instruction(&Instruction::If(BlockType::Empty));
                for (field, array, into) in [(KEYS, self.ml.keys, ks), (VALUES, self.ml.values, vs)] {
                    f.instruction(&Instruction::LocalGet(into));
                    f.instruction(&Instruction::LocalGet(j));
                    self.get(f, m, field);
                    f.instruction(&Instruction::LocalGet(i));
                    f.instruction(&Instruction::ArrayGet(array));
                    f.instruction(&Instruction::ArraySet(array));
                }
                f.instruction(&Instruction::LocalGet(j));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::I32Add);
                f.instruction(&Instruction::LocalSet(j));
                f.instruction(&Instruction::End);
            },
        );
        f.instruction(&Instruction::LocalGet(ks));
        f.instruction(&Instruction::LocalGet(vs));
        f.instruction(&Instruction::LocalGet(j));
        f.instruction(&Instruction::LocalGet(j));
        f.instruction(&null());
        f.instruction(&null());
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::StructNew(self.ml.record));
        f.instruction(&Instruction::End);
        f
    }

    /// `pack(map)`: the entries moved down over the holes, in order, and the
    /// index dropped, to be built again when a lookup wants it. Nothing a
    /// holder can see changes, so this is done to a shared map too.
    fn pack(self) -> Function {
        // Parameter 0 map. Locals: 1 i, 2 j.
        let (m, i, j) = (0, 1, 2);
        let mut f = Function::new(vec![(2, ValType::I32)]);
        self.get(&mut f, m, LIVE);
        self.get(&mut f, m, LEN);
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
        Self::upto(
            &mut f,
            i,
            |f| self.get(f, m, LEN),
            |f| {
                self.get(f, m, NEXT);
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::ArrayGet(self.i32s));
                f.instruction(&Instruction::I32Const(HOLE));
                f.instruction(&Instruction::I32Ne);
                f.instruction(&Instruction::If(BlockType::Empty));
                for (field, array) in [(KEYS, self.ml.keys), (VALUES, self.ml.values)] {
                    self.get(f, m, field);
                    f.instruction(&Instruction::LocalGet(j));
                    self.get(f, m, field);
                    f.instruction(&Instruction::LocalGet(i));
                    f.instruction(&Instruction::ArrayGet(array));
                    f.instruction(&Instruction::ArraySet(array));
                }
                f.instruction(&Instruction::LocalGet(j));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::I32Add);
                f.instruction(&Instruction::LocalSet(j));
                f.instruction(&Instruction::End);
            },
        );
        self.put(&mut f, m, LEN, |f| {
            f.instruction(&Instruction::LocalGet(j));
        });
        for field in [HEADS, NEXT] {
            self.put(&mut f, m, field, |f| {
                f.instruction(&null());
            });
        }
        f.instruction(&Instruction::End);
        f
    }

    /// `index(map)`: buckets for twice the entries, a power of two and at
    /// least sixteen, each listing its slots in insertion order — built from
    /// the last slot back, each put at the front of its list. The map has no
    /// holes: one is only ever indexed fresh, or after `pack`.
    fn index(self) -> Option<Function> {
        let hash = self.hash()?;
        // Parameter 0 map. Locals: 1 buckets, 2 slot, 3 bucket, 4 heads,
        // 5 next.
        let (m, buckets, i, b, heads, next) = (0, 1, 2, 3, 4, 5);
        let mut f = Function::new(vec![(3, ValType::I32), (2, nullable(self.i32s))]);
        f.instruction(&Instruction::I32Const(MIN_BUCKETS));
        f.instruction(&Instruction::LocalSet(buckets));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(buckets));
        self.get(&mut f, m, LEN);
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(buckets));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::LocalSet(buckets));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::I32Const(END));
        f.instruction(&Instruction::LocalGet(buckets));
        f.instruction(&Instruction::ArrayNew(self.i32s));
        f.instruction(&Instruction::LocalSet(heads));
        f.instruction(&Instruction::I32Const(END));
        self.get(&mut f, m, KEYS);
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::ArrayNew(self.i32s));
        f.instruction(&Instruction::LocalSet(next));
        self.get(&mut f, m, LEN);
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::LocalSet(i));
        self.get(&mut f, m, KEYS);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArrayGet(self.ml.keys));
        f.instruction(&Instruction::Call(hash));
        f.instruction(&Instruction::LocalGet(buckets));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::I32And);
        f.instruction(&Instruction::LocalSet(b));
        f.instruction(&Instruction::LocalGet(next));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::LocalGet(heads));
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::ArrayGet(self.i32s));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::LocalGet(heads));
        f.instruction(&Instruction::LocalGet(b));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::ArraySet(self.i32s));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        for (field, local) in [(HEADS, heads), (NEXT, next)] {
            self.put(&mut f, m, field, |f| {
                f.instruction(&Instruction::LocalGet(local));
            });
        }
        f.instruction(&Instruction::End);
        Some(f)
    }

    /// `dedup(keys, values) -> map`: a literal's entries written one at a
    /// time into a map of their own, which is the VM's rule exactly — a key
    /// named twice is one entry at its first position with its last value.
    fn dedup(self) -> Option<Function> {
        let set = self.op(MapOp::Set)?;
        // Parameters: 0 keys, 1 values. Locals: 2 i, 3 the map.
        let (ks, vs, i, m) = (0, 1, 2, 3);
        let mut f = Function::new(vec![(1, ValType::I32), (1, nullable(self.ml.record))]);
        for array in [self.ml.keys, self.ml.values] {
            f.instruction(&Instruction::LocalGet(ks));
            f.instruction(&Instruction::ArrayLen);
            f.instruction(&Instruction::ArrayNewDefault(array));
        }
        new_record(&mut f, self.ml, 0);
        f.instruction(&Instruction::LocalSet(m));
        Self::upto(
            &mut f,
            i,
            |f| {
                f.instruction(&Instruction::LocalGet(ks));
                f.instruction(&Instruction::ArrayLen);
            },
            |f| {
                f.instruction(&Instruction::LocalGet(m));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::LocalGet(ks));
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::ArrayGet(self.ml.keys));
                f.instruction(&Instruction::LocalGet(vs));
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::ArrayGet(self.ml.values));
                f.instruction(&Instruction::Call(set));
                f.instruction(&Instruction::LocalSet(m));
            },
        );
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::End);
        Some(f)
    }
}

/// `hash(str) -> i32`: FNV-1a over the scalar values, then mixed. `==` on
/// strings compares the scalars, so equal strings hash alike.
fn hash_str(layout: &TypeLayout) -> Function {
    // Parameter 0 the string. Locals: 1 i, 2 h.
    let (s, i, h) = (0, 1, 2);
    let mut f = Function::new(vec![(2, ValType::I32)]);
    f.instruction(&Instruction::I32Const(0x811c_9dc5_u32 as i32));
    f.instruction(&Instruction::LocalSet(h));
    Parts::upto(
        &mut f,
        i,
        |f| {
            f.instruction(&Instruction::LocalGet(s));
            f.instruction(&Instruction::ArrayLen);
        },
        |f| {
            f.instruction(&Instruction::LocalGet(h));
            f.instruction(&Instruction::LocalGet(s));
            f.instruction(&Instruction::LocalGet(i));
            f.instruction(&Instruction::ArrayGet(layout.str_array));
            f.instruction(&Instruction::I32Xor);
            f.instruction(&Instruction::I32Const(0x0100_0193));
            f.instruction(&Instruction::I32Mul);
            f.instruction(&Instruction::LocalSet(h));
        },
    );
    f.instruction(&Instruction::LocalGet(h));
    for inst in mix(h) {
        f.instruction(&inst);
    }
    f.instruction(&Instruction::End);
    f
}

/// Murmur3's finishing mix of the `i32` on the stack, through `scratch`, so
/// the low bits a bucket is chosen by depend on all of it.
fn mix(scratch: u32) -> Vec<Instruction<'static>> {
    let mut out = Vec::new();
    for (shift, times) in [(16, Some(0x85eb_ca6b_u32)), (13, Some(0xc2b2_ae35)), (16, None)] {
        out.push(Instruction::LocalTee(scratch));
        out.push(Instruction::LocalGet(scratch));
        out.push(Instruction::I32Const(shift));
        out.push(Instruction::I32ShrU);
        out.push(Instruction::I32Xor);
        if let Some(c) = times {
            out.push(Instruction::I32Const(c as i32));
            out.push(Instruction::I32Mul);
        }
    }
    out
}

/// Whether hashing a key of `ty` reaches a string, so the string hash is
/// needed: the same walk [`HashGen::value`] makes.
fn reaches_str(ty: TyId, types: &Types, depth: u32) -> bool {
    match types.kind(ty) {
        TyKind::Str | TyKind::Err => true,
        _ if depth >= HASH_DEPTH => false,
        TyKind::Optional(t) | TyKind::Slice(t) => reaches_str(*t, types, depth + 1),
        TyKind::Tuple(ts) => ts.iter().any(|t| reaches_str(*t, types, depth + 1)),
        TyKind::Struct(s) => {
            let def = types.struct_def(*s);
            !def.fields.iter().any(|f| f.mutable)
                && def.fields.iter().any(|f| reaches_str(f.ty, types, depth + 1))
        }
        TyKind::Enum(e) => types
            .enum_def(*e)
            .variants
            .iter()
            .any(|v| v.fields.iter().any(|f| reaches_str(f.ty, types, depth + 1))),
        _ => false,
    }
}

/// The body of a key type's hash: `hash(key) -> i32`.
///
/// **Equal keys hash alike**, because the hash only ever reads what `==`
/// compares, and reads it the way `==` does:
///
/// - an `int` or a `bool` by its value; a `float` by its bits, except that
///   `-0.0 == 0.0` and so both hash as `0.0` (`NaN` equals nothing, so its
///   hash does not matter);
/// - a string by its scalar values, and an error by its message, which is
///   what two errors are compared by;
/// - a tuple, an optional's payload, and a struct with no `var` field by
///   their parts in order; a slice by its length and its elements;
/// - an enum by its variant and that variant's payload, as a struct;
/// - a map by how many entries it has — coarser than `==`, never finer;
/// - a struct with a `var` field by its type alone. The field can change
///   while the struct is a key, through any other handle to it, and a hash of
///   the fields would have been taken before the change. The keys that differ
///   only inside such a struct share a bucket and are found by comparing
///   them, as every key once was — the VM's rule, and native's.
///
/// It stops [`HASH_DEPTH`] levels into a key, hashing anything deeper as a
/// constant. Every key of a type is cut off at the same place, so equal keys
/// still hash alike; a key whose differences are all deeper than that shares
/// a bucket with the others like it and is found by comparing. That bounds
/// the hash's code, which is emitted for each key type, and its stack: a key
/// may be a recursive type as deep as the program makes it. Width is bounded
/// the same way: past [`HASH_PARTS`] aggregates read, in the order the walk
/// meets them, the rest hash as a constant too — a tree-shaped enum's three
/// levels of variants would otherwise be kilobytes of hash. And an enum met
/// again inside its own payload is read by its variant alone, for the same
/// reason.
struct HashGen<'a> {
    keys: KeyEquality<'a>,
    str_hash: Option<u32>,
    first: u32,
    locals: Vec<ValType>,
    code: Vec<Instruction<'static>>,
    /// Aggregates the walk may still read into.
    parts: u32,
    /// The enums whose payload is being read, outermost first.
    open: Vec<kite_hir::EnumId>,
}

/// How many aggregates — tuples, structs, enums with a payload, slices,
/// optionals — one key type's hash reads into. See [`HashGen`].
const HASH_PARTS: u32 = 8;

impl<'a> HashGen<'a> {
    fn new(keys: KeyEquality<'a>, str_hash: Option<u32>, params: u32) -> Self {
        HashGen {
            keys,
            str_hash,
            first: params,
            locals: Vec::new(),
            code: Vec::new(),
            parts: HASH_PARTS,
            open: Vec::new(),
        }
    }

    /// Whether the walk may read into one more aggregate, counting it.
    fn take_part(&mut self) -> bool {
        if self.parts == 0 {
            return false;
        }
        self.parts -= 1;
        true
    }

    fn local(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.first + self.locals.len() as u32 - 1
    }

    fn push(&mut self, i: Instruction<'static>) {
        self.code.push(i);
    }

    /// With the hash on the stack, mix it and end the function.
    fn finish(mut self) -> Function {
        let h = self.local(ValType::I32);
        self.code.extend(mix(h));
        self.push(Instruction::End);
        let mut f = Function::new(self.locals.iter().map(|t| (1, *t)));
        for i in &self.code {
            f.instruction(i);
        }
        f
    }

    /// Replace the value of `ty` on the stack with a constant: what a part
    /// the hash does not look into contributes.
    fn constant(&mut self, ty: TyId, c: i32) {
        if !matches!(self.keys.types.kind(ty), TyKind::Unit) {
            self.push(Instruction::Drop);
        }
        self.push(Instruction::I32Const(c));
    }

    /// Fold the hashes of `parts`, each loaded from the value in `at` and then
    /// hashed, into one.
    fn fold(&mut self, at: u32, seed: i32, parts: Vec<(TyId, Vec<Instruction<'static>>)>, depth: u32) {
        let acc = self.local(ValType::I32);
        self.push(Instruction::I32Const(seed));
        self.push(Instruction::LocalSet(acc));
        for (ty, load) in parts {
            self.push(Instruction::LocalGet(acc));
            self.push(Instruction::I32Const(5));
            self.push(Instruction::I32Rotl);
            self.push(Instruction::LocalGet(at));
            self.code.extend(load);
            self.value(ty, depth + 1);
            self.push(Instruction::I32Xor);
            self.push(Instruction::I32Const(0x9e37_79b1_u32 as i32));
            self.push(Instruction::I32Mul);
            self.push(Instruction::LocalSet(acc));
        }
        self.push(Instruction::LocalGet(acc));
    }

    /// Hash the value of `ty` on the stack, leaving an `i32`.
    fn value(&mut self, ty: TyId, depth: u32) {
        let (types, layout) = (self.keys.types, self.keys.layout);
        let vt = val_type_with(ty, types, layout);
        let aggregate = match types.kind(ty) {
            TyKind::Optional(_) | TyKind::Tuple(_) | TyKind::Slice(_) => true,
            TyKind::Struct(s) => !types.struct_def(*s).fields.iter().any(|f| f.mutable),
            TyKind::Enum(e) => {
                !self.open.contains(e) && types.enum_def(*e).variants.iter().any(|v| !v.fields.is_empty())
            }
            _ => false,
        };
        if aggregate && depth < HASH_DEPTH && !self.take_part() {
            return self.constant(ty, 8);
        }
        match types.kind(ty) {
            TyKind::Str => match self.str_hash {
                Some(h) => self.push(Instruction::Call(h)),
                None => self.constant(ty, 0),
            },
            TyKind::Err => {
                let at = self.local(vt);
                self.push(Instruction::LocalTee(at));
                self.push(Instruction::RefIsNull);
                self.push(Instruction::If(BlockType::Result(ValType::I32)));
                self.push(Instruction::I32Const(1));
                self.push(Instruction::Else);
                self.push(Instruction::LocalGet(at));
                self.push(Instruction::StructGet {
                    struct_type_index: layout.error_record,
                    field_index: 0,
                });
                self.value(TyId::STR, depth);
                self.push(Instruction::End);
            }
            _ if depth >= HASH_DEPTH && matches!(vt, ValType::Ref(_)) => self.constant(ty, 7),
            TyKind::Optional(inner) => {
                let Some(boxed) = layout.option_type(*inner) else {
                    return self.constant(ty, 2);
                };
                let at = self.local(vt);
                self.push(Instruction::LocalTee(at));
                self.push(Instruction::RefIsNull);
                self.push(Instruction::If(BlockType::Result(ValType::I32)));
                self.push(Instruction::I32Const(2));
                self.push(Instruction::Else);
                self.push(Instruction::LocalGet(at));
                self.push(Instruction::StructGet {
                    struct_type_index: boxed,
                    field_index: 0,
                });
                self.value(*inner, depth + 1);
                self.push(Instruction::End);
            }
            TyKind::Tuple(elems) => {
                let Some(record) = layout.tuple_type(ty) else {
                    return self.constant(ty, 3);
                };
                let at = self.local(vt);
                self.push(Instruction::LocalSet(at));
                let parts = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        (*e, vec![Instruction::StructGet {
                            struct_type_index: record,
                            field_index: i as u32,
                        }])
                    })
                    .collect();
                self.fold(at, elems.len() as i32, parts, depth);
            }
            TyKind::Struct(s) => {
                let def = types.struct_def(*s);
                if def.fields.iter().any(|f| f.mutable) {
                    return self.constant(ty, s.index() as i32);
                }
                let (record, shift) = (layout.struct_type(*s), layout.struct_shift(*s));
                let at = self.local(vt);
                self.push(Instruction::LocalSet(at));
                let parts = def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, fd)| {
                        (fd.ty, vec![Instruction::StructGet {
                            struct_type_index: record,
                            field_index: i as u32 + shift,
                        }])
                    })
                    .collect();
                self.fold(at, s.index() as i32, parts, depth);
            }
            // The variant, and the payload of the one it is: `==` compares
            // the payloads of two values of one variant, so they are read
            // the way a struct's fields are. An enum read by its variant
            // alone put every `Id(n)` of a map in one bucket, and a map of
            // forty thousand of them took ten seconds where the VM took a
            // tenth of one.
            TyKind::Enum(e) => {
                let (base, shift) = (layout.enum_base_type(*e), layout.enum_shift(*e));
                let def = types.enum_def(*e);
                let tag = |g: &mut Self| {
                    g.push(Instruction::StructGet {
                        struct_type_index: base,
                        field_index: shift,
                    });
                };
                if self.open.contains(e) || def.variants.iter().all(|v| v.fields.is_empty()) {
                    return tag(self);
                }
                self.open.push(*e);
                let (at, variant, acc) =
                    (self.local(vt), self.local(ValType::I32), self.local(ValType::I32));
                self.push(Instruction::LocalTee(at));
                tag(self);
                self.push(Instruction::LocalTee(variant));
                self.push(Instruction::LocalSet(acc));
                for (v, variant_def) in def.variants.iter().enumerate() {
                    if variant_def.fields.is_empty() {
                        continue;
                    }
                    let record = layout.variant_type(*e, v as u32);
                    self.push(Instruction::LocalGet(variant));
                    self.push(Instruction::I32Const(v as i32));
                    self.push(Instruction::I32Eq);
                    self.push(Instruction::If(BlockType::Empty));
                    let parts = variant_def
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, fd)| {
                            (fd.ty, vec![
                                Instruction::RefCastNonNull(HeapType::Concrete(record)),
                                Instruction::StructGet {
                                    struct_type_index: record,
                                    field_index: i as u32 + 1 + shift,
                                },
                            ])
                        })
                        .collect();
                    self.fold(at, v as i32, parts, depth);
                    self.push(Instruction::LocalSet(acc));
                    self.push(Instruction::End);
                }
                self.open.pop();
                self.push(Instruction::LocalGet(acc));
            }
            TyKind::Map(..) => match layout.map_layout(ty) {
                Some(ml) => self.push(Instruction::StructGet {
                    struct_type_index: ml.record,
                    field_index: LIVE,
                }),
                None => self.constant(ty, 4),
            },
            TyKind::Slice(elem) => {
                let (Some(array), Some(header)) = (layout.slice_type(*elem), layout.slice_header(*elem))
                else {
                    return self.constant(ty, 5);
                };
                // The length, then each element folded in.
                let at = self.local(vt);
                let buf = self.local(nullable(array));
                let (i, n, acc) = (
                    self.local(ValType::I32),
                    self.local(ValType::I32),
                    self.local(ValType::I32),
                );
                self.push(Instruction::LocalTee(at));
                self.push(Instruction::StructGet {
                    struct_type_index: header,
                    field_index: 0,
                });
                self.push(Instruction::LocalSet(buf));
                self.push(Instruction::LocalGet(at));
                self.push(Instruction::StructGet {
                    struct_type_index: header,
                    field_index: 1,
                });
                self.push(Instruction::LocalTee(n));
                self.push(Instruction::LocalSet(acc));
                // From 0 each time: this may run once per element of an
                // enclosing slice.
                self.push(Instruction::I32Const(0));
                self.push(Instruction::LocalSet(i));
                self.push(Instruction::Block(BlockType::Empty));
                self.push(Instruction::Loop(BlockType::Empty));
                self.push(Instruction::LocalGet(i));
                self.push(Instruction::LocalGet(n));
                self.push(Instruction::I32GeS);
                self.push(Instruction::BrIf(1));
                self.push(Instruction::LocalGet(acc));
                self.push(Instruction::I32Const(5));
                self.push(Instruction::I32Rotl);
                self.push(Instruction::LocalGet(buf));
                self.push(Instruction::LocalGet(i));
                self.push(Instruction::ArrayGet(array));
                self.value(*elem, depth + 1);
                self.push(Instruction::I32Xor);
                self.push(Instruction::I32Const(0x9e37_79b1_u32 as i32));
                self.push(Instruction::I32Mul);
                self.push(Instruction::LocalSet(acc));
                self.push(Instruction::LocalGet(i));
                self.push(Instruction::I32Const(1));
                self.push(Instruction::I32Add);
                self.push(Instruction::LocalSet(i));
                self.push(Instruction::Br(0));
                self.push(Instruction::End);
                self.push(Instruction::End);
                self.push(Instruction::LocalGet(acc));
            }
            _ => match vt {
                // A float by its bits, with `-0.0` as `0.0`; then as an int.
                ValType::F64 => {
                    let x = self.local(ValType::F64);
                    self.push(Instruction::LocalTee(x));
                    self.push(Instruction::I64ReinterpretF64);
                    self.push(Instruction::I64Const(0));
                    self.push(Instruction::LocalGet(x));
                    self.push(Instruction::F64Const(0.0.into()));
                    self.push(Instruction::F64Ne);
                    self.push(Instruction::Select);
                    self.int();
                }
                ValType::I64 => self.int(),
                ValType::I32 => {}
                // A function, a trait object or a host value has no `==`,
                // and the checker refuses a key holding one.
                _ => self.constant(ty, 6),
            },
        }
    }

    /// Fold the `i64` on the stack into an `i32`: both halves.
    fn int(&mut self) {
        let x = self.local(ValType::I64);
        self.push(Instruction::LocalTee(x));
        self.push(Instruction::I32WrapI64);
        self.push(Instruction::LocalGet(x));
        self.push(Instruction::I64Const(32));
        self.push(Instruction::I64ShrU);
        self.push(Instruction::I32WrapI64);
        self.push(Instruction::I32Xor);
    }
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
