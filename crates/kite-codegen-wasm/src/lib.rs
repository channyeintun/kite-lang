//! The WebAssembly backend.
//!
//! Emits WebAssembly directly through `wasm-encoder` — no LLVM anywhere. That
//! is the architecture MoonBit validated, and it is what keeps builds
//! sub-second and the toolchain measured in megabytes.
//!
//! # Control flow
//!
//! MIR is a control-flow *graph*; Wasm has structured control flow and no
//! `goto`. This backend bridges the two with a **dispatch loop**: one `loop`
//! containing nested `block`s, entered through a `br_table` on a synthetic
//! program-counter local.
//!
//! ```wat
//! (block $exit
//!   (loop $dispatch
//!     (block $bb2 (block $bb1 (block $bb0
//!       (br_table 0 1 2 (local.get $pc)))
//!       ;; bb0 body — sets $pc and branches back to $dispatch
//!     )
//!     ;; bb1 body
//!   )
//! ))
//! ```
//!
//! This handles an arbitrary CFG, including irreducible ones, and engines
//! optimise the shape well. A relooper that recovers `if`/`loop` structure
//! directly would produce tighter code and is the obvious later improvement;
//! correctness first.
//!
//! # Strings
//!
//! String constants are passed to the host as indices into a table the
//! generated glue holds, so the module needs no linear memory at all. When JS
//! String Builtins are wired up this becomes an `externref` carrying the JS
//! string itself, with no copy at the boundary.

use kite_hir::{BinOp, Builtin, TyId, TyKind, Types, UnOp};
use kite_mir as mir;
use wasm_encoder::{
    BlockType, CodeSection, CompositeInnerType, CompositeType, EntityType, ExportKind,
    ElementSection, Elements, ExportSection, FieldType, Function, FunctionSection, HeapType,
    ImportSection, Instruction,
    Module, RefType, StorageType, SubType, TypeSection, ValType,
};

mod eq;
mod glue;
mod support;
pub use glue::{generate_glue, generate_page};
pub use support::{unsupported, Unsupported};

/// Host functions the module imports, as (name, params, results).
///
/// Deliberately small: the standard library replaces them from Phase 6. String
/// operations live here because a `str` is an index into a table the host
/// holds — which is also why the module needs no linear memory.
const IMPORTS: [(&str, &[ValType], &[ValType]); 23] = [
    ("print_int", &[ValType::I64], &[]),
    ("print_float", &[ValType::F64], &[]),
    ("print_bool", &[ValType::I32], &[]),
    ("print_str", &[ValType::I32], &[]),
    ("str_concat", &[ValType::I32, ValType::I32], &[ValType::I32]),
    ("str_eq", &[ValType::I32, ValType::I32], &[ValType::I32]),
    // Rendering for `\(expr)`. These share their formatting with the `print_*`
    // imports, so a value looks the same however it reaches the host.
    ("str_of_int", &[ValType::I64], &[ValType::I32]),
    ("str_of_float", &[ValType::F64], &[ValType::I32]),
    ("str_of_bool", &[ValType::I32], &[ValType::I32]),
    ("str_len", &[ValType::I32], &[ValType::I64]),
    // The drawing boundary: a filled rectangle and a run of text. Everything a
    // layout produces is one of the two, which is what lets a DOM renderer and
    // a canvas renderer meet the same interface.
    (
        "draw_rect",
        &[ValType::F64, ValType::F64, ValType::F64, ValType::F64, ValType::I64],
        &[],
    ),
    ("draw_text", &[ValType::F64, ValType::F64, ValType::I32, ValType::I64], &[]),
    ("measure_text", &[ValType::I32], &[ValType::F64]),
    ("line_height", &[], &[ValType::F64]),
    ("str_slice", &[ValType::I32, ValType::I64, ValType::I64], &[ValType::I32]),
    ("str_index_of", &[ValType::I32, ValType::I32], &[ValType::I64]),
    ("str_trim", &[ValType::I32], &[ValType::I32]),
    (
        "draw_clip",
        &[ValType::F64, ValType::F64, ValType::F64, ValType::F64],
        &[],
    ),
    ("draw_unclip", &[], &[]),
    // The scheduler lives in the host, because a queue of live tasks is
    // mutable state and Kite has none. What crosses is a closure the host
    // cannot look inside: it hands the reference back through `kite_poll`,
    // which is the module's own trampoline.
    ("task_spawn", &[ANY_REF], &[]),
    ("task_wake_at", &[ValType::I64], &[]),
    ("task_park", &[], &[]),
    ("time_now", &[], &[ValType::I64]),
];

const IMPORT_COUNT: u32 = IMPORTS.len() as u32;

/// Which host functions a module declares, and where each ended up.
///
/// Imported functions occupy the bottom of the function index space, so
/// dropping one shifts everything above it. That is the whole reason this is a
/// map rather than a set.
#[derive(Default)]
pub struct Hosts {
    declared: Vec<bool>,
    index: Vec<u32>,
    /// Where the module's own functions start.
    pub base: u32,
}

impl Hosts {
    fn new(declared: Vec<bool>) -> Hosts {
        let mut index = vec![0; declared.len()];
        let mut n = 0;
        for (i, d) in declared.iter().enumerate() {
            index[i] = n;
            if *d {
                n += 1;
            }
        }
        Hosts { declared, index, base: n }
    }

    fn declared(&self, i: u32) -> bool {
        self.declared[i as usize]
    }

    /// The function index of a host call. A call to an import that was not
    /// declared is a compiler bug — the scan and the emitter disagreed — and
    /// index 0 would call the wrong thing silently, so it is caught here.
    pub fn at(&self, i: u32) -> u32 {
        debug_assert!(
            self.declared(i),
            "emitting a call to `{}`, which the import scan did not mark used",
            IMPORTS[i as usize].0
        );
        self.index[i as usize]
    }
}

/// Which host functions a program reaches for.
///
/// Conservative where it has to be — `io.print` picks among four by the type
/// of its argument, so printing anything marks all four — and exact elsewhere.
/// Over-declaring costs bytes; under-declaring produces a module that will not
/// instantiate, which is why the lookup asserts rather than defaulting.
fn used_imports(program: &mir::Program, types: &Types, eq_fns: &[eq::EqFn]) -> Hosts {
    let mut used = vec![false; IMPORTS.len()];
    let mut mark = |i: u32| used[i as usize] = true;

    // A generated comparison calls out for string equality, and reaches it
    // through any aggregate holding a `str`.
    if !eq_fns.is_empty() {
        mark(host::STR_EQ);
    }

    for f in &program.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                let mir::Inst::Assign { value, .. } = s else { continue };
                match value {
                    mir::Rvalue::CallBuiltin { builtin, .. } => match builtin {
                        Builtin::IoPrint => {
                            mark(host::PRINT_INT);
                            mark(host::PRINT_FLOAT);
                            mark(host::PRINT_BOOL);
                            mark(host::PRINT_STR);
                        }
                        Builtin::DrawRect => mark(host::DRAW_RECT),
                        Builtin::DrawText => mark(host::DRAW_TEXT),
                        Builtin::TextWidth => mark(host::MEASURE_TEXT),
                        Builtin::TextHeight => mark(host::LINE_HEIGHT),
                        Builtin::DrawClip => mark(host::DRAW_CLIP),
                        Builtin::DrawUnclip => mark(host::DRAW_UNCLIP),
                        Builtin::TaskSpawn => mark(host::TASK_SPAWN),
                        Builtin::TaskWakeAt => mark(host::TASK_WAKE_AT),
                        Builtin::TaskPark => mark(host::TASK_PARK),
                        Builtin::TimeNow => mark(host::TIME_NOW),
                    },
                    mir::Rvalue::StrOp { op, .. } => mark(match op {
                        kite_hir::StrKind::Len => host::STR_LEN,
                        kite_hir::StrKind::Slice => host::STR_SLICE,
                        kite_hir::StrKind::IndexOf => host::STR_INDEX_OF,
                        kite_hir::StrKind::Trim => host::STR_TRIM,
                    }),
                    mir::Rvalue::Binary { op, .. } => match op {
                        BinOp::ConcatStr => mark(host::STR_CONCAT),
                        BinOp::EqStr | BinOp::NeStr => mark(host::STR_EQ),
                        _ => {}
                    },
                    mir::Rvalue::ToStr { from, .. } => match types.kind(*from) {
                        TyKind::Int => mark(host::STR_OF_INT),
                        TyKind::Float => mark(host::STR_OF_FLOAT),
                        _ => mark(host::STR_OF_BOOL),
                    },
                    _ => {}
                }
            }
        }
    }
    Hosts::new(used)
}

/// The type a closure's captured environment is seen as from outside. Two
/// closures of one Kite type capture different things, so the environment is
/// opaque at the call site and each thunk casts it back to its own record.
const ANY_REF: ValType = ValType::Ref(RefType {
    nullable: true,
    heap_type: HeapType::Abstract { shared: false, ty: wasm_encoder::AbstractHeapType::Any },
});

/// Import indices, by position in [`IMPORTS`].
mod host {
    pub const PRINT_INT: u32 = 0;
    pub const PRINT_FLOAT: u32 = 1;
    pub const PRINT_BOOL: u32 = 2;
    pub const PRINT_STR: u32 = 3;
    pub const STR_CONCAT: u32 = 4;
    pub const STR_EQ: u32 = 5;
    pub const STR_OF_INT: u32 = 6;
    pub const STR_OF_FLOAT: u32 = 7;
    pub const STR_OF_BOOL: u32 = 8;
    pub const STR_LEN: u32 = 9;
    pub const DRAW_RECT: u32 = 10;
    pub const DRAW_TEXT: u32 = 11;
    pub const MEASURE_TEXT: u32 = 12;
    pub const LINE_HEIGHT: u32 = 13;
    pub const STR_SLICE: u32 = 14;
    pub const STR_INDEX_OF: u32 = 15;
    pub const STR_TRIM: u32 = 16;
    pub const DRAW_CLIP: u32 = 17;
    pub const DRAW_UNCLIP: u32 = 18;
    pub const TASK_SPAWN: u32 = 19;
    pub const TASK_WAKE_AT: u32 = 20;
    pub const TASK_PARK: u32 = 21;
    pub const TIME_NOW: u32 = 22;
}

pub struct WasmModule {
    pub bytes: Vec<u8>,
    /// String constants, in index order. The glue turns these into real
    /// strings; the module only refers to them by index.
    pub strings: Vec<String>,
}

/// Where each kind of type lives in the type index space.
///
/// Struct types come first after the imports so a function signature can refer
/// to them, and they sit in one recursive group so mutually recursive
/// declarations work — which they must, because every Kite aggregate is a GC
/// reference and recursion needs no annotation from the user.
struct TypeLayout {
    struct_base: u32,
    /// Each enum contributes one base record plus one per variant, so its
    /// types are found through this offset table rather than by arithmetic.
    enum_base: Vec<u32>,
    /// One box record per distinct optional payload type. `Option<T>` is a
    /// nullable reference to a one-field record, so `nil` is a null reference
    /// and the payload keeps its own type rather than being erased.
    option_box: std::collections::HashMap<TyId, u32>,
    /// One array type per distinct slice element type.
    slice_array: std::collections::HashMap<TyId, u32>,
    /// The error record: a message index. `nil` is a null reference, which is
    /// what makes `return value, nil` read the way it does.
    error_record: u32,
    /// One record per distinct fallible value type. The pair is a single GC
    /// object so a function can return both slots at once.
    pair_record: std::collections::HashMap<TyId, u32>,
    /// One record per distinct tuple shape.
    tuple_record: std::collections::HashMap<TyId, u32>,
    /// One record per distinct map shape, holding parallel key and value
    /// arrays. Two arrays rather than an array of pairs keeps lookup a scan
    /// over one contiguous run, and reuses the array machinery slices need.
    map_record: std::collections::HashMap<TyId, MapLayout>,
    /// The root record every dispatchable aggregate extends: one immutable i32
    /// holding the concrete type's identity.
    ///
    /// WasmGC types are compared structurally, so `struct Circle { r: float }`
    /// and `struct Square { s: float }` are *the same* Wasm type and `ref.test`
    /// cannot tell them apart. A stored tag is what makes dispatch sound. Only
    /// types that appear in a vtable carry one, so a program without `dyn` pays
    /// nothing for it.
    object_record: u32,
    /// Types carrying a tag, by [`kite_hir::TypeTag::encode`].
    tagged: std::collections::HashSet<u32>,
    /// Per distinct `fn(..) -> ..` type: the uniform signature every closure of
    /// that type is called through, and the record a closure value is.
    ///
    /// The signature takes the captured environment as an `anyref` first
    /// parameter. Two closures of the same Kite type capture different things,
    /// so the environment cannot be in the signature — which is the whole
    /// reason a closure is a pair of a function reference and an opaque
    /// environment rather than just a function reference.
    closure_sig: std::collections::HashMap<TyId, u32>,
    closure_record: std::collections::HashMap<TyId, u32>,
    /// Per lifted function reached by a `ClosureNew`: the record holding its
    /// captures, and how many of its leading parameters they are.
    env_record: std::collections::HashMap<u32, (u32, usize)>,
}

#[derive(Clone, Copy)]
pub struct MapLayout {
    /// The record holding both arrays.
    pub record: u32,
    pub keys: u32,
    pub values: u32,
    pub key_ty: TyId,
    pub value_ty: TyId,
}

impl TypeLayout {
    /// How far a type's own fields are pushed down by the identity tag: one
    /// field for a dispatchable type, none otherwise.
    fn shift(&self, tag: kite_hir::TypeTag) -> u32 {
        u32::from(self.tagged.contains(&tag.encode()))
    }

    fn struct_shift(&self, id: kite_hir::StructId) -> u32 {
        self.shift(kite_hir::TypeTag::Struct(id))
    }

    fn enum_shift(&self, id: kite_hir::EnumId) -> u32 {
        self.shift(kite_hir::TypeTag::Enum(id))
    }

    fn struct_type(&self, id: kite_hir::StructId) -> u32 {
        self.struct_base + id.0
    }

    /// The common supertype for an enum. Every variant record extends it, so
    /// one nullable reference to this type describes any value of the enum.
    fn enum_base_type(&self, id: kite_hir::EnumId) -> u32 {
        self.enum_base[id.index()]
    }

    /// The record for one variant, which carries the tag plus that variant's
    /// payload.
    fn variant_type(&self, id: kite_hir::EnumId, variant: u32) -> u32 {
        self.enum_base[id.index()] + 1 + variant
    }

    fn closure_type(&self, ty: TyId) -> Option<u32> {
        self.closure_record.get(&ty).copied()
    }

    fn option_type(&self, payload: TyId) -> Option<u32> {
        self.option_box.get(&payload).copied()
    }

    fn slice_type(&self, elem: TyId) -> Option<u32> {
        self.slice_array.get(&elem).copied()
    }

    fn pair_type(&self, value: TyId) -> Option<u32> {
        self.pair_record.get(&value).copied()
    }

    fn tuple_type(&self, ty: TyId) -> Option<u32> {
        self.tuple_record.get(&ty).copied()
    }

    fn map_layout(&self, ty: TyId) -> Option<MapLayout> {
        self.map_record.get(&ty).copied()
    }
}

/// Every map shape a program mentions, in a stable order.
fn map_shapes(program: &mir::Program, types: &Types) -> Vec<TyId> {
    let mut seen = Vec::new();
    let note = |ty: TyId, seen: &mut Vec<TyId>| {
        if matches!(types.kind(ty), TyKind::Map(..)) && !seen.contains(&ty) {
            seen.push(ty);
        }
    };
    for f in &program.fns {
        note(f.ret, &mut seen);
        for l in &f.locals {
            note(l.ty, &mut seen);
        }
    }
    seen
}

/// Every tuple shape a program mentions, in a stable order.
fn tuple_shapes(program: &mir::Program, types: &Types) -> Vec<TyId> {
    let mut seen = Vec::new();
    let note = |ty: TyId, seen: &mut Vec<TyId>| {
        if matches!(types.kind(ty), TyKind::Tuple(_)) && !seen.contains(&ty) {
            seen.push(ty);
        }
    };
    for f in &program.fns {
        note(f.ret, &mut seen);
        for l in &f.locals {
            note(l.ty, &mut seen);
        }
    }
    seen
}

/// Every fallible value type a program mentions, in a stable order.
fn pair_values(program: &mir::Program, types: &Types) -> Vec<TyId> {
    let mut seen = Vec::new();
    let note = |ty: TyId, seen: &mut Vec<TyId>| {
        if let TyKind::Fallible(v) = types.kind(ty) {
            if !seen.contains(v) {
                seen.push(*v);
            }
        }
    };
    for f in &program.fns {
        note(f.ret, &mut seen);
        for l in &f.locals {
            note(l.ty, &mut seen);
        }
    }
    seen
}

/// Every slice element type a program mentions, in a stable order.
fn slice_elements(program: &mir::Program, types: &Types) -> Vec<TyId> {
    let mut seen = Vec::new();
    let note = |ty: TyId, seen: &mut Vec<TyId>| {
        if let TyKind::Slice(elem) = types.kind(ty) {
            if !seen.contains(elem) {
                seen.push(*elem);
            }
        }
    };
    for f in &program.fns {
        note(f.ret, &mut seen);
        for l in &f.locals {
            note(l.ty, &mut seen);
        }
    }
    for i in 0..types.struct_count() {
        for f in &types.struct_def(kite_hir::StructId(i as u32)).fields {
            note(f.ty, &mut seen);
        }
    }
    for i in 0..types.enum_count() {
        for v in &types.enum_def(kite_hir::EnumId(i as u32)).variants {
            for f in &v.fields {
                note(f.ty, &mut seen);
            }
        }
    }
    seen
}

/// Every optional payload type a program mentions, in a stable order.
fn option_payloads(program: &mir::Program, types: &Types) -> Vec<TyId> {
    let mut seen = Vec::new();
    let note = |ty: TyId, seen: &mut Vec<TyId>| {
        if let TyKind::Optional(inner) = types.kind(ty) {
            if !seen.contains(inner) {
                seen.push(*inner);
            }
        }
    };
    for f in &program.fns {
        note(f.ret, &mut seen);
        for l in &f.locals {
            note(l.ty, &mut seen);
        }
    }
    // A struct or variant field may be optional even when no local is.
    for i in 0..types.struct_count() {
        for f in &types.struct_def(kite_hir::StructId(i as u32)).fields {
            note(f.ty, &mut seen);
        }
    }
    for i in 0..types.enum_count() {
        for v in &types.enum_def(kite_hir::EnumId(i as u32)).variants {
            for f in &v.fields {
                note(f.ty, &mut seen);
            }
        }
    }
    seen
}

/// Compile a MIR program to a WebAssembly module.
pub fn compile(program: &mir::Program, types: &Types) -> WasmModule {
    let mut module = Module::new();

    // Type index space: import signatures, then structs, then each enum's base
    // record followed by its variants, then function signatures.
    // Only types reachable through a trait object need an identity tag.
    let tagged: std::collections::HashSet<u32> = program
        .vtables
        .iter()
        .flat_map(|v| v.entries.iter())
        .map(|e| e.tag.encode())
        .collect();
    // Every distinct function type a program mentions, and every lifted
    // function a `ClosureNew` builds. Signature types are declared before the
    // aggregate group because a closure record holds a reference to one, and a
    // type may only name types declared before it or in its own group.
    let mut fn_types: Vec<TyId> = Vec::new();
    let mut envs: Vec<(u32, usize)> = Vec::new();
    for f in &program.fns {
        for l in &f.locals {
            if matches!(types.kind(l.ty), TyKind::Fn { .. }) && !fn_types.contains(&l.ty) {
                fn_types.push(l.ty);
            }
        }
        for b in &f.blocks {
            for s in &b.stmts {
                if let mir::Inst::Assign {
                    value: mir::Rvalue::ClosureNew { func, captures }, ..
                } = s
                {
                    if !envs.iter().any(|(g, _)| *g == func.0) {
                        envs.push((func.0, captures.len()));
                    }
                }
            }
        }
    }
    fn_types.sort_by_key(|t| t.0);
    envs.sort_by_key(|(f, _)| *f);

    let sig_base = IMPORT_COUNT;
    let object_record = sig_base + fn_types.len() as u32;
    let struct_base = object_record + 1;
    let mut next = struct_base + types.struct_count() as u32;
    let mut enum_base = Vec::with_capacity(types.enum_count());
    for i in 0..types.enum_count() {
        enum_base.push(next);
        next += 1 + types.enum_def(kite_hir::EnumId(i as u32)).variants.len() as u32;
    }
    // Optional boxes need the layout to describe their payload, so build a
    // provisional one first and fill the table in.
    let mut layout = TypeLayout {
        object_record,
        tagged,
        closure_sig: fn_types
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, sig_base + i as u32))
            .collect(),
        closure_record: std::collections::HashMap::new(),
        env_record: std::collections::HashMap::new(),
        struct_base,
        enum_base,
        option_box: std::collections::HashMap::new(),
        slice_array: std::collections::HashMap::new(),
        error_record: 0,
        pair_record: std::collections::HashMap::new(),
        tuple_record: std::collections::HashMap::new(),
        map_record: std::collections::HashMap::new(),
    };
    let payloads = option_payloads(program, types);
    for p in &payloads {
        layout.option_box.insert(*p, next);
        next += 1;
    }
    let elements = slice_elements(program, types);
    for e in &elements {
        layout.slice_array.insert(*e, next);
        next += 1;
    }
    layout.error_record = next;
    next += 1;
    let pairs = pair_values(program, types);
    for v in &pairs {
        layout.pair_record.insert(*v, next);
        next += 1;
    }
    let tuples = tuple_shapes(program, types);
    for s in &tuples {
        layout.tuple_record.insert(*s, next);
        next += 1;
    }
    // A map needs three types: the record, and one array for each side.
    let maps = map_shapes(program, types);
    for m in &maps {
        let TyKind::Map(k, v) = *types.kind(*m) else { continue };
        layout.map_record.insert(
            *m,
            MapLayout { record: next, keys: next + 1, values: next + 2, key_ty: k, value_ty: v },
        );
        next += 3;
    }
    for ty in &fn_types {
        layout.closure_record.insert(*ty, next);
        next += 1;
    }
    for (func, count) in &envs {
        layout.env_record.insert(*func, (next, *count));
        next += 1;
    }
    let aggregate_count = next - IMPORT_COUNT;

    // ---- types -------------------------------------------------------------
    let mut type_section = TypeSection::new();
    for (_, params, results) in IMPORTS.iter() {
        type_section
            .ty()
            .function(params.iter().copied(), results.iter().copied());
    }

    // Every aggregate goes in one `rec` group: a field may name a type declared
    // later, and mutual recursion has to work — which it must, because every
    // Kite aggregate is a GC reference and recursion needs no annotation.
    if aggregate_count > 0 {
        let mut group: Vec<SubType> = Vec::with_capacity(aggregate_count as usize);

        // Closure signatures come first. They belong in the group rather than
        // ahead of it because a signature may mention an aggregate — a closure
        // taking a struct — and a type may only name a later one from inside
        // its own group.
        for ty in &fn_types {
            let TyKind::Fn { params, ret } = types.kind(*ty) else {
                unreachable!("only function types are collected here")
            };
            let mut ps = vec![ANY_REF];
            ps.extend(params.iter().map(|p| val_type_with(*p, types, &layout)));
            let results: Vec<ValType> =
                wasm_result_with(*ret, types, &layout).into_iter().collect();
            group.push(func_subtype(ps, results));
        }

        // The root every dispatchable aggregate extends. It is emitted whether
        // or not anything extends it; one unused type declaration is cheaper
        // than a conditional index space.
        group.push(struct_subtype(vec![tag_field()], None, false));

        for i in 0..types.struct_count() {
            let sid = kite_hir::StructId(i as u32);
            let def = types.struct_def(sid);
            let mut fields: Vec<FieldType> = Vec::with_capacity(def.fields.len() + 1);
            let tagged = layout.struct_shift(sid) == 1;
            if tagged {
                fields.push(tag_field());
            }
            fields.extend(def.fields.iter().map(|f| FieldType {
                element_type: StorageType::Val(val_type_with(f.ty, types, &layout)),
                // Kite's per-field `var` marker is exactly WasmGC's per-field
                // mutability flag. Immutable fields let the engine hoist loads
                // without alias analysis.
                mutable: f.mutable,
            }));
            let super_ty = tagged.then_some(object_record);
            group.push(struct_subtype(fields, super_ty, !tagged));
        }

        // An enum becomes a base record holding just the tag, plus one subtype
        // per variant carrying its payload. A `match` reads the tag, and a
        // payload read casts to the variant it has already established.
        for i in 0..types.enum_count() {
            let eid = kite_hir::EnumId(i as u32);
            let base = layout.enum_base_type(eid);
            let tagged = layout.enum_shift(eid) == 1;
            let prefix: Vec<FieldType> = if tagged {
                vec![tag_field(), tag_field()]
            } else {
                vec![tag_field()]
            };
            group.push(struct_subtype(
                prefix.clone(),
                tagged.then_some(object_record),
                false,
            ));
            for variant in &types.enum_def(eid).variants {
                let mut fields = prefix.clone();
                for f in &variant.fields {
                    fields.push(FieldType {
                        element_type: StorageType::Val(val_type_with(f.ty, types, &layout)),
                        mutable: false,
                    });
                }
                group.push(struct_subtype(fields, Some(base), true));
            }
        }

        // One box per distinct optional payload. A `nil` is a null reference,
        // so no tag is needed and the payload keeps its own type.
        for p in &payloads {
            group.push(struct_subtype(
                vec![FieldType {
                    element_type: StorageType::Val(val_type_with(*p, types, &layout)),
                    mutable: false,
                }],
                None,
                true,
            ));
        }

        // One array per distinct element type. Kite slices are copy-on-write
        // values, so the array is mutable and a mutation copies first.
        for e in &elements {
            group.push(SubType {
                is_final: true,
                supertype_idx: None,
                composite_type: CompositeType {
                    inner: CompositeInnerType::Array(wasm_encoder::ArrayType(FieldType {
                        element_type: StorageType::Val(val_type_with(*e, types, &layout)),
                        mutable: true,
                    })),
                    shared: false,
                    descriptor: None,
                    describes: None,
                },
            });
        }

        // The error record holds a message index. `nil` is a null reference.
        group.push(struct_subtype(
            vec![FieldType {
                element_type: StorageType::Val(ValType::I32),
                mutable: false,
            }],
            None,
            true,
        ));

        // A fallible result is one GC object holding both slots, so a function
        // can return the pair without multi-value plumbing.
        let err_ref = ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(layout.error_record),
        });
        for v in &pairs {
            group.push(struct_subtype(
                vec![
                    FieldType {
                        element_type: StorageType::Val(val_type_with(*v, types, &layout)),
                        mutable: false,
                    },
                    FieldType { element_type: StorageType::Val(err_ref), mutable: false },
                ],
                None,
                true,
            ));
        }

        for s in &tuples {
            let TyKind::Tuple(elems) = types.kind(*s).clone() else {
                continue;
            };
            group.push(struct_subtype(
                elems
                    .iter()
                    .map(|e| FieldType {
                        element_type: StorageType::Val(val_type_with(*e, types, &layout)),
                        mutable: false,
                    })
                    .collect(),
                None,
                true,
            ));
        }

        for m in &maps {
            let Some(ml) = layout.map_layout(*m) else { continue };
            let karr = ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(ml.keys),
            });
            let varr = ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(ml.values),
            });
            group.push(struct_subtype(
                vec![
                    FieldType { element_type: StorageType::Val(karr), mutable: false },
                    FieldType { element_type: StorageType::Val(varr), mutable: false },
                ],
                None,
                true,
            ));
            for elem in [ml.key_ty, ml.value_ty] {
                group.push(SubType {
                    is_final: true,
                    supertype_idx: None,
                    composite_type: CompositeType {
                        inner: CompositeInnerType::Array(wasm_encoder::ArrayType(FieldType {
                            element_type: StorageType::Val(val_type_with(elem, types, &layout)),
                            mutable: true,
                        })),
                        shared: false,
                        descriptor: None,
                        describes: None,
                    },
                });
            }
        }

        // A closure value: which code to run, and the environment to run it
        // with. Both fields are immutable — a closure is a value, and rebinding
        // one replaces it rather than editing it.
        for ty in &fn_types {
            let sig = layout.closure_sig[ty];
            group.push(struct_subtype(
                vec![
                    FieldType {
                        element_type: StorageType::Val(ValType::Ref(RefType {
                            nullable: true,
                            heap_type: HeapType::Concrete(sig),
                        })),
                        mutable: false,
                    },
                    FieldType { element_type: StorageType::Val(ANY_REF), mutable: false },
                ],
                None,
                true,
            ));
        }

        // One environment record per lifted function, holding what it captured.
        for (func, (_, count)) in envs.iter().map(|(f, c)| (f, (0u32, *c))) {
            let lifted = &program.fns[*func as usize];
            let fields: Vec<FieldType> = lifted.locals[..count]
                .iter()
                .map(|l| FieldType {
                    element_type: StorageType::Val(val_type_with(l.ty, types, &layout)),
                    mutable: false,
                })
                .collect();
            group.push(struct_subtype(fields, None, true));
        }

        type_section.ty().rec(group);
    }

    // Indices are computed rather than read back: `TypeSection::len` counts a
    // `rec` group as one entry, not as the types inside it, so trusting it here
    // would collide function types with every struct after the first.
    // Signature types come between the imports and the aggregate group, so a
    // function type index is past both.
    let fn_type_base = IMPORT_COUNT + aggregate_count;
    // Dispatchers: one per trait method, taking the receiver as a reference to
    // the tagged root. They live above the user functions in the index space.
    let mut dispatchers: Vec<Dispatcher> = Vec::new();
    for v in &program.vtables {
        let def = types.trait_def(v.trait_id);
        for (m, method) in def.methods.iter().enumerate() {
            let mut params = vec![ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(object_record),
            })];
            params.extend(method.params.iter().map(|p| val_type_with(*p, types, &layout)));
            dispatchers.push(Dispatcher {
                trait_id: v.trait_id,
                method: m as u32,
                params,
                ret: method.ret,
                arms: v
                    .entries
                    .iter()
                    .map(|e| (e.tag, e.methods[m]))
                    .collect(),
            });
        }
    }
    // Which host functions the module declares decides where its own functions
    // start, so this has to be settled before any index is handed out.
    let eq_fns = eq::collect(program, types);
    let hosts = used_imports(program, types, &eq_fns);
    let fn_base = hosts.base;
    let dispatch_base = fn_base + program.fns.len() as u32;

    // Structural equality: one generated function per aggregate type a program
    // actually compares. They may call each other, so all are declared before
    // any is emitted.
    let eq_base = dispatch_base + dispatchers.len() as u32;

    // One thunk per lifted function: it takes the environment as an `anyref`,
    // casts it back to that function's own record, and calls the lifted
    // function with the captures unpacked ahead of the arguments. This is what
    // lets every closure of one Kite type share a single call signature while
    // capturing different things.
    let thunk_base = eq_base + eq_fns.len() as u32;
    let thunk_fns: Vec<u32> = envs.iter().map(|(f, _)| *f).collect();

    let mut fn_type_index = Vec::with_capacity(program.fns.len());
    for (i, f) in program.fns.iter().enumerate() {
        let params: Vec<ValType> = (0..f.param_count)
            .map(|j| val_type_with(f.locals[j].ty, types, &layout))
            .collect();
        let results: Vec<ValType> = wasm_result_with(f.ret, types, &layout).into_iter().collect();
        fn_type_index.push(fn_type_base + i as u32);
        type_section.ty().function(params, results);
    }
    let mut extra_type_index = Vec::with_capacity(dispatchers.len() + eq_fns.len());
    let mut next_fn_type = fn_type_base + program.fns.len() as u32;
    for d in &dispatchers {
        extra_type_index.push(next_fn_type);
        next_fn_type += 1;
        let results: Vec<ValType> = wasm_result_with(d.ret, types, &layout).into_iter().collect();
        type_section.ty().function(d.params.iter().copied(), results);
    }
    for e in &eq_fns {
        extra_type_index.push(next_fn_type);
        next_fn_type += 1;
        let (params, results) = eq::signature(e.ty, types, &layout);
        type_section.ty().function(params, results);
    }
    // A thunk is declared with the shared signature of the type it serves,
    // which already exists — no new function type is needed.
    for (func, _) in &envs {
        let ty = closure_ty_of(*func, program, types);
        extra_type_index.push(layout.closure_sig[&ty]);
    }

    // The poll trampoline, for a module with tasks in it. The host holds a
    // task's resume closure as an opaque reference and cannot call it — a
    // closure is a GC struct, and JavaScript has no way to enter one — so the
    // module exports the one function that can: cast it back and `call_ref`.
    let poll_ty = types.find_fn(&[], TyId::BOOL);
    let poll_trampoline = hosts.declared(host::TASK_SPAWN);
    let trampoline_index = fn_base
        + program.fns.len() as u32
        + dispatchers.len() as u32
        + eq_fns.len() as u32
        + envs.len() as u32;
    if poll_trampoline {
        extra_type_index.push(next_fn_type);
        next_fn_type += 1;
        type_section
            .ty()
            .function(vec![ANY_REF], vec![ValType::I32]);
    }
    let _ = next_fn_type;
    module.section(&type_section);

    // ---- imports -----------------------------------------------------------
    // Only what the module reaches for. Every declared import costs bytes in
    // the module and a function the host must supply, and the set has grown as
    // the language has — a `hello world` should not carry string slicing or a
    // font metric it never asks about.
    let mut imports = ImportSection::new();
    for (i, (name, _, _)) in IMPORTS.iter().enumerate() {
        if hosts.declared(i as u32) {
            imports.import("kite", name, EntityType::Function(i as u32));
        }
    }
    module.section(&imports);

    // ---- functions ---------------------------------------------------------
    let mut functions = FunctionSection::new();
    for idx in fn_type_index.iter().chain(&extra_type_index) {
        functions.function(*idx);
    }
    module.section(&functions);

    // ---- exports -----------------------------------------------------------
    // `main`, plus every `pub` free function — those are what a host can call,
    // and their names are unique because Kite has no overloading. Methods are
    // not exported: two types may each have an `area`, and a module may not
    // export one name twice.
    let mut exports = ExportSection::new();
    let mut exported: std::collections::HashSet<&str> = std::collections::HashSet::new();
    if let Some(entry) = program.entry {
        exports.export("main", ExportKind::Func, fn_base + entry.0);
        exported.insert("main");
    }
    for (i, f) in program.fns.iter().enumerate() {
        if f.exportable && exported.insert(f.name.as_str()) {
            exports.export(&f.name, ExportKind::Func, fn_base + i as u32);
        }
    }
    if poll_trampoline {
        exports.export("kite_poll", ExportKind::Func, trampoline_index);
    }
    module.section(&exports);

    // ---- elements ----------------------------------------------------------
    // `ref.func` may only name a function that has been declared for it. The
    // segment is declarative: it reserves nothing and initialises no table, it
    // only says which functions a reference may be taken to.
    if !envs.is_empty() {
        let mut elements = ElementSection::new();
        let indices: Vec<u32> = (0..envs.len() as u32).map(|i| thunk_base + i).collect();
        elements.declared(Elements::Functions(indices.as_slice().into()));
        module.section(&elements);
    }

    // ---- code --------------------------------------------------------------
    // A call's result type decides whether the value has to be stored, so the
    // whole table is needed before any body is emitted.
    let fn_returns: Vec<TyId> = program.fns.iter().map(|f| f.ret).collect();
    let mut code = CodeSection::new();
    let eq_builder =
        eq::EqBuilder { types, layout: &layout, base: eq_base, fns: &eq_fns, hosts: &hosts };
    for f in &program.fns {
        code.function(&compile_fn(
            f,
            types,
            &fn_returns,
            &layout,
            dispatch_base,
            &dispatchers,
            &eq_builder,
            program,
            &thunk_fns,
            thunk_base,
            fn_base,
            &hosts,
        ));
    }
    for d in &dispatchers {
        code.function(&compile_dispatcher(d, &layout, fn_base));
    }
    for e in &eq_fns {
        code.function(&eq_builder.build(e.ty));
    }
    for (func, count) in &envs {
        code.function(&compile_thunk(*func, *count, program, types, &layout, fn_base));
    }
    if poll_trampoline {
        code.function(&compile_poll_trampoline(poll_ty, &layout));
    }
    module.section(&code);

    WasmModule {
        bytes: module.finish(),
        strings: program.strings.clone(),
    }
}

/// A function type, for the shared signature closures are called through.
fn func_subtype(params: Vec<ValType>, results: Vec<ValType>) -> SubType {
    SubType {
        is_final: true,
        supertype_idx: None,
        composite_type: CompositeType {
            inner: CompositeInnerType::Func(wasm_encoder::FuncType::new(params, results)),
            shared: false,
            descriptor: None,
            describes: None,
        },
    }
}

/// An immutable `i32` field: a variant tag or a type identity.
fn tag_field() -> FieldType {
    FieldType {
        element_type: StorageType::Val(ValType::I32),
        mutable: false,
    }
}

fn struct_subtype(
    fields: Vec<FieldType>,
    supertype: Option<u32>,
    is_final: bool,
) -> SubType {
    SubType {
        is_final,
        supertype_idx: supertype,
        composite_type: CompositeType {
            inner: CompositeInnerType::Struct(wasm_encoder::StructType {
                fields: fields.into_boxed_slice(),
            }),
            shared: false,
            descriptor: None,
            describes: None,
        },
    }
}

/// The Wasm value type for a Kite type.
fn val_type_with(ty: TyId, types: &Types, layout: &TypeLayout) -> ValType {
    match types.kind(ty) {
        TyKind::Float => ValType::F64,
        TyKind::Int => ValType::I64,
        // A struct is a GC reference, traced by the host engine. Kite ships no
        // collector of its own, which is the whole reason a `hello world` is
        // hundreds of bytes rather than hundreds of kilobytes.
        TyKind::Struct(s) => ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(layout.struct_type(*s)),
        }),
        // Any variant is a subtype of the base record, so one reference to it
        // describes every value of the enum.
        TyKind::Enum(e) => ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(layout.enum_base_type(*e)),
        }),
        TyKind::Map(..) => match layout.map_layout(ty) {
            Some(ml) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(ml.record),
            }),
            None => ValType::I32,
        },
        TyKind::Tuple(_) => match layout.tuple_type(ty) {
            Some(idx) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(idx),
            }),
            None => ValType::I32,
        },
        TyKind::Err => ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(layout.error_record),
        }),
        // A function value is a closure record: code plus environment.
        TyKind::Fn { .. } => match layout.closure_type(ty) {
            Some(idx) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(idx),
            }),
            None => ValType::I32,
        },
        // A trait object is a reference to the tagged root. The value is
        // unchanged from its concrete form; only the static type widens, and
        // WasmGC subtyping makes that free.
        TyKind::Dyn(_) => ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(layout.object_record),
        }),
        TyKind::Fallible(v) => match layout.pair_type(*v) {
            Some(idx) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(idx),
            }),
            None => ValType::I32,
        },
        TyKind::Slice(elem) => match layout.slice_type(*elem) {
            Some(idx) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(idx),
            }),
            None => ValType::I32,
        },
        TyKind::Optional(inner) => match layout.option_type(*inner) {
            Some(idx) => ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(idx),
            }),
            // Only reachable while the layout is still being built.
            None => ValType::I32,
        },
        // `str` is a constant index for now; with JS String Builtins this
        // becomes `externref` carrying the JS string with no copy.
        _ => ValType::I32,
    }
}

/// The Kite function type a lifted function is called through: its parameters
/// past the captures, and its return type.
fn closure_ty_of(func: u32, program: &mir::Program, types: &Types) -> TyId {
    let lifted = &program.fns[func as usize];
    let count = program
        .fns
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.stmts.iter())
        .find_map(|s| match s {
            mir::Inst::Assign { value: mir::Rvalue::ClosureNew { func: g, captures }, .. }
                if g.0 == func =>
            {
                Some(captures.len())
            }
            _ => None,
        })
        .unwrap_or(0);
    let params: Vec<TyId> = lifted.locals[count..lifted.param_count].iter().map(|l| l.ty).collect();
    // `types` is shared and immutable here, so the type must already exist —
    // and it does: the local holding the closure has it.
    types.find_fn(&params, lifted.ret).unwrap_or(TyId::ERROR)
}

/// The wrapper that gives every closure of one Kite type the same signature.
fn compile_thunk(
    func: u32,
    count: usize,
    program: &mir::Program,
    types: &Types,
    layout: &TypeLayout,
    fn_base: u32,
) -> Function {
    let mut f = Function::new(Vec::new());
    let lifted = &program.fns[func as usize];
    let (env_record, _) = layout.env_record[&func];

    // The captures, read back out of the environment.
    for i in 0..count {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(env_record)));
        f.instruction(&Instruction::StructGet {
            struct_type_index: env_record,
            field_index: i as u32,
        });
    }
    // Then the arguments, which follow the environment parameter.
    for i in 0..(lifted.param_count - count) {
        f.instruction(&Instruction::LocalGet(1 + i as u32));
    }
    f.instruction(&Instruction::Call(fn_base + func));
    f.instruction(&Instruction::End);
    let _ = types;
    f
}

/// `kite_poll(task) -> i32`: run one step of a task and say whether it is done.
///
/// The host's scheduler holds resume closures it cannot enter. This casts one
/// back to the record a `fn() -> bool` closure is, takes the environment and
/// the code out of it, and calls through — which is exactly what a
/// `CallClosure` compiles to, written once for the host to reach.
fn compile_poll_trampoline(poll_ty: Option<TyId>, layout: &TypeLayout) -> Function {
    let Some((record, sig)) = poll_ty.and_then(|ty| {
        Some((layout.closure_type(ty)?, layout.closure_sig.get(&ty).copied()?))
    }) else {
        // No `fn() -> bool` closure exists, so nothing could have been
        // spawned. A trap here is unreachable rather than a fallback.
        let mut f = Function::new(Vec::new());
        f.instruction(&Instruction::Unreachable);
        f.instruction(&Instruction::End);
        return f;
    };
    let mut f = Function::new(vec![(
        1,
        ValType::Ref(RefType { nullable: true, heap_type: HeapType::Concrete(record) }),
    )]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(record)));
    f.instruction(&Instruction::LocalSet(1));
    // Environment, then the code to run: the order `call_ref` reads them in.
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::StructGet { struct_type_index: record, field_index: 1 });
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::StructGet { struct_type_index: record, field_index: 0 });
    f.instruction(&Instruction::CallRef(sig));
    f.instruction(&Instruction::End);
    f
}

/// A trait method's dispatcher: the receiver's stored tag chooses which
/// implementation runs.
struct Dispatcher {
    trait_id: kite_hir::TraitId,
    method: u32,
    params: Vec<ValType>,
    ret: TyId,
    /// `(concrete type tag, the function implementing this method for it)`.
    arms: Vec<(kite_hir::TypeTag, kite_hir::FnId)>,
}

/// A chain of tag comparisons. A `br_table` would be denser, but tags are not
/// contiguous — they encode a struct or enum id — and a trait rarely has more
/// than a handful of implementers, so the comparison chain is both smaller and
/// easier to read in a disassembly.
fn compile_dispatcher(d: &Dispatcher, layout: &TypeLayout, fn_base: u32) -> Function {
    let mut func = Function::new(Vec::new());
    for (tag, callee) in &d.arms {
        func.instruction(&Instruction::LocalGet(0));
        func.instruction(&Instruction::StructGet {
            struct_type_index: layout.object_record,
            field_index: 0,
        });
        func.instruction(&Instruction::I32Const(tag.encode() as i32));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::If(BlockType::Empty));
        // The tag has just been checked, so the cast cannot fail.
        func.instruction(&Instruction::LocalGet(0));
        let concrete = match tag {
            kite_hir::TypeTag::Struct(s) => layout.struct_type(*s),
            kite_hir::TypeTag::Enum(e) => layout.enum_base_type(*e),
        };
        func.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(concrete)));
        for i in 1..d.params.len() {
            func.instruction(&Instruction::LocalGet(i as u32));
        }
        func.instruction(&Instruction::Call(fn_base + callee.0));
        func.instruction(&Instruction::Return);
        func.instruction(&Instruction::End);
    }
    // No arm matched: the checker proved the receiver implements the trait, so
    // reaching here is a compiler bug rather than a program one.
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);
    func
}

/// The result type of a function, or `None` for unit.
fn wasm_result_with(ty: TyId, types: &Types, layout: &TypeLayout) -> Option<ValType> {
    (ty != TyId::UNIT).then(|| val_type_with(ty, types, layout))
}

fn compile_fn(
    f: &mir::Function,
    types: &Types,
    fn_returns: &[TyId],
    layout: &TypeLayout,
    dispatch_base: u32,
    dispatchers: &[Dispatcher],
    eq: &eq::EqBuilder,
    program: &mir::Program,
    thunks: &[u32],
    thunk_base: u32,
    fn_base: u32,
    hosts: &Hosts,
) -> Function {
    // Locals beyond the parameters, plus one synthetic program counter.
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for l in f.locals.iter().skip(f.param_count) {
        push_local(&mut locals, val_type_with(l.ty, types, layout));
    }
    push_local(&mut locals, ValType::I32); // $pc
    let pc = f.locals.len() as u32;

    // One scratch local per distinct slice type in the function, so a
    // copy-on-write mutation has somewhere to hold the new array while
    // `array.copy` consumes its operands. One register per distinct slice
    // shape: a function handling `[int]` and `[str]` needs one of each, and a
    // single shared register was only ever right while a function used one.
    let scratch = pc + 1;
    push_local(&mut locals, ValType::I32);

    // A dedicated index register. This used to alias `$pc`, which happened to
    // work because a terminator always rewrites the program counter before
    // branching — but a scan clobbering it is not something to rely on.
    push_local(&mut locals, ValType::I32);
    let index_scratch = scratch + 1;

    // Two array registers per distinct map shape, so a map write can hold the
    // arrays it is building while `array.copy` consumes its operands.
    push_local(&mut locals, ValType::I32);
    let mut map_scratch: std::collections::HashMap<TyId, (u32, u32)> =
        std::collections::HashMap::new();
    let mut next_local = index_scratch + 2;

    let mut slice_scratch: std::collections::HashMap<TyId, u32> =
        std::collections::HashMap::new();
    let mut slice_shapes: Vec<TyId> = Vec::new();
    for l in &f.locals {
        if matches!(types.kind(l.ty), TyKind::Slice(_)) && !slice_shapes.contains(&l.ty) {
            slice_shapes.push(l.ty);
        }
    }
    for shape in &slice_shapes {
        push_local(&mut locals, val_type_with(*shape, types, layout));
        let TyKind::Slice(elem) = *types.kind(*shape) else { continue };
        if let Some(idx) = layout.slice_type(elem) {
            slice_scratch.insert(TyId(idx), next_local);
        }
        next_local += 1;
    }

    let mut shapes: Vec<TyId> = Vec::new();
    for l in &f.locals {
        if matches!(types.kind(l.ty), TyKind::Map(..)) && !shapes.contains(&l.ty) {
            shapes.push(l.ty);
        }
    }
    for shape in &shapes {
        let Some(ml) = layout.map_layout(*shape) else { continue };
        push_local(
            &mut locals,
            ValType::Ref(RefType { nullable: true, heap_type: HeapType::Concrete(ml.keys) }),
        );
        push_local(
            &mut locals,
            ValType::Ref(RefType { nullable: true, heap_type: HeapType::Concrete(ml.values) }),
        );
        map_scratch.insert(*shape, (next_local, next_local + 1));
        next_local += 2;
    }

    let mut func = Function::new(locals);
    let n = f.blocks.len() as u32;

    // Start at block 0.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pc));

    func.instruction(&Instruction::Block(BlockType::Empty)); // $exit
    func.instruction(&Instruction::Loop(BlockType::Empty)); // $dispatch

    // One block per basic block, innermost first, so `br_table` can select any
    // of them by depth.
    for _ in 0..n {
        func.instruction(&Instruction::Block(BlockType::Empty));
    }
    func.instruction(&Instruction::LocalGet(pc));
    let targets: Vec<u32> = (0..n).collect();
    func.instruction(&Instruction::BrTable(targets.into(), n));

    for (i, block) in f.blocks.iter().enumerate() {
        // Close the block whose body follows.
        func.instruction(&Instruction::End);

        let mut e = Emitter {
            f,
            types,
            fn_returns,
            dispatch_base,
            dispatchers,
            eq,
            program,
            thunks,
            thunk_base,
            fn_base,
            hosts,
            layout,
            current_dst: None,
            pc,
            scratch,
            index_scratch,
            map_scratch: &map_scratch,
            slice_scratch: &slice_scratch,
            block_index: i,
            total: n as usize,
        };
        e.block(&mut func, block);
    }

    func.instruction(&Instruction::End); // loop
    func.instruction(&Instruction::End); // block $exit
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End); // function
    func
}

fn push_local(locals: &mut Vec<(u32, ValType)>, ty: ValType) {
    match locals.last_mut() {
        Some((count, t)) if *t == ty => *count += 1,
        _ => locals.push((1, ty)),
    }
}

struct Emitter<'a> {
    f: &'a mir::Function,
    types: &'a Types,
    /// Return type of every function, indexed by id.
    fn_returns: &'a [TyId],
    /// Where the dispatchers start in the function index space.
    dispatch_base: u32,
    dispatchers: &'a [Dispatcher],
    eq: &'a eq::EqBuilder<'a>,
    /// The whole program, for reading a lifted function's signature back.
    program: &'a mir::Program,
    /// Lifted functions with a thunk, in emission order.
    thunks: &'a [u32],
    thunk_base: u32,
    /// Where the module's own functions start, past its imports.
    fn_base: u32,
    hosts: &'a Hosts,
    layout: &'a TypeLayout,
    /// The local a rvalue is being assigned into, when there is one. A slice
    /// construction takes its element type from there.
    current_dst: Option<u32>,
    pc: u32,
    /// A local of array type, used to hold a copy while `array.copy` consumes
    /// its operands.
    scratch: u32,
    /// An i32 local for an index that has to be read twice.
    index_scratch: u32,
    /// Per map shape, the two array registers a write builds into.
    map_scratch: &'a std::collections::HashMap<TyId, (u32, u32)>,
    /// One register per distinct array type, keyed by that type's index in the
    /// Wasm type space rather than by Kite type — two Kite slices with the same
    /// element share an array type and may share the register.
    slice_scratch: &'a std::collections::HashMap<TyId, u32>,
    block_index: usize,
    total: usize,
}

impl<'a> Emitter<'a> {
    /// Branch depth from this block's body out to the dispatch loop.
    ///
    /// At the body of block `i` the blocks `0..=i` are closed, so those still
    /// open are `i+1..n` — that many, and then the loop itself.
    fn dispatch_depth(&self) -> u32 {
        (self.total - 1 - self.block_index) as u32
    }

    fn block(&mut self, func: &mut Function, block: &mir::BasicBlock) {
        for stmt in &block.stmts {
            self.stmt(func, stmt);
        }
        self.current_dst = None;
        self.terminator(func, &block.term);
    }

    fn stmt(&mut self, func: &mut Function, stmt: &mir::Inst) {
        match stmt {
            mir::Inst::Assign { dst, value } => {
                self.current_dst = Some(dst.0);
                // MIR wraps a statement-position call in an assignment so the
                // call still happens. A unit-returning call leaves nothing on
                // the stack, so there is nothing to store.
                if self.rvalue(func, value) {
                    func.instruction(&Instruction::LocalSet(dst.0));
                }
            }
            // Aggregates are not lowered yet. The driver refuses these programs
            // before codegen runs, so this is defensive.
            mir::Inst::SetField { base, index, value } => {
                let Some(sid) = self.struct_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                self.operand(func, base);
                self.operand(func, value);
                func.instruction(&Instruction::StructSet {
                    struct_type_index: self.layout.struct_type(sid),
                    field_index: *index + self.layout.struct_shift(sid),
                });
            }
            mir::Inst::MapSet { local, key, value } => {
                let base = mir::Operand::Local(*local);
                let map_ty = self.f.locals[local.index()].ty;
                let (Some(ml), Some(&(kreg, vreg))) =
                    (self.map_of(&base), self.map_scratch.get(&map_ty))
                else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                self.map_write(func, ml, &base, key, value, local.0, kreg, vreg);
            }

            // Slices are copy-on-write *values*, so a mutation copies the
            // array first and rebinds the local. The bytecode VM does the same
            // thing lazily through `Rc::make_mut`; here it is unconditional,
            // which is correct but not yet cheap.
            mir::Inst::SetIndex { base, index, value } => {
                let Some((idx, _)) = self.slice_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                let Some(local) = self.local_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                self.copy_array(func, idx, base, 0);
                func.instruction(&Instruction::LocalSet(local));

                func.instruction(&Instruction::LocalGet(local));
                self.index_operand(func, index);
                self.operand(func, value);
                func.instruction(&Instruction::ArraySet(idx));
            }

            mir::Inst::SlicePush { local, value } => {
                let base = mir::Operand::Local(*local);
                let Some((idx, _)) = self.slice_of(&base) else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                // One longer, contents copied, new element last.
                self.copy_array(func, idx, &base, 1);
                func.instruction(&Instruction::LocalSet(local.0));

                func.instruction(&Instruction::LocalGet(local.0));
                func.instruction(&Instruction::LocalGet(local.0));
                func.instruction(&Instruction::ArrayLen);
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub);
                self.operand(func, value);
                func.instruction(&Instruction::ArraySet(idx));
            }
        }
    }

    /// Emit `value`, returning whether it left a result on the stack.
    fn rvalue(&mut self, func: &mut Function, value: &mir::Rvalue) -> bool {
        match value {
            mir::Rvalue::Use(o) => {
                self.operand(func, o);
                return true;
            }

            mir::Rvalue::Binary { op, lhs, rhs } => {
                self.operand(func, lhs);
                self.operand(func, rhs);
                // A `str` is a table index, so its operations are host calls
                // rather than instructions.
                match op {
                    BinOp::ConcatStr => {
                        func.instruction(&Instruction::Call(self.hosts.at(host::STR_CONCAT)));
                    }
                    BinOp::EqStr => {
                        func.instruction(&Instruction::Call(self.hosts.at(host::STR_EQ)));
                    }
                    BinOp::NeStr => {
                        func.instruction(&Instruction::Call(self.hosts.at(host::STR_EQ)));
                        func.instruction(&Instruction::I32Eqz);
                    }
                    // Deep equality on an aggregate: a generated function per
                    // type, because Wasm has no instruction for it.
                    BinOp::EqValue | BinOp::NeValue => {
                        match self.operand_ty(lhs).and_then(|t| self.eq.index_of(t)) {
                            Some(i) => func.instruction(&Instruction::Call(i)),
                            None => func.instruction(&Instruction::Unreachable),
                        };
                        if matches!(op, BinOp::NeValue) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                    }
                    _ => self.binop(func, *op),
                }
                return true;
            }

            mir::Rvalue::Unary { op, operand } => match op {
                UnOp::NegInt => {
                    func.instruction(&Instruction::I64Const(0));
                    self.operand(func, operand);
                    func.instruction(&Instruction::I64Sub);
                }
                UnOp::NegFloat => {
                    self.operand(func, operand);
                    func.instruction(&Instruction::F64Neg);
                }
                UnOp::Not => {
                    self.operand(func, operand);
                    func.instruction(&Instruction::I32Eqz);
                }
            },

            mir::Rvalue::Call { callee, args } => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.fn_base + callee.0));
                return self.fn_returns[callee.index()] != TyId::UNIT;
            }

            mir::Rvalue::CallVirtual { trait_id, method, args } => {
                for a in args {
                    self.operand(func, a);
                }
                let i = self
                    .dispatchers
                    .iter()
                    .position(|d| d.trait_id == *trait_id && d.method == *method);
                match i {
                    Some(i) => {
                        func.instruction(&Instruction::Call(self.dispatch_base + i as u32));
                        return self.dispatchers[i].ret != TyId::UNIT;
                    }
                    // A trait with no implementers has no dispatcher; the
                    // checker has already reported the call site.
                    None => {
                        func.instruction(&Instruction::Unreachable);
                        return false;
                    }
                }
            }

            mir::Rvalue::ClosureNew { func: callee, captures } => {
                let record = self
                    .closure_record_for(callee.0)
                    .unwrap_or(self.layout.object_record);
                let Some((env_record, _)) = self.layout.env_record.get(&callee.0).copied() else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                let Some(thunk) = self.thunk_of(callee.0) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                func.instruction(&Instruction::RefFunc(thunk));
                for c in captures {
                    self.operand(func, c);
                }
                func.instruction(&Instruction::StructNew(env_record));
                func.instruction(&Instruction::StructNew(record));
                return true;
            }

            mir::Rvalue::CallClosure { callee, args } => {
                let Some(ty) = self.operand_ty(callee) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                let (Some(record), Some(sig)) =
                    (self.layout.closure_type(ty), self.layout.closure_sig.get(&ty).copied())
                else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                // Environment, then arguments, then the code to run — which is
                // the order `call_ref` reads them in.
                self.operand(func, callee);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: record,
                    field_index: 1,
                });
                for a in args {
                    self.operand(func, a);
                }
                self.operand(func, callee);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: record,
                    field_index: 0,
                });
                func.instruction(&Instruction::CallRef(sig));
                let TyKind::Fn { ret, .. } = self.types.kind(ty) else {
                    return true;
                };
                return *ret != TyId::UNIT;
            }

            mir::Rvalue::Cast { operand, from, to } => {
                self.operand(func, operand);
                if from == to {
                    return true;
                }
                // Saturating rather than trapping, so a value out of range or
                // a NaN gives a number rather than killing the program — the
                // bytecode VM does the same, and they have to agree.
                func.instruction(&if *to == TyId::FLOAT {
                    Instruction::F64ConvertI64S
                } else {
                    Instruction::I64TruncSatF64S
                });
                return true;
            }

            mir::Rvalue::StrOp { op, args } => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(match op {
                    kite_hir::StrKind::Len => host::STR_LEN,
                    kite_hir::StrKind::Slice => host::STR_SLICE,
                    kite_hir::StrKind::IndexOf => host::STR_INDEX_OF,
                    kite_hir::StrKind::Trim => host::STR_TRIM,
                })));
                return true;
            }

            mir::Rvalue::ToStr { operand, from } => {
                self.operand(func, operand);
                let call = match self.types.kind(*from) {
                    TyKind::Int => host::STR_OF_INT,
                    TyKind::Float => host::STR_OF_FLOAT,
                    TyKind::Bool => host::STR_OF_BOOL,
                    // A `str` renders as itself; the checker emits no node for
                    // that, so anything else here is a compiler bug.
                    _ => {
                        func.instruction(&Instruction::Unreachable);
                        return true;
                    }
                };
                func.instruction(&Instruction::Call(self.hosts.at(call)));
                return true;
            }

            mir::Rvalue::CallBuiltin { builtin, args } => {
                self.builtin(func, *builtin, args);
                // Drawing returns nothing; measuring returns a width, and the
                // clock returns a reading.
                return matches!(
                    builtin,
                    Builtin::TextWidth | Builtin::TextHeight | Builtin::TimeNow
                );
            }

            mir::Rvalue::StructNew { struct_id, fields } => {
                // A dispatchable type stores its identity in field 0, because
                // WasmGC's structural typing cannot recover it from the value.
                let tag = kite_hir::TypeTag::Struct(*struct_id);
                if self.layout.shift(tag) == 1 {
                    func.instruction(&Instruction::I32Const(tag.encode() as i32));
                }
                for f in fields {
                    self.operand(func, f);
                }
                func.instruction(&Instruction::StructNew(self.layout.struct_type(*struct_id)));
                return true;
            }

            mir::Rvalue::FieldGet { base, index } => {
                let Some((record, shift)) = self.record_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: record,
                    field_index: *index + shift,
                });
                return true;
            }

            mir::Rvalue::MapNew { entries } => {
                let Some(ml) = self
                    .current_dst
                    .and_then(|d| self.layout.map_layout(self.f.locals[d as usize].ty))
                else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                // Entries arrive flattened as key, value, key, value.
                let pairs = entries.len() / 2;
                for e in entries.iter().step_by(2) {
                    self.operand(func, e);
                }
                func.instruction(&Instruction::ArrayNewFixed {
                    array_type_index: ml.keys,
                    array_size: pairs as u32,
                });
                for e in entries.iter().skip(1).step_by(2) {
                    self.operand(func, e);
                }
                func.instruction(&Instruction::ArrayNewFixed {
                    array_type_index: ml.values,
                    array_size: pairs as u32,
                });
                func.instruction(&Instruction::StructNew(ml.record));
                return true;
            }

            mir::Rvalue::MapLen { base } => {
                let Some(ml) = self.map_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: ml.record,
                    field_index: 0,
                });
                func.instruction(&Instruction::ArrayLen);
                func.instruction(&Instruction::I64ExtendI32U);
                return true;
            }

            // Lookup is a linear scan over the key array. A hash index is an
            // optimisation for later; a scan is what makes the semantics —
            // insertion order, and first match wins — obviously right.
            mir::Rvalue::MapGet { base, key } => {
                let (Some(ml), Some(box_idx)) = (
                    self.map_of(base),
                    self.map_of(base)
                        .and_then(|m| self.layout.option_type(m.value_ty)),
                ) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.map_scan(func, ml, base, key, box_idx);
                return true;
            }

            mir::Rvalue::TupleNew { elems } => {
                let Some(idx) = self
                    .current_dst
                    .and_then(|d| self.layout.tuple_type(self.f.locals[d as usize].ty))
                else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                for e in elems {
                    self.operand(func, e);
                }
                func.instruction(&Instruction::StructNew(idx));
                return true;
            }

            mir::Rvalue::ErrorNew { message } => {
                self.operand(func, message);
                func.instruction(&Instruction::StructNew(self.layout.error_record));
                return true;
            }

            mir::Rvalue::ErrorMessage { base } => {
                self.operand(func, base);
                func.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(
                    self.layout.error_record,
                )));
                func.instruction(&Instruction::StructGet {
                    struct_type_index: self.layout.error_record,
                    field_index: 0,
                });
                return true;
            }

            mir::Rvalue::PairNew { value, error } => {
                let (Some(idx), Some(vty)) = (self.pair_for_result(), self.pair_value_ty()) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                // `return _, err` carries no value — that is the whole point of
                // the failure arm, and the taint analysis has already proved
                // nothing can read it. The record still needs bits in that
                // slot, so a default goes there and stays unobservable.
                match value {
                    mir::Operand::Nil | mir::Operand::Unit => self.default_of(func, vty),
                    other => self.operand(func, other),
                }
                self.operand(func, error);
                func.instruction(&Instruction::StructNew(idx));
                return true;
            }

            mir::Rvalue::PairValue { base } => {
                let Some(idx) = self.pair_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: idx,
                    field_index: 0,
                });
                return true;
            }

            mir::Rvalue::PairError { base } => {
                let Some(idx) = self.pair_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: idx,
                    field_index: 1,
                });
                return true;
            }

            mir::Rvalue::SliceNew { elems } => {
                let Some(idx) = self.slice_array_for_result() else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                for e in elems {
                    self.operand(func, e);
                }
                func.instruction(&Instruction::ArrayNewFixed {
                    array_type_index: idx,
                    array_size: elems.len() as u32,
                });
                return true;
            }

            // `array.get` traps when out of range, which is exactly Kite's
            // rule: an out-of-range index is a program bug, not a runtime
            // condition. `.get()` is the form for when it genuinely is one.
            mir::Rvalue::IndexGet { base, index } => {
                let Some((idx, _)) = self.slice_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                self.index_operand(func, index);
                func.instruction(&Instruction::ArrayGet(idx));
                return true;
            }

            // `.get()` is the form for when an out-of-range index genuinely is
            // a runtime condition, so it bounds-checks and yields an optional
            // rather than trapping.
            mir::Rvalue::SliceGet { base, index } => {
                let (Some((idx, elem)), Some(box_idx)) = (
                    self.slice_of(base),
                    self.slice_of(base)
                        .and_then(|(_, e)| self.layout.option_type(e)),
                ) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                let _ = elem;

                self.index_operand(func, index);
                func.instruction(&Instruction::LocalTee(self.index_scratch));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::LocalGet(self.index_scratch));
                self.operand(func, base);
                func.instruction(&Instruction::ArrayLen);
                func.instruction(&Instruction::I32LtU);
                func.instruction(&Instruction::I32And);

                let result = ValType::Ref(RefType {
                    nullable: true,
                    heap_type: HeapType::Concrete(box_idx),
                });
                func.instruction(&Instruction::If(BlockType::Result(result)));
                self.operand(func, base);
                func.instruction(&Instruction::LocalGet(self.index_scratch));
                func.instruction(&Instruction::ArrayGet(idx));
                func.instruction(&Instruction::StructNew(box_idx));
                func.instruction(&Instruction::Else);
                func.instruction(&Instruction::RefNull(HeapType::Abstract {
                    shared: false,
                    ty: wasm_encoder::AbstractHeapType::None,
                }));
                func.instruction(&Instruction::End);
                return true;
            }

            mir::Rvalue::SliceLen { base } => {
                self.operand(func, base);
                func.instruction(&Instruction::ArrayLen);
                func.instruction(&Instruction::I64ExtendI32U);
                return true;
            }

            mir::Rvalue::Wrap { value } => {
                let Some(idx) = self.option_box_for(value) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, value);
                func.instruction(&Instruction::StructNew(idx));
                return true;
            }

            // Narrowing has already proved the value is present, so the cast
            // is a formality the validator needs and the engine elides.
            mir::Rvalue::Unwrap { value } => {
                let Some((idx, _)) = self.optional_of(value) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, value);
                func.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(idx)));
                func.instruction(&Instruction::StructGet {
                    struct_type_index: idx,
                    field_index: 0,
                });
                return true;
            }

            mir::Rvalue::IsNil { value } => {
                self.operand(func, value);
                func.instruction(&Instruction::RefIsNull);
                return true;
            }

            mir::Rvalue::EnumNew { enum_id, variant, fields } => {
                // The variant tag, then the payload — behind the identity tag
                // when this enum is reachable through a trait object.
                let tag = kite_hir::TypeTag::Enum(*enum_id);
                if self.layout.shift(tag) == 1 {
                    func.instruction(&Instruction::I32Const(tag.encode() as i32));
                }
                func.instruction(&Instruction::I32Const(*variant as i32));
                for f in fields {
                    self.operand(func, f);
                }
                func.instruction(&Instruction::StructNew(
                    self.layout.variant_type(*enum_id, *variant),
                ));
                return true;
            }

            mir::Rvalue::TagOf { base } => {
                let Some(eid) = self.enum_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: self.layout.enum_base_type(eid),
                    field_index: self.layout.enum_shift(eid),
                });
                // The tag is an i32 in the record but an int in Kite.
                func.instruction(&Instruction::I64ExtendI32S);
                return true;
            }

            // The tag has already been tested, so the cast cannot fail.
            mir::Rvalue::VariantGet { base, enum_id, variant, index } => {
                self.operand(func, base);
                func.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(
                    self.layout.variant_type(*enum_id, *variant),
                )));
                func.instruction(&Instruction::StructGet {
                    struct_type_index: self.layout.variant_type(*enum_id, *variant),
                    // The variant tag precedes the payload.
                    field_index: index + 1 + self.layout.enum_shift(*enum_id),
                });
                return true;
            }

            // The state-machine transform replaced both of these before any
            // backend saw the program.
            mir::Rvalue::Await { .. } | mir::Rvalue::Yield => {
                unreachable!("`await` survived the state-machine transform")
            }

            // Every MIR rvalue is handled: there is deliberately no catch-all
            // here, so adding one to MIR fails to compile rather than silently
            // producing a module that traps.
        }
        true
    }

    fn builtin(&mut self, func: &mut Function, builtin: Builtin, args: &[mir::Operand]) {
        match builtin {
            Builtin::IoPrint => {
                let Some(arg) = args.first() else {
                    func.instruction(&Instruction::Unreachable);
                    return;
                };
                // The host function is chosen by the argument's type. The
                // import list is small on purpose; `Display` replaces it later.
                let import = if self.is_str(arg) {
                    host::PRINT_STR
                } else {
                    match self.operand_type(arg) {
                        ValType::I64 => host::PRINT_INT,
                        ValType::F64 => host::PRINT_FLOAT,
                        _ => host::PRINT_BOOL,
                    }
                };
                self.operand(func, arg);
                func.instruction(&Instruction::Call(self.hosts.at(import)));
            }

            // Both take their arguments in declaration order; the host reads
            // them positionally, which is why the checker fixes their types.
            Builtin::DrawRect => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::DRAW_RECT)));
            }
            Builtin::DrawText => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::DRAW_TEXT)));
            }
            Builtin::TextWidth => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::MEASURE_TEXT)));
            }
            Builtin::TextHeight => {
                func.instruction(&Instruction::Call(self.hosts.at(host::LINE_HEIGHT)));
            }
            Builtin::DrawClip => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::DRAW_CLIP)));
            }
            Builtin::TaskSpawn => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::TASK_SPAWN)));
            }
            Builtin::TaskWakeAt => {
                for a in args {
                    self.operand(func, a);
                }
                func.instruction(&Instruction::Call(self.hosts.at(host::TASK_WAKE_AT)));
            }
            Builtin::TaskPark => {
                func.instruction(&Instruction::Call(self.hosts.at(host::TASK_PARK)));
            }
            Builtin::TimeNow => {
                func.instruction(&Instruction::Call(self.hosts.at(host::TIME_NOW)));
            }
            Builtin::DrawUnclip => {
                func.instruction(&Instruction::Call(self.hosts.at(host::DRAW_UNCLIP)));
            }
        }
    }

    fn operand(&mut self, func: &mut Function, o: &mir::Operand) {
        match o {
            mir::Operand::Local(l) => {
                func.instruction(&Instruction::LocalGet(l.0));
            }
            mir::Operand::Int(v) => {
                func.instruction(&Instruction::I64Const(*v));
            }
            mir::Operand::Float(v) => {
                func.instruction(&Instruction::F64Const((*v).into()));
            }
            mir::Operand::Bool(v) => {
                func.instruction(&Instruction::I32Const(i32::from(*v)));
            }
            // A string is its index into the constant table the glue holds.
            mir::Operand::Str(s) => {
                func.instruction(&Instruction::I32Const(s.0 as i32));
            }
            mir::Operand::Unit => {
                func.instruction(&Instruction::I32Const(0));
            }
            // `ref.null none` is a subtype of every nullable internal
            // reference, so one instruction serves every optional type.
            mir::Operand::Nil => {
                func.instruction(&Instruction::RefNull(HeapType::Abstract {
                    shared: false,
                    ty: wasm_encoder::AbstractHeapType::None,
                }));
            }
            // A slot nothing may read still needs bits of the slot's own type.
            // The bytecode VM's registers carry their own tags and can use one
            // nil for all of them; here every type needs its own zero.
            mir::Operand::Default(ty) => self.default_of(func, *ty),
        }
    }

    /// Leave a fresh array on the stack holding `base`'s contents, `extra`
    /// elements longer.
    fn copy_array(&mut self, func: &mut Function, idx: u32, base: &mir::Operand, extra: u32) {
        // The destination, sized len + extra.
        self.operand(func, base);
        func.instruction(&Instruction::ArrayLen);
        if extra > 0 {
            func.instruction(&Instruction::I32Const(extra as i32));
            func.instruction(&Instruction::I32Add);
        }
        func.instruction(&Instruction::ArrayNewDefault(idx));

        // array.copy takes dest, dest_offset, src, src_offset, len — and the
        // destination has to survive the call, so it is held in a register of
        // its own array type rather than duplicated on the stack.
        let hold = self.slice_scratch.get(&TyId(idx)).copied().unwrap_or(self.scratch);
        func.instruction(&Instruction::LocalSet(hold));
        func.instruction(&Instruction::LocalGet(hold));
        func.instruction(&Instruction::I32Const(0));
        self.operand(func, base);
        func.instruction(&Instruction::I32Const(0));
        self.operand(func, base);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: idx,
            array_type_index_src: idx,
        });
        func.instruction(&Instruction::LocalGet(hold));
    }

    fn local_of(&self, o: &mir::Operand) -> Option<u32> {
        match o {
            mir::Operand::Local(l) => Some(l.0),
            _ => None,
        }
    }

    /// The value type of the pair currently being built.
    fn pair_value_ty(&self) -> Option<TyId> {
        let ty = match self.current_dst {
            Some(dst) => self.f.locals[dst as usize].ty,
            None => self.f.ret,
        };
        match *self.types.kind(ty) {
            TyKind::Fallible(v) => Some(v),
            _ => None,
        }
    }

    /// A value of `ty` that no program can observe.
    fn default_of(&mut self, func: &mut Function, ty: TyId) {
        match val_type_with(ty, self.types, self.layout) {
            ValType::I64 => func.instruction(&Instruction::I64Const(0)),
            ValType::F64 => func.instruction(&Instruction::F64Const(0.0.into())),
            ValType::Ref(_) => func.instruction(&Instruction::RefNull(HeapType::Abstract {
                shared: false,
                ty: wasm_encoder::AbstractHeapType::None,
            })),
            _ => func.instruction(&Instruction::I32Const(0)),
        };
    }

    /// `m[k] = v`.
    ///
    /// Maps are copy-on-write values, so this builds new arrays and rebinds the
    /// local rather than mutating in place. One code path covers both replacing
    /// an existing key and appending a new one: the scan yields the key's index
    /// or, when absent, the current length, and the new arrays are one longer
    /// only in the second case.
    #[allow(clippy::too_many_arguments)]
    fn map_write(
        &mut self,
        func: &mut Function,
        ml: MapLayout,
        base: &mir::Operand,
        key: &mir::Operand,
        value: &mir::Operand,
        dst: u32,
        kreg: u32,
        vreg: u32,
    ) {
        let pos = self.index_scratch;

        // Scan for the key, leaving `pos` at its index or at the length.
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(pos));
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::ArrayGet(ml.keys));
        self.operand(func, key);
        self.key_equality(func, ml.key_ty);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End); // loop
        func.instruction(&Instruction::End); // block

        // The new length: one longer only when the key was absent.
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32GtU);
        func.instruction(&Instruction::Select);
        func.instruction(&Instruction::LocalSet(self.index_scratch2(func)));

        // Keys.
        func.instruction(&Instruction::LocalGet(self.index_scratch2(func)));
        func.instruction(&Instruction::ArrayNewDefault(ml.keys));
        func.instruction(&Instruction::LocalSet(kreg));
        func.instruction(&Instruction::LocalGet(kreg));
        func.instruction(&Instruction::I32Const(0));
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::I32Const(0));
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: ml.keys,
            array_type_index_src: ml.keys,
        });
        func.instruction(&Instruction::LocalGet(kreg));
        func.instruction(&Instruction::LocalGet(pos));
        self.operand(func, key);
        func.instruction(&Instruction::ArraySet(ml.keys));

        // Values.
        func.instruction(&Instruction::LocalGet(self.index_scratch2(func)));
        func.instruction(&Instruction::ArrayNewDefault(ml.values));
        func.instruction(&Instruction::LocalSet(vreg));
        func.instruction(&Instruction::LocalGet(vreg));
        func.instruction(&Instruction::I32Const(0));
        self.map_field(func, ml, base, 1);
        func.instruction(&Instruction::I32Const(0));
        self.map_field(func, ml, base, 1);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: ml.values,
            array_type_index_src: ml.values,
        });
        func.instruction(&Instruction::LocalGet(vreg));
        func.instruction(&Instruction::LocalGet(pos));
        self.operand(func, value);
        func.instruction(&Instruction::ArraySet(ml.values));

        func.instruction(&Instruction::LocalGet(kreg));
        func.instruction(&Instruction::LocalGet(vreg));
        func.instruction(&Instruction::StructNew(ml.record));
        func.instruction(&Instruction::LocalSet(dst));
    }

    /// A second i32 register, for the new length.
    fn index_scratch2(&self, _func: &Function) -> u32 {
        self.index_scratch + 1
    }

    /// Emit a linear scan for `key`, leaving `Option<V>` on the stack.
    ///
    /// ```text
    /// block $end (result optref)
    ///   block $miss
    ///     loop $scan
    ///       i >= len  -> br $miss
    ///       keys[i] == key -> box(values[i]) ; br $end
    ///       i += 1 ; br $scan
    ///     end
    ///   end
    ///   ref.null none
    /// end
    /// ```
    fn map_scan(
        &mut self,
        func: &mut Function,
        ml: MapLayout,
        base: &mir::Operand,
        key: &mir::Operand,
        box_idx: u32,
    ) {
        let i = self.index_scratch;
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));

        let result = ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(box_idx),
        });
        func.instruction(&Instruction::Block(BlockType::Result(result)));
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));

        // Past the end?
        func.instruction(&Instruction::LocalGet(i));
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::ArrayLen);
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));

        // Key match?
        self.map_field(func, ml, base, 0);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::ArrayGet(ml.keys));
        self.operand(func, key);
        self.key_equality(func, ml.key_ty);

        func.instruction(&Instruction::If(BlockType::Empty));
        self.map_field(func, ml, base, 1);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::ArrayGet(ml.values));
        func.instruction(&Instruction::StructNew(box_idx));
        // 0 = if, 1 = loop, 2 = the miss block, 3 = the result block.
        func.instruction(&Instruction::Br(3));
        func.instruction(&Instruction::End);

        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));

        func.instruction(&Instruction::End); // loop
        func.instruction(&Instruction::End); // miss block

        func.instruction(&Instruction::RefNull(HeapType::Abstract {
            shared: false,
            ty: wasm_encoder::AbstractHeapType::None,
        }));
        func.instruction(&Instruction::End); // result block
    }

    fn map_field(&mut self, func: &mut Function, ml: MapLayout, base: &mir::Operand, field: u32) {
        self.operand(func, base);
        func.instruction(&Instruction::StructGet {
            struct_type_index: ml.record,
            field_index: field,
        });
    }

    /// Compare two keys. A `str` is a table index the glue interns, so equal
    /// strings share an index and an integer compare is exact.
    fn key_equality(&mut self, func: &mut Function, key_ty: TyId) {
        let inst = match val_type_with(key_ty, self.types, self.layout) {
            ValType::I64 => Instruction::I64Eq,
            ValType::F64 => Instruction::F64Eq,
            _ => Instruction::I32Eq,
        };
        func.instruction(&inst);
    }

    fn map_of(&self, o: &mir::Operand) -> Option<MapLayout> {
        let mir::Operand::Local(l) = o else { return None };
        self.layout.map_layout(self.f.locals[l.index()].ty)
    }

    /// The record type an operand holds, whether a struct or a tuple. Both are
    /// positional records once lowered.
    /// The closure record a lifted function's values are, and the index of its
    /// thunk. Both are keyed by the lifted function, because its captures fix
    /// the environment's shape.
    fn closure_record_for(&self, func: u32) -> Option<u32> {
        self.layout.closure_type(closure_ty_of(func, self.program, self.types))
    }

    fn thunk_of(&self, func: u32) -> Option<u32> {
        self.thunks.iter().position(|f| *f == func).map(|i| self.thunk_base + i as u32)
    }

    /// The type an operand holds, when it is a local. A literal never has an
    /// aggregate type, so this is all deep equality needs.
    fn operand_ty(&self, o: &mir::Operand) -> Option<TyId> {
        match o {
            mir::Operand::Local(l) => Some(self.f.locals[l.index()].ty),
            _ => None,
        }
    }

    /// The record an operand holds, and how far its fields are shifted by an
    /// identity tag. Tuples never dispatch, so they are never shifted.
    fn record_of(&self, o: &mir::Operand) -> Option<(u32, u32)> {
        let mir::Operand::Local(l) = o else { return None };
        let ty = self.f.locals[l.index()].ty;
        match self.types.kind(ty) {
            TyKind::Struct(s) => Some((self.layout.struct_type(*s), self.layout.struct_shift(*s))),
            TyKind::Tuple(_) => self.layout.tuple_type(ty).map(|i| (i, 0)),
            _ => None,
        }
    }

    /// The pair record an operand holds.
    fn pair_of(&self, o: &mir::Operand) -> Option<u32> {
        let mir::Operand::Local(l) = o else { return None };
        let TyKind::Fallible(v) = *self.types.kind(self.f.locals[l.index()].ty) else {
            return None;
        };
        self.layout.pair_type(v)
    }

    /// The pair record a `PairNew` is producing, taken from its destination.
    fn pair_for_result(&self) -> Option<u32> {
        let ty = match self.current_dst {
            Some(dst) => self.f.locals[dst as usize].ty,
            // A `return value, nil` builds the pair straight into the result.
            None => self.f.ret,
        };
        let TyKind::Fallible(v) = *self.types.kind(ty) else {
            return None;
        };
        self.layout.pair_type(v)
    }

    /// The array type and element type of an operand holding a slice.
    fn slice_of(&self, o: &mir::Operand) -> Option<(u32, TyId)> {
        let mir::Operand::Local(l) = o else { return None };
        let TyKind::Slice(elem) = *self.types.kind(self.f.locals[l.index()].ty) else {
            return None;
        };
        self.layout.slice_type(elem).map(|idx| (idx, elem))
    }

    /// The array type for the slice a `SliceNew` is producing.
    ///
    /// The destination local carries the slice type, and the emitter knows it
    /// because MIR always assigns a construction into one.
    fn slice_array_for_result(&self) -> Option<u32> {
        self.current_dst.and_then(|d| {
            let TyKind::Slice(elem) = *self.types.kind(self.f.locals[d as usize].ty) else {
                return None;
            };
            self.layout.slice_type(elem)
        })
    }

    /// An index, narrowed from Kite's 64-bit `int` to the i32 Wasm arrays use.
    fn index_operand(&mut self, func: &mut Function, o: &mir::Operand) {
        self.operand(func, o);
        func.instruction(&Instruction::I32WrapI64);
    }

    /// The box type for an operand about to be wrapped. The operand carries
    /// the *payload*, so the box is found by its own type.
    fn option_box_for(&self, o: &mir::Operand) -> Option<u32> {
        let payload = match o {
            mir::Operand::Local(l) => self.f.locals[l.index()].ty,
            mir::Operand::Int(_) => TyId::INT,
            mir::Operand::Float(_) => TyId::FLOAT,
            mir::Operand::Bool(_) => TyId::BOOL,
            mir::Operand::Str(_) => TyId::STR,
            _ => return None,
        };
        self.layout.option_type(payload)
    }

    /// The box type and payload type of an operand holding an optional.
    fn optional_of(&self, o: &mir::Operand) -> Option<(u32, TyId)> {
        let mir::Operand::Local(l) = o else { return None };
        let TyKind::Optional(inner) = *self.types.kind(self.f.locals[l.index()].ty) else {
            return None;
        };
        self.layout.option_type(inner).map(|idx| (idx, inner))
    }

    /// The enum an operand holds, if it holds one.
    fn enum_of(&self, o: &mir::Operand) -> Option<kite_hir::EnumId> {
        let mir::Operand::Local(l) = o else { return None };
        match self.types.kind(self.f.locals[l.index()].ty) {
            TyKind::Enum(e) => Some(*e),
            _ => None,
        }
    }

    /// The struct an operand holds, if it holds one.
    fn struct_of(&self, o: &mir::Operand) -> Option<kite_hir::StructId> {
        let mir::Operand::Local(l) = o else { return None };
        match self.types.kind(self.f.locals[l.index()].ty) {
            TyKind::Struct(s) => Some(*s),
            _ => None,
        }
    }

    fn operand_type(&self, o: &mir::Operand) -> ValType {
        match o {
            mir::Operand::Local(l) => {
                val_type_with(self.f.locals[l.index()].ty, self.types, self.layout)
            }
            mir::Operand::Int(_) => ValType::I64,
            mir::Operand::Float(_) => ValType::F64,
            _ => ValType::I32,
        }
    }

    fn is_str(&self, o: &mir::Operand) -> bool {
        match o {
            mir::Operand::Str(_) => true,
            mir::Operand::Local(l) => self.f.locals[l.index()].ty == TyId::STR,
            _ => false,
        }
    }

    fn binop(&mut self, func: &mut Function, op: BinOp) {
        use BinOp::*;
        let inst = match op {
            AddInt => Instruction::I64Add,
            SubInt => Instruction::I64Sub,
            MulInt => Instruction::I64Mul,
            DivInt => Instruction::I64DivS,
            RemInt => Instruction::I64RemS,
            AddFloat => Instruction::F64Add,
            SubFloat => Instruction::F64Sub,
            MulFloat => Instruction::F64Mul,
            DivFloat => Instruction::F64Div,
            BitAnd => Instruction::I64And,
            BitOr => Instruction::I64Or,
            BitXor => Instruction::I64Xor,
            Shl => Instruction::I64Shl,
            Shr => Instruction::I64ShrS,
            EqInt => Instruction::I64Eq,
            NeInt => Instruction::I64Ne,
            LtInt => Instruction::I64LtS,
            LeInt => Instruction::I64LeS,
            GtInt => Instruction::I64GtS,
            GeInt => Instruction::I64GeS,
            EqFloat => Instruction::F64Eq,
            NeFloat => Instruction::F64Ne,
            LtFloat => Instruction::F64Lt,
            LeFloat => Instruction::F64Le,
            GtFloat => Instruction::F64Gt,
            GeFloat => Instruction::F64Ge,
            EqBool => Instruction::I32Eq,
            NeBool => Instruction::I32Ne,
            // Handled by a host call before this table is consulted.
            EqStr | NeStr | ConcatStr => Instruction::Unreachable,
            // Structural equality on aggregates is not lowered yet.
            EqValue | NeValue => Instruction::Unreachable,
            // MIR has already turned these into branches.
            And | Or => Instruction::Unreachable,
        };
        func.instruction(&inst);
    }

    fn terminator(&mut self, func: &mut Function, term: &mir::Terminator) {
        match term {
            mir::Terminator::Goto(target) => self.jump(func, target.0, 0),

            mir::Terminator::Branch { cond, then, else_ } => {
                self.operand(func, cond);
                func.instruction(&Instruction::If(BlockType::Empty));
                self.jump(func, then.0, 1);
                func.instruction(&Instruction::Else);
                self.jump(func, else_.0, 1);
                func.instruction(&Instruction::End);
                // Both arms branch away, so control never reaches here.
                func.instruction(&Instruction::Unreachable);
            }

            mir::Terminator::Return(v) => {
                if self.f.ret != TyId::UNIT {
                    // MIR emits `return ()` for a function that falls off its
                    // end. The checker has already proved every path returns a
                    // value, so this path is dead — but it still has to be
                    // well-typed, and `unreachable` is what makes it so.
                    // `Operand::Unit` is the placeholder MIR emits for a
                    // function that falls off its end; `Nil` is a real value.
                    let usable = !matches!(v, None | Some(mir::Operand::Unit));
                    match v {
                        Some(o) if usable => self.operand(func, o),
                        _ => {
                            func.instruction(&Instruction::Unreachable);
                            return;
                        }
                    }
                }
                func.instruction(&Instruction::Return);
            }

            mir::Terminator::Unreachable => {
                func.instruction(&Instruction::Unreachable);
            }
        }
    }

    /// Set the program counter and branch back to the dispatch loop. `extra`
    /// accounts for blocks opened since the body started, such as the `if` in a
    /// conditional terminator.
    fn jump(&mut self, func: &mut Function, target: u32, extra: u32) {
        func.instruction(&Instruction::I32Const(target as i32));
        func.instruction(&Instruction::LocalSet(self.pc));
        func.instruction(&Instruction::Br(self.dispatch_depth() + extra));
    }
}

#[cfg(test)]
mod tests;
