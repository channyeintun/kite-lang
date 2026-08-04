//! The native backend.
//!
//! MIR lowered to Cranelift IR — no LLVM anywhere, which is the same bargain
//! the Wasm backend struck with `wasm-encoder`: fast builds, a small
//! toolchain, and generated code a person can read in a disassembler.
//!
//! # Two paths, one lowering
//!
//! The same lowering feeds two [`cranelift_module::Module`] implementations:
//! `cranelift-object` writes a relocatable object file that the system linker
//! joins with the `staticlib` build of `kite-rt`, and `cranelift-jit` maps
//! the code straight into this process, resolving the same runtime symbols by
//! name. `kitec run --native` takes the JIT path so running a file needs no
//! linker at all; `kitec build --emit native` takes the object path and hands
//! `cc` the rest.
//!
//! # Values
//!
//! An `int` is an `i64`, a `float` an `f64`, a `bool` an `i8`, and everything
//! else — strings, structs, enums, slices, maps, tuples, closures, optionals,
//! errors, fallible pairs, trait objects — is an `i64` holding a pointer into
//! `kite-rt`'s heap, with 0 for `nil`. The runtime's objects are
//! self-describing, so this backend does not reproduce the Wasm backend's
//! typed records; it must only produce *identical observable behaviour*,
//! and the shape tables it registers are what let one runtime routine render
//! and compare every value the way the bytecode VM does.
//!
//! # The collector's contract
//!
//! Every reference-typed MIR local becomes a Cranelift variable declared with
//! `declare_var_needs_stack_map`, so at each safepoint the live references
//! are spilled to stack slots the compiled function's stack maps describe —
//! and reloaded afterwards, which is what lets the nursery move objects. The
//! maps are serialised into a data section next to the code (with relocated
//! function addresses, so the same bytes work under the JIT and the linker)
//! and registered with the runtime before `main` runs.
//!
//! Allocation and mutation all cross into the runtime: variadic constructions
//! stage their operands in `KITE_RT_STAGE` — the native shape of the bytecode
//! VM's consecutive argument window — and the one in-place heap mutation the
//! language has, a `var` field write, is a runtime call so the write barrier
//! lives in exactly one place.

use cranelift_codegen::ir::{types, AbiParam, ArgumentExtension, InstBuilder, MemFlagsData, Signature, TrapCode, Type, Value};
use cranelift_codegen::isa::{CallConv, TargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use kite_hir::{BinOp, Builtin, StrKind, TyId, TyKind, Types, UnOp};
use kite_mir as mir;
use std::collections::HashMap;

mod support;
pub use support::{unsupported, Unsupported};

/// How a Kite type is seen by the runtime: one of `kite_rt::kind`'s bytes.
fn kind_of(ty: TyId, types: &Types) -> u8 {
    match types.kind(ty) {
        TyKind::Unit | TyKind::Never | TyKind::Error => kite_rt::kind::UNIT,
        TyKind::Bool => kite_rt::kind::BOOL,
        TyKind::Int => kite_rt::kind::INT,
        TyKind::Float => kite_rt::kind::FLOAT,
        _ => kite_rt::kind::REF,
    }
}

/// The Cranelift value type carrying a Kite value of this kind.
fn cl_type_of_kind(k: u8) -> Type {
    match k {
        kite_rt::kind::FLOAT => types::F64,
        kite_rt::kind::BOOL | kite_rt::kind::UNIT => types::I8,
        _ => types::I64,
    }
}

fn cl_type(ty: TyId, types: &Types) -> Type {
    cl_type_of_kind(kind_of(ty, types))
}

/// `i8` values are extended explicitly in signatures: Apple's AArch64 ABI
/// wants the caller to widen small integers, and Rust's `extern "C"` `u8`
/// carries the matching attribute on its side of the boundary.
fn abi(ty: Type) -> AbiParam {
    if ty == types::I8 {
        AbiParam { value_type: ty, extension: ArgumentExtension::Uext, purpose: cranelift_codegen::ir::ArgumentPurpose::Normal }
    } else {
        AbiParam::new(ty)
    }
}

fn make_sig(call_conv: CallConv, params: &[Type], ret: Option<Type>) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params = params.iter().map(|t| abi(*t)).collect();
    if let Some(r) = ret {
        sig.returns.push(abi(r));
    }
    sig
}

/// The runtime surface, as `(name, params, return)`. One row per
/// `extern "C"` function in `kite-rt`; the JIT resolves these by name from
/// `kite_rt::jit_symbols`, the linker from the staticlib, and a drift between
/// this table and the runtime is a link error rather than a silent
/// miscompilation.
const I64: Type = types::I64;
const F64: Type = types::F64;
const I8: Type = types::I8;

#[rustfmt::skip]
const RUNTIME: &[(&str, &[Type], Option<Type>)] = &[
    ("kite_rt_startup", &[], None),
    ("kite_rt_trap", &[I64, I64, I64], None),
    ("kite_rt_register_string", &[I64, I64, I64], None),
    ("kite_rt_register_struct_shape", &[I64, I64, I64], None),
    ("kite_rt_register_enum_shape", &[I64, I64, I64, I64], None),
    ("kite_rt_register_tuple_shape", &[I64, I64, I64], None),
    ("kite_rt_register_closure", &[I64, I64, I64, I64], None),
    ("kite_rt_register_fn_name", &[I64, I64, I64], None),
    ("kite_rt_register_extern", &[I64, I64, I64], None),
    ("kite_rt_register_vtable_method", &[I64, I64, I64, I64], None),
    ("kite_rt_register_stack_maps", &[I64], None),
    ("kite_rt_struct_new", &[I64, I64], Some(I64)),
    ("kite_rt_enum_new", &[I64, I64, I64], Some(I64)),
    ("kite_rt_tuple_new", &[I64, I64], Some(I64)),
    ("kite_rt_slice_new", &[I64, I64], Some(I64)),
    ("kite_rt_closure_new", &[I64, I64], Some(I64)),
    ("kite_rt_map_new", &[I64, I64, I64], Some(I64)),
    ("kite_rt_box_new", &[I64, I64], Some(I64)),
    ("kite_rt_pair_new", &[I64, I64, I64], Some(I64)),
    ("kite_rt_error_new", &[I64], Some(I64)),
    ("kite_rt_error_message", &[I64], Some(I64)),
    ("kite_rt_set_field", &[I64, I64, I64, I64], None),
    ("kite_rt_index_get", &[I64, I64], Some(I64)),
    ("kite_rt_set_index", &[I64, I64, I64], Some(I64)),
    ("kite_rt_slice_push", &[I64, I64], Some(I64)),
    ("kite_rt_slice_len", &[I64], Some(I64)),
    ("kite_rt_slice_get", &[I64, I64, I64], Some(I64)),
    ("kite_rt_map_len", &[I64], Some(I64)),
    ("kite_rt_map_get", &[I64, I64, I64, I64], Some(I64)),
    ("kite_rt_map_set", &[I64, I64, I64, I64], Some(I64)),
    ("kite_rt_map_keys", &[I64], Some(I64)),
    ("kite_rt_map_values", &[I64], Some(I64)),
    ("kite_rt_str_const", &[I64], Some(I64)),
    ("kite_rt_str_concat", &[I64, I64], Some(I64)),
    ("kite_rt_str_eq", &[I64, I64], Some(I8)),
    ("kite_rt_str_compare", &[I64, I64], Some(I64)),
    ("kite_rt_str_len", &[I64], Some(I64)),
    ("kite_rt_str_trim", &[I64], Some(I64)),
    ("kite_rt_str_slice", &[I64, I64, I64], Some(I64)),
    ("kite_rt_str_index_of", &[I64, I64], Some(I64)),
    ("kite_rt_str_code_at", &[I64, I64], Some(I64)),
    ("kite_rt_value_eq", &[I64, I64, I64], Some(I8)),
    ("kite_rt_virtual_lookup", &[I64, I64, I64], Some(I64)),
    ("kite_rt_print_int", &[I64], None),
    ("kite_rt_print_float", &[F64], None),
    ("kite_rt_print_bool", &[I8], None),
    ("kite_rt_print_str", &[I64], None),
    ("kite_rt_print_unit", &[], None),
    ("kite_rt_print_ref", &[I64], None),
    ("kite_rt_str_of_int", &[I64], Some(I64)),
    ("kite_rt_text_from_code", &[I64], Some(I64)),
    ("kite_rt_read_line", &[], Some(I64)),
    ("kite_rt_error_int", &[I64], None),
    ("kite_rt_error_float", &[F64], None),
    ("kite_rt_error_bool", &[I8], None),
    ("kite_rt_error_str", &[I64], None),
    ("kite_rt_error_unit", &[], None),
    ("kite_rt_str_of_float", &[F64], Some(I64)),
    ("kite_rt_str_of_bool", &[I8], Some(I64)),
    ("kite_rt_str_of_ref", &[I64], Some(I64)),
    ("kite_rt_draw_rect", &[F64, F64, F64, F64, I64], None),
    ("kite_rt_draw_rrect", &[F64, F64, F64, F64, F64, I64], None),
    ("kite_rt_draw_drrect", &[F64, F64, F64, F64, F64, F64, I64], None),
    ("kite_rt_draw_alpha", &[F64], None),
    ("kite_rt_draw_font", &[F64, I64], None),
    ("kite_rt_draw_text", &[F64, F64, I64, I64], None),
    ("kite_rt_draw_field", &[F64, F64, F64, F64, I64, I64, I64, I64, I8], None),
    ("kite_rt_draw_image", &[F64, F64, F64, F64, I64], None),
    ("kite_rt_draw_semantics", &[F64, F64, F64, F64, I64, I64, I64, I64], None),
    ("kite_rt_draw_clip", &[F64, F64, F64, F64], None),
    ("kite_rt_draw_unclip", &[], None),
    ("kite_rt_text_width", &[I64], Some(F64)),
    ("kite_rt_text_height", &[], Some(F64)),
    ("kite_rt_require", &[I8, I64], None),
    ("kite_rt_call_extern", &[I64, I64], Some(I64)),
    ("kite_rt_task_spawn", &[I64], None),
    ("kite_rt_task_wake_at", &[I64], None),
    ("kite_rt_task_park", &[], None),
    ("kite_rt_task_wait_host", &[], None),
    ("kite_rt_time_now", &[], Some(I64)),
    ("kite_rt_drive", &[], None),
];

/// Trap codes shared with `kite_rt_trap`.
mod trap_code {
    pub const DIV_ZERO: i64 = 1;
    pub const OVERFLOW_ADD: i64 = 2;
    pub const OVERFLOW_SUB: i64 = 3;
    pub const OVERFLOW_MUL: i64 = 4;
    pub const OVERFLOW_DIV: i64 = 5;
    pub const OVERFLOW_REM: i64 = 6;
    pub const OVERFLOW_SHL: i64 = 7;
    pub const OVERFLOW_SHR: i64 = 8;
    pub const UNREACHABLE: i64 = 10;
}

/// Everything the per-function lowering needs from the module scan.
struct Meta {
    /// Distinct tuple types, in first-appearance order; position is the shape
    /// id the runtime knows a tuple by.
    tuple_shape: HashMap<TyId, u32>,
    tuples: Vec<TyId>,
    /// Lifted functions reached by a `ClosureNew`, with their capture counts.
    envs: Vec<(u32, usize)>,
    /// Position of each trait in `program.vtables`, the runtime's table index.
    traits: Vec<u32>,
}

fn scan(program: &mir::Program, types: &Types) -> Meta {
    let mut tuples: Vec<TyId> = Vec::new();
    for f in &program.fns {
        for ty in f.locals.iter().map(|l| l.ty).chain([f.ret]) {
            if matches!(types.kind(ty), TyKind::Tuple(_)) && !tuples.contains(&ty) {
                tuples.push(ty);
            }
        }
    }
    let mut envs: Vec<(u32, usize)> = Vec::new();
    for f in &program.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                if let mir::Inst::Assign { value: mir::Rvalue::ClosureNew { func, captures }, .. } = s {
                    if !envs.iter().any(|(g, _)| *g == func.0) {
                        envs.push((func.0, captures.len()));
                    }
                }
            }
        }
    }
    envs.sort_by_key(|(f, _)| *f);
    Meta {
        tuple_shape: tuples.iter().enumerate().map(|(i, t)| (*t, i as u32)).collect(),
        tuples: tuples.clone(),
        envs,
        traits: program.vtables.iter().map(|v| v.trait_id.0).collect(),
    }
}

fn host_isa(pic: bool) -> Result<std::sync::Arc<dyn TargetIsa>, String> {
    let mut flags = settings::builder();
    // Frame pointers are how the collector walks from its own Rust frame up
    // through compiled frames to find the safepoints; without them there is
    // no chain to follow.
    flags.set("preserve_frame_pointers", "true").unwrap();
    flags.set("is_pic", if pic { "true" } else { "false" }).unwrap();
    flags.set("opt_level", "speed").unwrap();
    let isa_builder = cranelift_native::builder()
        .map_err(|e| format!("this host is not supported by the native backend: {}", e))?;
    isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| format!("cannot configure the native target: {}", e))
}

/// Everything `build` declares and defines, by id.
struct Artifacts {
    wrapper: FuncId,
}

/// One collected safepoint: where in the function, and which stack slots.
type FnMaps = Vec<(u32, Vec<u32>)>;

struct ModuleCx<'a, M: Module> {
    module: &'a mut M,
    program: &'a mir::Program,
    types: &'a Types,
    meta: Meta,
    runtime: HashMap<&'static str, FuncId>,
    fns: Vec<FuncId>,
    thunks: HashMap<u32, FuncId>,
    stage: DataId,
    call_conv: CallConv,
}

impl<'a, M: Module> ModuleCx<'a, M> {
    fn rt(&self, name: &str) -> FuncId {
        self.runtime[name]
    }

    fn fn_sig(&self, f: &mir::Function) -> Signature {
        let params: Vec<Type> = (0..f.param_count)
            .map(|i| cl_type(f.locals[i].ty, self.types))
            .collect();
        let ret = (f.ret != TyId::UNIT).then(|| cl_type(f.ret, self.types));
        make_sig(self.call_conv, &params, ret)
    }
}

/// Lower the whole program into `module`. Returns the ids the caller needs to
/// run or export what was built.
fn build<M: Module>(
    module: &mut M,
    program: &mir::Program,
    types: &Types,
) -> Result<Artifacts, String> {
    let call_conv = module.isa().default_call_conv();
    let meta = scan(program, types);

    // ---- declarations ----------------------------------------------------
    let mut runtime = HashMap::new();
    for (name, params, ret) in RUNTIME {
        let sig = make_sig(call_conv, params, *ret);
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| e.to_string())?;
        runtime.insert(*name, id);
    }
    let stage = module
        .declare_data("KITE_RT_STAGE", Linkage::Import, true, false)
        .map_err(|e| e.to_string())?;

    let mut cx = ModuleCx {
        module,
        program,
        types,
        meta,
        runtime,
        fns: Vec::new(),
        thunks: HashMap::new(),
        stage,
        call_conv,
    };

    for (i, f) in program.fns.iter().enumerate() {
        let sig = cx.fn_sig(f);
        let id = cx
            .module
            .declare_function(&format!("kite_fn_{}", i), Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        cx.fns.push(id);
    }
    for (func, count) in cx.meta.envs.clone() {
        let lifted = &program.fns[func as usize];
        let mut params = vec![I64];
        params.extend(
            lifted.locals[count..lifted.param_count]
                .iter()
                .map(|l| cl_type(l.ty, types)),
        );
        let ret = (lifted.ret != TyId::UNIT).then(|| cl_type(lifted.ret, types));
        let sig = make_sig(call_conv, &params, ret);
        let id = cx
            .module
            .declare_function(&format!("kite_thunk_{}", func), Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        cx.thunks.insert(func, id);
    }

    // ---- function bodies -------------------------------------------------
    let mut fbcx = FunctionBuilderContext::new();
    let mut maps: Vec<(FuncId, FnMaps)> = Vec::new();
    for (i, f) in program.fns.iter().enumerate() {
        let id = cx.fns[i];
        let collected = define_fn(&mut cx, &mut fbcx, id, i, f)?;
        if !collected.is_empty() {
            maps.push((id, collected));
        }
    }
    for (func, count) in cx.meta.envs.clone() {
        let id = cx.thunks[&func];
        let collected = define_thunk(&mut cx, &mut fbcx, id, func, count)?;
        if !collected.is_empty() {
            maps.push((id, collected));
        }
    }

    // ---- the stack-map table ---------------------------------------------
    // Words: [fn count]; per function [address][map count]; per map
    // [return-address offset][entry count][offsets…]. The address slots are
    // function relocations, which is what lets the identical bytes serve the
    // JIT and the linker.
    let mut blob: Vec<u8> = Vec::new();
    let mut addr_slots: Vec<(u32, FuncId)> = Vec::new();
    let word = |blob: &mut Vec<u8>, v: u64| blob.extend_from_slice(&v.to_le_bytes());
    word(&mut blob, maps.len() as u64);
    for (id, fn_maps) in &maps {
        addr_slots.push((blob.len() as u32, *id));
        word(&mut blob, 0);
        word(&mut blob, fn_maps.len() as u64);
        for (ret_off, offsets) in fn_maps {
            word(&mut blob, *ret_off as u64);
            word(&mut blob, offsets.len() as u64);
            for off in offsets {
                word(&mut blob, *off as u64);
            }
        }
    }
    let maps_data = {
        let id = cx
            .module
            .declare_data("kite_stack_maps", Linkage::Local, false, false)
            .map_err(|e| e.to_string())?;
        let mut desc = DataDescription::new();
        desc.define(blob.into_boxed_slice());
        for (off, func) in addr_slots {
            let fref = cx.module.declare_func_in_data(func, &mut desc);
            desc.write_function_addr(off, fref);
        }
        cx.module.define_data(id, &desc).map_err(|e| e.to_string())?;
        id
    };

    // ---- registration and the wrapper ------------------------------------
    let init = define_init(&mut cx, &mut fbcx, maps_data)?;
    let wrapper = define_wrapper(&mut cx, &mut fbcx, init)?;

    Ok(Artifacts { wrapper })
}

/// A named, read-only byte blob the registration function can point at.
fn define_bytes<M: Module>(
    cx: &mut ModuleCx<M>,
    name: &str,
    bytes: &[u8],
) -> Result<DataId, String> {
    let id = cx
        .module
        .declare_data(name, Linkage::Local, false, false)
        .map_err(|e| e.to_string())?;
    let mut desc = DataDescription::new();
    // Data must not be empty; an empty shape keeps a single padding byte the
    // registered length never reaches.
    let owned: Box<[u8]> = if bytes.is_empty() { Box::new([0]) } else { bytes.into() };
    desc.define(owned);
    cx.module.define_data(id, &desc).map_err(|e| e.to_string())?;
    Ok(id)
}

/// The registration function: everything the runtime must know about this
/// program — strings, shapes, thunks, names, dispatch tables, stack maps —
/// delivered as plain calls, because the addresses involved are exactly what
/// the JIT and the linker already relocate.
fn define_init<M: Module>(
    cx: &mut ModuleCx<M>,
    fbcx: &mut FunctionBuilderContext,
    maps_data: DataId,
) -> Result<FuncId, String> {
    // Byte tables first, so the builder below only references them.
    let mut string_data = Vec::new();
    for (i, s) in cx.program.strings.iter().enumerate() {
        string_data.push((define_bytes(cx, &format!("kite_str_{}", i), s.as_bytes())?, s.len()));
    }
    let mut struct_shapes = Vec::new();
    for i in 0..cx.types.struct_count() {
        let def = cx.types.struct_def(kite_hir::StructId(i as u32));
        let kinds: Vec<u8> = def.fields.iter().map(|f| kind_of(f.ty, cx.types)).collect();
        struct_shapes.push((define_bytes(cx, &format!("kite_shape_s{}", i), &kinds)?, kinds.len()));
    }
    let mut enum_shapes = Vec::new();
    for i in 0..cx.types.enum_count() {
        let def = cx.types.enum_def(kite_hir::EnumId(i as u32));
        for (v, variant) in def.variants.iter().enumerate() {
            let kinds: Vec<u8> =
                variant.fields.iter().map(|f| kind_of(f.ty, cx.types)).collect();
            enum_shapes.push((
                i as u64,
                v as u64,
                define_bytes(cx, &format!("kite_shape_e{}_{}", i, v), &kinds)?,
                kinds.len(),
            ));
        }
    }
    let mut tuple_shapes = Vec::new();
    for (i, ty) in cx.meta.tuples.clone().iter().enumerate() {
        let TyKind::Tuple(elems) = cx.types.kind(*ty).clone() else { continue };
        let kinds: Vec<u8> = elems.iter().map(|e| kind_of(*e, cx.types)).collect();
        tuple_shapes.push((define_bytes(cx, &format!("kite_shape_t{}", i), &kinds)?, kinds.len()));
    }
    let mut closure_shapes = Vec::new();
    for (func, count) in cx.meta.envs.clone() {
        let lifted = &cx.program.fns[func as usize];
        let kinds: Vec<u8> = lifted.locals[..count]
            .iter()
            .map(|l| kind_of(l.ty, cx.types))
            .collect();
        closure_shapes.push((
            func,
            define_bytes(cx, &format!("kite_shape_c{}", func), &kinds)?,
            kinds.len(),
        ));
    }
    let mut fn_names = Vec::new();
    for (i, f) in cx.program.fns.iter().enumerate() {
        fn_names.push((define_bytes(cx, &format!("kite_name_{}", i), f.name.as_bytes())?, f.name.len()));
    }
    let mut extern_names = Vec::new();
    for (i, e) in cx.program.externs.iter().enumerate() {
        let name = format!("{}.{}", e.host, e.name);
        extern_names.push((define_bytes(cx, &format!("kite_extern_{}", i), name.as_bytes())?, name.len()));
    }

    let cfg = cx.module.target_config();
    let sig = make_sig(cx.call_conv, &[], None);
    let id = cx
        .module
        .declare_function("kite_init_program", Linkage::Local, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = cx.module.make_context();
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, fbcx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    // Tiny helpers to keep the call sites below readable.
    struct Init<'x, 'y> {
        b: FunctionBuilder<'x>,
        refs: HashMap<&'static str, cranelift_codegen::ir::FuncRef>,
        _m: std::marker::PhantomData<&'y ()>,
    }
    let mut refs = HashMap::new();
    for name in [
        "kite_rt_register_string",
        "kite_rt_register_struct_shape",
        "kite_rt_register_enum_shape",
        "kite_rt_register_tuple_shape",
        "kite_rt_register_closure",
        "kite_rt_register_fn_name",
        "kite_rt_register_extern",
        "kite_rt_register_vtable_method",
        "kite_rt_register_stack_maps",
    ] {
        let fid = cx.rt(name);
        refs.insert(name, cx.module.declare_func_in_func(fid, b.func));
    }
    let mut init = Init { b, refs, _m: std::marker::PhantomData };
    impl<'x, 'y> Init<'x, 'y> {
        fn call(&mut self, name: &'static str, args: &[Value]) {
            let f = self.refs[name];
            self.b.ins().call(f, args);
        }
        fn i(&mut self, v: u64) -> Value {
            self.b.ins().iconst(types::I64, v as i64)
        }
    }

    for (i, (data, len)) in string_data.iter().enumerate() {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (idx, n) = (init.i(i as u64), init.i(*len as u64));
        init.call("kite_rt_register_string", &[idx, ptr, n]);
    }
    for (i, (data, len)) in struct_shapes.iter().enumerate() {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (idx, n) = (init.i(i as u64), init.i(*len as u64));
        init.call("kite_rt_register_struct_shape", &[idx, ptr, n]);
    }
    for (e, v, data, len) in &enum_shapes {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (e, v, n) = (init.i(*e), init.i(*v), init.i(*len as u64));
        init.call("kite_rt_register_enum_shape", &[e, v, ptr, n]);
    }
    for (i, (data, len)) in tuple_shapes.iter().enumerate() {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (idx, n) = (init.i(i as u64), init.i(*len as u64));
        init.call("kite_rt_register_tuple_shape", &[idx, ptr, n]);
    }
    for (func, data, len) in &closure_shapes {
        let thunk = cx.thunks[func];
        let fref = cx.module.declare_func_in_func(thunk, init.b.func);
        let addr = init.b.ins().func_addr(I64, fref);
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (f, n) = (init.i(*func as u64), init.i(*len as u64));
        init.call("kite_rt_register_closure", &[f, addr, ptr, n]);
    }
    for (i, (data, len)) in fn_names.iter().enumerate() {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (idx, n) = (init.i(i as u64), init.i(*len as u64));
        init.call("kite_rt_register_fn_name", &[idx, ptr, n]);
    }
    for (i, (data, len)) in extern_names.iter().enumerate() {
        let gv = cx.module.declare_data_in_func(*data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        let (idx, n) = (init.i(i as u64), init.i(*len as u64));
        init.call("kite_rt_register_extern", &[idx, ptr, n]);
    }
    for (t, table) in cx.program.vtables.iter().enumerate() {
        for entry in &table.entries {
            for (m, callee) in entry.methods.iter().enumerate() {
                let fref = cx.module.declare_func_in_func(cx.fns[callee.index()], init.b.func);
                let addr = init.b.ins().func_addr(I64, fref);
                let (t, tag, m) = (
                    init.i(t as u64),
                    init.i(entry.tag.encode() as u64),
                    init.i(m as u64),
                );
                init.call("kite_rt_register_vtable_method", &[t, tag, m, addr]);
            }
        }
    }
    {
        let gv = cx.module.declare_data_in_func(maps_data, init.b.func);
        let ptr = init.b.ins().symbol_value(I64, gv);
        init.call("kite_rt_register_stack_maps", &[ptr]);
    }
    init.b.ins().return_(&[]);
    let Init { b, .. } = init;
    b.finalize(cfg);

    cx.module.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    cx.module.clear_context(&mut ctx);
    Ok(id)
}

/// The entry the outside world calls: start the runtime, register the
/// program, run `main`, then drive the scheduler until nothing is left —
/// because `main` returning is not the program ending.
fn define_wrapper<M: Module>(
    cx: &mut ModuleCx<M>,
    fbcx: &mut FunctionBuilderContext,
    init: FuncId,
) -> Result<FuncId, String> {
    let cfg = cx.module.target_config();
    let sig = make_sig(cx.call_conv, &[], Some(types::I32));
    let id = cx
        .module
        .declare_function("main", Linkage::Export, &sig)
        .map_err(|e| e.to_string())?;
    let mut ctx = cx.module.make_context();
    ctx.func.signature = sig;
    let mut b = FunctionBuilder::new(&mut ctx.func, fbcx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    let startup = cx.rt("kite_rt_startup");
    let f = cx.module.declare_func_in_func(startup, b.func);
    b.ins().call(f, &[]);
    let init_ref = cx.module.declare_func_in_func(init, b.func);
    b.ins().call(init_ref, &[]);
    if let Some(entry_fn) = cx.program.entry {
        let f = cx.module.declare_func_in_func(cx.fns[entry_fn.index()], b.func);
        b.ins().call(f, &[]);
    }
    let drive = cx.rt("kite_rt_drive");
    let f = cx.module.declare_func_in_func(drive, b.func);
    b.ins().call(f, &[]);
    let zero = b.ins().iconst(types::I32, 0);
    b.ins().return_(&[zero]);
    b.finalize(cfg);
    cx.module.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    cx.module.clear_context(&mut ctx);
    Ok(id)
}

/// The wrapper that gives every closure of one Kite type a callable address:
/// unpack the captures from the closure object, then enter the lifted
/// function like any other call.
fn define_thunk<M: Module>(
    cx: &mut ModuleCx<M>,
    fbcx: &mut FunctionBuilderContext,
    id: FuncId,
    func: u32,
    count: usize,
) -> Result<FnMaps, String> {
    let cfg = cx.module.target_config();
    let lifted = &cx.program.fns[func as usize];
    let mut ctx = cx.module.make_context();
    ctx.func.signature = cx
        .module
        .declarations()
        .get_function_decl(id)
        .signature
        .clone();
    let mut b = FunctionBuilder::new(&mut ctx.func, fbcx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    let closure = b.block_params(entry)[0];
    let mut args = Vec::with_capacity(lifted.param_count);
    for i in 0..count {
        let ty = cl_type(lifted.locals[i].ty, cx.types);
        // Captures sit after the header and the thunk slot.
        let off = 16 + 8 + 8 * i as i32;
        args.push(b.ins().load(ty, MemFlagsData::trusted(), closure, off));
    }
    let params: Vec<Value> = b.block_params(entry)[1..].to_vec();
    args.extend(params);
    let callee = cx.module.declare_func_in_func(cx.fns[func as usize], b.func);
    let call = b.ins().call(callee, &args);
    let results = b.inst_results(call).to_vec();
    b.ins().return_(&results);
    b.finalize(cfg);
    cx.module.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    let maps = collect_maps(&ctx);
    cx.module.clear_context(&mut ctx);
    Ok(maps)
}

/// The safepoints Cranelift recorded for the function just defined.
fn collect_maps(ctx: &cranelift_codegen::Context) -> FnMaps {
    let compiled = ctx.compiled_code().expect("the function was just compiled");
    compiled
        .buffer
        .user_stack_maps()
        .iter()
        .map(|(ret_off, _, map)| {
            let offsets: Vec<u32> = map.entries().map(|(_, off)| off).collect();
            (*ret_off, offsets)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Function bodies
// ---------------------------------------------------------------------------

struct FnLower<'a, 'b, M: Module> {
    cx: &'a mut ModuleCx<'b, M>,
    f: &'a mir::Function,
    fn_index: usize,
    b: FunctionBuilder<'a>,
    vars: Vec<Variable>,
    blocks: Vec<cranelift_codegen::ir::Block>,
    /// Function references imported into this function, on first use.
    rt_refs: HashMap<&'static str, cranelift_codegen::ir::FuncRef>,
    fn_refs: HashMap<usize, cranelift_codegen::ir::FuncRef>,
    stage_base: Option<Value>,
}

fn define_fn<M: Module>(
    cx: &mut ModuleCx<M>,
    fbcx: &mut FunctionBuilderContext,
    id: FuncId,
    fn_index: usize,
    f: &mir::Function,
) -> Result<FnMaps, String> {
    let cfg = cx.module.target_config();
    let mut ctx = cx.module.make_context();
    ctx.func.signature = cx.fn_sig(f);
    let mut b = FunctionBuilder::new(&mut ctx.func, fbcx);

    // One variable per MIR local. A reference-typed local is declared as
    // needing a stack map, which is the whole precise-roots story: Cranelift
    // spills it at each safepoint, records where, and reloads after.
    let mut vars = Vec::with_capacity(f.locals.len());
    for l in &f.locals {
        let ty = cl_type(l.ty, cx.types);
        let var = b.declare_var(ty);
        if kind_of(l.ty, cx.types) == kite_rt::kind::REF {
            b.declare_var_needs_stack_map(var);
        }
        vars.push(var);
    }

    let entry = b.create_block();
    let blocks: Vec<_> = f.blocks.iter().map(|_| b.create_block()).collect();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params: Vec<Value> = b.block_params(entry).to_vec();
    for (var, v) in vars.iter().zip(params) {
        b.def_var(*var, v);
    }
    // Every other local starts as its type's all-zero value — `nil`, 0, 0.0 —
    // so a use on a path the checker knows is impossible still reads a value
    // of the right type rather than tripping the SSA builder.
    for (i, l) in f.locals.iter().enumerate().skip(f.param_count) {
        let ty = cl_type(l.ty, cx.types);
        let zero = if ty == types::F64 {
            b.ins().f64const(0.0)
        } else {
            b.ins().iconst(ty, 0)
        };
        b.def_var(vars[i], zero);
    }
    b.ins().jump(blocks[0], &[]);

    let mut lower = FnLower {
        cx,
        f,
        fn_index,
        b,
        vars,
        blocks,
        rt_refs: HashMap::new(),
        fn_refs: HashMap::new(),
        stage_base: None,
    };

    let reachable = mir::reachable_blocks(f);
    for (i, block) in f.blocks.iter().enumerate() {
        lower.b.switch_to_block(lower.blocks[i]);
        if !reachable[i] {
            // Kept so block indices stay stable, like everywhere else; a
            // bare trap is a valid, empty-cost body for a block nothing
            // reaches.
            lower.b.ins().trap(TrapCode::user(2).unwrap());
            continue;
        }
        // The staging window's address is per-block: an SSA value made in the
        // entry block would have to dominate every use, and re-materialising
        // a symbol address is one instruction.
        lower.stage_base = None;
        for stmt in &block.stmts {
            lower.stmt(stmt);
        }
        lower.terminator(&block.term, i);
    }
    lower.b.seal_all_blocks();
    let FnLower { b, .. } = lower;
    b.finalize(cfg);

    cx.module.define_function(id, &mut ctx).map_err(|e| {
        format!("compiling `{}`: {}", f.name, e)
    })?;
    let maps = collect_maps(&ctx);
    cx.module.clear_context(&mut ctx);
    Ok(maps)
}

impl<'a, 'b, M: Module> FnLower<'a, 'b, M> {
    fn rt_ref(&mut self, name: &'static str) -> cranelift_codegen::ir::FuncRef {
        if let Some(r) = self.rt_refs.get(name) {
            return *r;
        }
        let id = self.cx.rt(name);
        let r = self.cx.module.declare_func_in_func(id, self.b.func);
        self.rt_refs.insert(name, r);
        r
    }

    fn call_rt(&mut self, name: &'static str, args: &[Value]) -> Option<Value> {
        let f = self.rt_ref(name);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call).first().copied()
    }

    fn kite_fn_ref(&mut self, index: usize) -> cranelift_codegen::ir::FuncRef {
        if let Some(r) = self.fn_refs.get(&index) {
            return *r;
        }
        let id = self.cx.fns[index];
        let r = self.cx.module.declare_func_in_func(id, self.b.func);
        self.fn_refs.insert(index, r);
        r
    }

    fn stage_base(&mut self) -> Value {
        if let Some(v) = self.stage_base {
            return v;
        }
        let gv = self.cx.module.declare_data_in_func(self.cx.stage, self.b.func);
        let v = self.b.ins().symbol_value(I64, gv);
        self.stage_base = Some(v);
        v
    }

    fn iconst(&mut self, v: i64) -> Value {
        self.b.ins().iconst(I64, v)
    }

    // ---- types of things -------------------------------------------------

    fn local_ty(&self, l: mir::Local) -> TyId {
        self.f.locals[l.index()].ty
    }

    fn operand_ty(&self, o: &mir::Operand) -> TyId {
        match o {
            mir::Operand::Local(l) => self.local_ty(*l),
            mir::Operand::Int(_) => TyId::INT,
            mir::Operand::Float(_) => TyId::FLOAT,
            mir::Operand::Bool(_) => TyId::BOOL,
            mir::Operand::Str(_) => TyId::STR,
            mir::Operand::Unit => TyId::UNIT,
            // `nil` only ever sits where a reference belongs; `ERR` is a
            // reference-kinded stand-in, since the exact type is the slot's.
            mir::Operand::Nil => TyId::ERR,
            mir::Operand::Default(t) => *t,
        }
    }

    fn kind(&self, ty: TyId) -> u8 {
        kind_of(ty, self.cx.types)
    }

    // ---- values ----------------------------------------------------------

    fn operand(&mut self, o: &mir::Operand) -> Value {
        match o {
            mir::Operand::Local(l) => self.b.use_var(self.vars[l.index()]),
            mir::Operand::Int(v) => self.b.ins().iconst(I64, *v),
            mir::Operand::Float(v) => self.b.ins().f64const(*v),
            mir::Operand::Bool(v) => self.b.ins().iconst(I8, i64::from(*v)),
            mir::Operand::Str(s) => {
                let idx = self.iconst(s.0 as i64);
                // Constants are immortal and never move, so the returned
                // pointer is the one value that may live across safepoints
                // without a stack-map slot.
                self.call_rt("kite_rt_str_const", &[idx]).unwrap()
            }
            mir::Operand::Unit => self.b.ins().iconst(I8, 0),
            mir::Operand::Nil => self.iconst(0),
            mir::Operand::Default(t) => {
                let ty = cl_type(*t, self.cx.types);
                if ty == F64 {
                    self.b.ins().f64const(0.0)
                } else {
                    self.b.ins().iconst(ty, 0)
                }
            }
        }
    }

    /// An operand as the raw 8-byte word the runtime traffics in.
    fn operand_word(&mut self, o: &mir::Operand) -> Value {
        let v = self.operand(o);
        self.as_word(v)
    }

    fn as_word(&mut self, v: Value) -> Value {
        let ty = self.b.func.dfg.value_type(v);
        if ty == F64 {
            self.b.ins().bitcast(I64, MemFlagsData::new(), v)
        } else if ty == I8 {
            self.b.ins().uextend(I64, v)
        } else {
            v
        }
    }

    fn word_as(&mut self, w: Value, ty: TyId) -> Value {
        let cl = cl_type(ty, self.cx.types);
        if cl == F64 {
            self.b.ins().bitcast(F64, MemFlagsData::new(), w)
        } else if cl == I8 {
            self.b.ins().ireduce(I8, w)
        } else {
            w
        }
    }

    fn def(&mut self, dst: mir::Local, v: Value) {
        // Whatever the shape of the producing expression, the local's own
        // type decides its representation.
        let want = cl_type(self.local_ty(dst), self.cx.types);
        let got = self.b.func.dfg.value_type(v);
        let v = if want == got {
            v
        } else if want == I8 && got == I64 {
            self.b.ins().ireduce(I8, v)
        } else if want == I64 && got == I8 {
            self.b.ins().uextend(I64, v)
        } else if want == F64 && got == I64 {
            self.b.ins().bitcast(F64, MemFlagsData::new(), v)
        } else if want == I64 && got == F64 {
            self.b.ins().bitcast(I64, MemFlagsData::new(), v)
        } else {
            v
        };
        self.b.def_var(self.vars[dst.index()], v);
    }

    fn def_zero(&mut self, dst: mir::Local) {
        let ty = cl_type(self.local_ty(dst), self.cx.types);
        let v = if ty == F64 {
            self.b.ins().f64const(0.0)
        } else {
            self.b.ins().iconst(ty, 0)
        };
        self.b.def_var(self.vars[dst.index()], v);
    }

    /// Write operands into the staging window, for a variadic construction.
    fn stage(&mut self, args: &[mir::Operand]) {
        let base = self.stage_base();
        for (i, a) in args.iter().enumerate() {
            let w = self.operand_word(a);
            self.b
                .ins()
                .store(MemFlagsData::trusted(), w, base, (8 * i) as i32);
        }
    }

    /// Branch to a fresh trap block when `cond` is true.
    fn trap_if(&mut self, cond: Value, code: i64, a: i64, bb: i64) {
        let tb = self.b.create_block();
        let cont = self.b.create_block();
        self.b.ins().brif(cond, tb, &[], cont, &[]);
        self.b.switch_to_block(tb);
        self.b.seal_block(tb);
        let args = [self.iconst(code), self.iconst(a), self.iconst(bb)];
        self.call_rt("kite_rt_trap", &args);
        self.b.ins().trap(TrapCode::user(1).unwrap());
        self.b.switch_to_block(cont);
        self.b.seal_block(cont);
    }

    // ---- statements ------------------------------------------------------

    fn stmt(&mut self, s: &mir::Inst) {
        match s {
            mir::Inst::Assign { dst, value } => self.rvalue(*dst, value),
            mir::Inst::SetField { base, index, value } => {
                let obj = self.operand(base);
                let w = self.operand_word(value);
                let is_ref = self.kind(self.operand_ty(value)) == kite_rt::kind::REF;
                let args = [obj, self.iconst(*index as i64), w, self.iconst(i64::from(is_ref))];
                self.call_rt("kite_rt_set_field", &args);
            }
            // Slices are copy-on-write values: the runtime copies, mutates
            // the copy, and the local is rebound — the same observable
            // behaviour as the VM's clone-if-shared.
            mir::Inst::SetIndex { base, index, value } => {
                let cur = self.operand(base);
                let idx = self.operand(index);
                let w = self.operand_word(value);
                let new = self.call_rt("kite_rt_set_index", &[cur, idx, w]).unwrap();
                if let mir::Operand::Local(l) = base {
                    self.b.def_var(self.vars[l.index()], new);
                }
            }
            mir::Inst::SlicePush { local, value } => {
                let cur = self.b.use_var(self.vars[local.index()]);
                let w = self.operand_word(value);
                let new = self.call_rt("kite_rt_slice_push", &[cur, w]).unwrap();
                self.b.def_var(self.vars[local.index()], new);
            }
            mir::Inst::MapSet { local, key, value } => {
                let cur = self.b.use_var(self.vars[local.index()]);
                let kw = self.operand_word(key);
                let key_kind = self.kind(self.operand_ty(key));
                let vw = self.operand_word(value);
                let kk = self.iconst(key_kind as i64);
                let new = self.call_rt("kite_rt_map_set", &[cur, kw, kk, vw]).unwrap();
                self.b.def_var(self.vars[local.index()], new);
            }
        }
    }

    // ---- rvalues ---------------------------------------------------------

    fn rvalue(&mut self, dst: mir::Local, value: &mir::Rvalue) {
        match value {
            mir::Rvalue::Await { .. } | mir::Rvalue::Yield => {
                unreachable!("`await` survived the state-machine transform")
            }
            mir::Rvalue::Use(o) => {
                let v = self.operand(o);
                self.def(dst, v);
            }
            mir::Rvalue::Binary { op, lhs, rhs } => self.binary(dst, *op, lhs, rhs),
            mir::Rvalue::Unary { op, operand } => {
                let v = self.operand(operand);
                let r = match op {
                    UnOp::NegInt => {
                        let min = self.b.ins().icmp_imm_s(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            v,
                            i64::MIN,
                        );
                        self.trap_if(min, trap_code::OVERFLOW_SUB, 0, 0);
                        self.b.ins().ineg(v)
                    }
                    UnOp::NegFloat => self.b.ins().fneg(v),
                    UnOp::Not => self.b.ins().bxor_imm_s(v, 1),
                };
                self.def(dst, r);
            }
            mir::Rvalue::Call { callee, args } => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                let f = self.kite_fn_ref(callee.index());
                let call = self.b.ins().call(f, &vals);
                match self.b.inst_results(call).first().copied() {
                    Some(v) => self.def(dst, v),
                    None => self.def_zero(dst),
                }
            }
            mir::Rvalue::ClosureNew { func, captures } => {
                self.stage(captures);
                let args = [self.iconst(func.0 as i64), self.iconst(captures.len() as i64)];
                let v = self.call_rt("kite_rt_closure_new", &args).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::CallClosure { callee, args } => {
                let ty = self.operand_ty(callee);
                let TyKind::Fn { params, ret } = self.cx.types.kind(ty).clone() else {
                    unreachable!("calling a non-function value")
                };
                let clo = self.operand(callee);
                let thunk = self.b.ins().load(I64, MemFlagsData::trusted(), clo, 16);
                let mut vals = vec![clo];
                vals.extend(args.iter().map(|a| self.operand(a)));
                let mut ps = vec![I64];
                ps.extend(params.iter().map(|p| cl_type(*p, self.cx.types)));
                let r = (ret != TyId::UNIT).then(|| cl_type(ret, self.cx.types));
                let sig = make_sig(self.cx.call_conv, &ps, r);
                let sig_ref = self.b.import_signature(sig);
                let call = self.b.ins().call_indirect(sig_ref, thunk, &vals);
                match self.b.inst_results(call).first().copied() {
                    Some(v) => self.def(dst, v),
                    None => self.def_zero(dst),
                }
            }
            mir::Rvalue::CallVirtual { trait_id, method, args } => {
                let table = self
                    .cx
                    .meta
                    .traits
                    .iter()
                    .position(|t| *t == trait_id.0)
                    .unwrap_or(0);
                let (m_params, m_ret) = {
                    let def = self.cx.types.trait_def(*trait_id);
                    let m = &def.methods[*method as usize];
                    (m.params.clone(), m.ret)
                };
                let recv = self.operand(&args[0]);
                let lk_args = [recv, self.iconst(table as i64), self.iconst(*method as i64)];
                let addr = self.call_rt("kite_rt_virtual_lookup", &lk_args).unwrap();
                // The receiver is re-read rather than reused: the lookup was
                // a safepoint, and only a variable's reload survives one.
                let mut vals = vec![self.operand(&args[0])];
                vals.extend(args[1..].iter().map(|a| self.operand(a)));
                let mut ps = vec![I64];
                ps.extend(m_params.iter().map(|p| cl_type(*p, self.cx.types)));
                let r = (m_ret != TyId::UNIT).then(|| cl_type(m_ret, self.cx.types));
                let sig = make_sig(self.cx.call_conv, &ps, r);
                let sig_ref = self.b.import_signature(sig);
                let call = self.b.ins().call_indirect(sig_ref, addr, &vals);
                match self.b.inst_results(call).first().copied() {
                    Some(v) => self.def(dst, v),
                    None => self.def_zero(dst),
                }
            }
            mir::Rvalue::CallBuiltin { builtin, args } => self.builtin(dst, *builtin, args),
            mir::Rvalue::CallExtern { index, args } => {
                self.stage(args);
                let a = [self.iconst(*index as i64), self.iconst(args.len() as i64)];
                let v = self.call_rt("kite_rt_call_extern", &a).unwrap();
                let ty = self.local_ty(dst);
                let v = self.word_as(v, ty);
                self.def(dst, v);
            }
            mir::Rvalue::ToStr { operand, from } => {
                let v = match self.cx.types.kind(*from) {
                    TyKind::Int => {
                        let x = self.operand(operand);
                        self.call_rt("kite_rt_str_of_int", &[x]).unwrap()
                    }
                    TyKind::Float => {
                        let x = self.operand(operand);
                        self.call_rt("kite_rt_str_of_float", &[x]).unwrap()
                    }
                    TyKind::Bool => {
                        let x = self.operand(operand);
                        self.call_rt("kite_rt_str_of_bool", &[x]).unwrap()
                    }
                    TyKind::Str => self.operand(operand),
                    _ => {
                        let x = self.operand_word(operand);
                        self.call_rt("kite_rt_str_of_ref", &[x]).unwrap()
                    }
                };
                self.def(dst, v);
            }
            mir::Rvalue::StrOp { op, args } => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                let name = match op {
                    StrKind::Len => "kite_rt_str_len",
                    StrKind::Slice => "kite_rt_str_slice",
                    StrKind::IndexOf => "kite_rt_str_index_of",
                    StrKind::Trim => "kite_rt_str_trim",
                    StrKind::CodeAt => "kite_rt_str_code_at",
                };
                let v = self.call_rt(name, &vals).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::Cast { operand, from, to } => {
                let v = self.operand(operand);
                let r = if *to == TyId::FLOAT {
                    if *from == TyId::FLOAT {
                        v
                    } else {
                        self.b.ins().fcvt_from_sint(F64, v)
                    }
                } else if *from == TyId::INT {
                    v
                } else {
                    // Towards zero, saturating, zero for NaN — the rule the
                    // VM and Wasm's `trunc_sat` already agree on.
                    self.b.ins().fcvt_to_sint_sat(I64, v)
                };
                self.def(dst, r);
            }
            mir::Rvalue::StructNew { struct_id, fields } => {
                self.stage(fields);
                let a = [self.iconst(struct_id.0 as i64), self.iconst(fields.len() as i64)];
                let v = self.call_rt("kite_rt_struct_new", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::EnumNew { enum_id, variant, fields } => {
                self.stage(fields);
                let a = [
                    self.iconst(enum_id.0 as i64),
                    self.iconst(*variant as i64),
                    self.iconst(fields.len() as i64),
                ];
                let v = self.call_rt("kite_rt_enum_new", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::TupleNew { elems } => {
                let shape = self.cx.meta.tuple_shape[&self.local_ty(dst)];
                self.stage(elems);
                let a = [self.iconst(shape as i64), self.iconst(elems.len() as i64)];
                let v = self.call_rt("kite_rt_tuple_new", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::SliceNew { elems } => {
                let elem_kind = match self.cx.types.kind(self.local_ty(dst)) {
                    TyKind::Slice(e) => self.kind(*e),
                    _ => kite_rt::kind::REF,
                };
                self.stage(elems);
                let a = [self.iconst(elem_kind as i64), self.iconst(elems.len() as i64)];
                let v = self.call_rt("kite_rt_slice_new", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::MapNew { entries } => {
                let (kk, vk) = match self.cx.types.kind(self.local_ty(dst)) {
                    TyKind::Map(k, v) => (self.kind(*k), self.kind(*v)),
                    _ => (kite_rt::kind::REF, kite_rt::kind::REF),
                };
                self.stage(entries);
                let a = [
                    self.iconst(kk as i64),
                    self.iconst(vk as i64),
                    self.iconst(entries.len() as i64),
                ];
                let v = self.call_rt("kite_rt_map_new", &a).unwrap();
                self.def(dst, v);
            }
            // Struct fields, tuple elements and known-variant payloads all
            // sit at the same offsets past the header, so one load shape
            // serves all three — the runtime's layout was chosen for exactly
            // this.
            mir::Rvalue::FieldGet { base, index }
            | mir::Rvalue::VariantGet { base, index, .. } => {
                let obj = self.operand(base);
                let ty = cl_type(self.local_ty(dst), self.cx.types);
                let off = 16 + 8 * (*index as i32);
                let v = self.b.ins().load(ty, MemFlagsData::trusted(), obj, off);
                self.def(dst, v);
            }
            mir::Rvalue::TagOf { base } => {
                let obj = self.operand(base);
                let v = self.b.ins().load(types::I32, MemFlagsData::trusted(), obj, 8);
                let v = self.b.ins().uextend(I64, v);
                self.def(dst, v);
            }
            mir::Rvalue::PairNew { value, error } => {
                let vw = self.operand_word(value);
                let vk = self.kind(self.operand_ty(value));
                let e = self.operand(error);
                let a = [vw, self.iconst(vk as i64), e];
                let v = self.call_rt("kite_rt_pair_new", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::PairValue { base } => {
                let obj = self.operand(base);
                let ty = cl_type(self.local_ty(dst), self.cx.types);
                let v = self.b.ins().load(ty, MemFlagsData::trusted(), obj, 16);
                self.def(dst, v);
            }
            mir::Rvalue::PairError { base } => {
                let obj = self.operand(base);
                let v = self.b.ins().load(I64, MemFlagsData::trusted(), obj, 24);
                self.def(dst, v);
            }
            mir::Rvalue::ErrorNew { message } => {
                let m = self.operand(message);
                let v = self.call_rt("kite_rt_error_new", &[m]).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::ErrorMessage { base } => {
                let e = self.operand(base);
                let v = self.call_rt("kite_rt_error_message", &[e]).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::IsNil { value } => {
                let v = self.operand(value);
                let r = self.b.ins().icmp_imm_s(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    v,
                    0,
                );
                self.def(dst, r);
            }
            // An optional is a box, and the box is only there when the
            // payload's own representation could not say `nil` — which, with
            // every payload boxed uniformly, is always. Wrapping a value that
            // is already optional is the flattened `??T = ?T` case: a move.
            mir::Rvalue::Wrap { value } => {
                let src_ty = self.operand_ty(value);
                let dst_ty = self.local_ty(dst);
                if src_ty == dst_ty
                    || matches!(value, mir::Operand::Nil)
                    || matches!(self.cx.types.kind(src_ty), TyKind::Optional(_))
                {
                    let v = self.operand(value);
                    self.def(dst, v);
                } else {
                    let w = self.operand_word(value);
                    let k = self.kind(src_ty);
                    let a = [w, self.iconst(k as i64)];
                    let v = self.call_rt("kite_rt_box_new", &a).unwrap();
                    self.def(dst, v);
                }
            }
            mir::Rvalue::Unwrap { value } => {
                let src_ty = self.operand_ty(value);
                let dst_ty = self.local_ty(dst);
                if src_ty == dst_ty || !matches!(self.cx.types.kind(src_ty), TyKind::Optional(_)) {
                    let v = self.operand(value);
                    self.def(dst, v);
                } else {
                    let obj = self.operand(value);
                    let ty = cl_type(dst_ty, self.cx.types);
                    let v = self.b.ins().load(ty, MemFlagsData::trusted(), obj, 16);
                    self.def(dst, v);
                }
            }
            mir::Rvalue::IndexGet { base, index } => {
                let s = self.operand(base);
                let i = self.operand(index);
                let w = self.call_rt("kite_rt_index_get", &[s, i]).unwrap();
                let ty = self.local_ty(dst);
                let v = self.word_as(w, ty);
                self.def(dst, v);
            }
            mir::Rvalue::SliceLen { base } => {
                let s = self.operand(base);
                let v = self.b.ins().load(I64, MemFlagsData::trusted(), s, 8);
                self.def(dst, v);
            }
            mir::Rvalue::SliceGet { base, index } => {
                let elem = match self.cx.types.kind(self.operand_ty(base)) {
                    TyKind::Slice(e) => *e,
                    _ => TyId::ERR,
                };
                let wrap = self.wrap_kind(elem);
                let s = self.operand(base);
                let i = self.operand(index);
                let a = [s, i, self.iconst(wrap)];
                let v = self.call_rt("kite_rt_slice_get", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::MapLen { base } => {
                let m = self.operand(base);
                let v = self.b.ins().load(I64, MemFlagsData::trusted(), m, 8);
                self.def(dst, v);
            }
            mir::Rvalue::MapGet { base, key } => {
                let (key_ty, val_ty) = match self.cx.types.kind(self.operand_ty(base)) {
                    TyKind::Map(k, v) => (*k, *v),
                    _ => (TyId::ERR, TyId::ERR),
                };
                let wrap = self.wrap_kind(val_ty);
                let m = self.operand(base);
                let kw = self.operand_word(key);
                let kk = self.iconst(self.kind(key_ty) as i64);
                let a = [m, kw, kk, self.iconst(wrap)];
                let v = self.call_rt("kite_rt_map_get", &a).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::MapKeys { base } => {
                let m = self.operand(base);
                let v = self.call_rt("kite_rt_map_keys", &[m]).unwrap();
                self.def(dst, v);
            }
            mir::Rvalue::MapValues { base } => {
                let m = self.operand(base);
                let v = self.call_rt("kite_rt_map_values", &[m]).unwrap();
                self.def(dst, v);
            }
        }
    }

    /// How a `.get()`-style optional result is built: boxed with the
    /// element's kind, or passed through when the element is itself optional
    /// — `?T` flattens, so the stored reference already is the answer.
    fn wrap_kind(&self, elem: TyId) -> i64 {
        if matches!(self.cx.types.kind(elem), TyKind::Optional(_)) {
            -1
        } else {
            self.kind(elem) as i64
        }
    }

    fn binary(&mut self, dst: mir::Local, op: BinOp, lhs: &mir::Operand, rhs: &mir::Operand) {
        use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
        let a = self.operand(lhs);
        let b = self.operand(rhs);
        let r = match op {
            // Overflow traps in a debug build and wraps in a release one. Which
            // it is was decided in the checker, where the build mode is known,
            // so every backend gets the same answer from the same operation.
            BinOp::AddInt => {
                let (v, of) = self.b.ins().sadd_overflow(a, b);
                self.trap_if(of, trap_code::OVERFLOW_ADD, 0, 0);
                v
            }
            BinOp::SubInt => {
                let (v, of) = self.b.ins().ssub_overflow(a, b);
                self.trap_if(of, trap_code::OVERFLOW_SUB, 0, 0);
                v
            }
            BinOp::MulInt => {
                let (v, of) = self.b.ins().smul_overflow(a, b);
                self.trap_if(of, trap_code::OVERFLOW_MUL, 0, 0);
                v
            }
            BinOp::AddIntWrap => self.b.ins().iadd(a, b),
            BinOp::SubIntWrap => self.b.ins().isub(a, b),
            BinOp::MulIntWrap => self.b.ins().imul(a, b),
            BinOp::DivInt => {
                self.div_guards(a, b, trap_code::OVERFLOW_DIV);
                self.b.ins().sdiv(a, b)
            }
            BinOp::RemInt => {
                self.div_guards(a, b, trap_code::OVERFLOW_REM);
                self.b.ins().srem(a, b)
            }
            BinOp::AddFloat => self.b.ins().fadd(a, b),
            BinOp::SubFloat => self.b.ins().fsub(a, b),
            BinOp::MulFloat => self.b.ins().fmul(a, b),
            // IEEE-754 division by zero yields an infinity, and does not
            // trap. Only integer division does.
            BinOp::DivFloat => self.b.ins().fdiv(a, b),
            BinOp::ConcatStr => self.call_rt("kite_rt_str_concat", &[a, b]).unwrap(),
            BinOp::BitAnd => self.b.ins().band(a, b),
            BinOp::BitOr => self.b.ins().bor(a, b),
            BinOp::BitXor => self.b.ins().bxor(a, b),
            BinOp::Shl | BinOp::Shr => {
                let code = if op == BinOp::Shl {
                    trap_code::OVERFLOW_SHL
                } else {
                    trap_code::OVERFLOW_SHR
                };
                // The VM refuses a shift outside 0..64 rather than masking.
                let lo = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, b, 0);
                self.trap_if(lo, code, 0, 0);
                let hi = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, b, 64);
                self.trap_if(hi, code, 0, 0);
                if op == BinOp::Shl {
                    self.b.ins().ishl(a, b)
                } else {
                    self.b.ins().sshr(a, b)
                }
            }
            BinOp::EqInt | BinOp::EqBool => self.b.ins().icmp(IntCC::Equal, a, b),
            BinOp::NeInt | BinOp::NeBool => self.b.ins().icmp(IntCC::NotEqual, a, b),
            BinOp::LtInt => self.b.ins().icmp(IntCC::SignedLessThan, a, b),
            BinOp::LeInt => self.b.ins().icmp(IntCC::SignedLessThanOrEqual, a, b),
            BinOp::GtInt => self.b.ins().icmp(IntCC::SignedGreaterThan, a, b),
            BinOp::GeInt => self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, a, b),
            BinOp::EqFloat => self.b.ins().fcmp(FloatCC::Equal, a, b),
            BinOp::NeFloat => self.b.ins().fcmp(FloatCC::NotEqual, a, b),
            BinOp::LtFloat => self.b.ins().fcmp(FloatCC::LessThan, a, b),
            BinOp::LeFloat => self.b.ins().fcmp(FloatCC::LessThanOrEqual, a, b),
            BinOp::GtFloat => self.b.ins().fcmp(FloatCC::GreaterThan, a, b),
            BinOp::GeFloat => self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, a, b),
            BinOp::EqStr => self.call_rt("kite_rt_str_eq", &[a, b]).unwrap(),
            BinOp::NeStr => {
                let eq = self.call_rt("kite_rt_str_eq", &[a, b]).unwrap();
                self.b.ins().bxor_imm_s(eq, 1)
            }
            BinOp::LtStr | BinOp::LeStr | BinOp::GtStr | BinOp::GeStr => {
                let ord = self.call_rt("kite_rt_str_compare", &[a, b]).unwrap();
                let cc = match op {
                    BinOp::LtStr => IntCC::SignedLessThan,
                    BinOp::LeStr => IntCC::SignedLessThanOrEqual,
                    BinOp::GtStr => IntCC::SignedGreaterThan,
                    _ => IntCC::SignedGreaterThanOrEqual,
                };
                self.b.ins().icmp_imm_s(cc, ord, 0)
            }
            BinOp::EqValue | BinOp::NeValue => {
                // Structural comparison. The kind comes from whichever side
                // knows its type; `nil` alone knows only that it is a
                // reference.
                let ty = match lhs {
                    mir::Operand::Nil => self.operand_ty(rhs),
                    _ => self.operand_ty(lhs),
                };
                let k = self.kind(ty);
                let aw = self.as_word(a);
                let bw = self.as_word(b);
                let kk = self.iconst(k as i64);
                let eq = self.call_rt("kite_rt_value_eq", &[aw, bw, kk]).unwrap();
                if op == BinOp::NeValue {
                    self.b.ins().bxor_imm_s(eq, 1)
                } else {
                    eq
                }
            }
            // MIR has already lowered && and || to branches, so reaching
            // here would be a lowering bug.
            BinOp::And | BinOp::Or => {
                self.b.ins().trap(TrapCode::user(2).unwrap());
                return;
            }
        };
        self.def(dst, r);
    }

    fn div_guards(&mut self, a: Value, b: Value, overflow_code: i64) {
        use cranelift_codegen::ir::condcodes::IntCC;
        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
        self.trap_if(zero, trap_code::DIV_ZERO, 0, 0);
        let min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
        let m1 = self.b.ins().icmp_imm_s(IntCC::Equal, b, -1);
        let both = self.b.ins().band(min, m1);
        self.trap_if(both, overflow_code, 0, 0);
    }

    fn builtin(&mut self, dst: mir::Local, builtin: Builtin, args: &[mir::Operand]) {
        match builtin {
            Builtin::IoError => {
                if args.is_empty() {
                    self.call_rt("kite_rt_error_unit", &[]);
                } else {
                    let ty = self.operand_ty(&args[0]);
                    let v = self.operand(&args[0]);
                    match self.kind(ty) {
                        kite_rt::kind::INT => self.call_rt("kite_rt_error_int", &[v]),
                        kite_rt::kind::FLOAT => self.call_rt("kite_rt_error_float", &[v]),
                        kite_rt::kind::BOOL => self.call_rt("kite_rt_error_bool", &[v]),
                        _ if ty == TyId::STR => self.call_rt("kite_rt_error_str", &[v]),
                        _ => self.call_rt("kite_rt_error_str", &[v]),
                    };
                }
                self.def_zero(dst);
            }
            Builtin::IoReadLine => {
                let r = self.call_rt("kite_rt_read_line", &[]).unwrap();
                self.def(dst, r);
            }
            Builtin::IoPrint => {
                if args.is_empty() {
                    self.call_rt("kite_rt_print_unit", &[]);
                } else {
                    let ty = self.operand_ty(&args[0]);
                    let v = self.operand(&args[0]);
                    match self.kind(ty) {
                        kite_rt::kind::INT => self.call_rt("kite_rt_print_int", &[v]),
                        kite_rt::kind::FLOAT => self.call_rt("kite_rt_print_float", &[v]),
                        kite_rt::kind::BOOL => self.call_rt("kite_rt_print_bool", &[v]),
                        kite_rt::kind::UNIT => self.call_rt("kite_rt_print_unit", &[]),
                        _ if ty == TyId::STR => self.call_rt("kite_rt_print_str", &[v]),
                        _ => self.call_rt("kite_rt_print_ref", &[v]),
                    };
                }
                self.def_zero(dst);
            }
            Builtin::Require => {
                let cond = self.operand(&args[0]);
                let msg = match args.get(1) {
                    Some(m) => self.operand(m),
                    None => self.iconst(0),
                };
                self.call_rt("kite_rt_require", &[cond, msg]);
                self.def_zero(dst);
            }
            Builtin::DrawRect => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_rect", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawRRect => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_rrect", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawText => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_text", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawFont => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_font", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawDRRect => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_drrect", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawAlpha => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_alpha", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawClip => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_clip", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawUnclip => {
                self.call_rt("kite_rt_draw_unclip", &[]);
                self.def_zero(dst);
            }
            Builtin::DrawField => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_field", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawImage => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_image", &vals);
                self.def_zero(dst);
            }
            Builtin::DrawSemantics => {
                let vals: Vec<Value> = args.iter().map(|a| self.operand(a)).collect();
                self.call_rt("kite_rt_draw_semantics", &vals);
                self.def_zero(dst);
            }
            Builtin::TextWidth => {
                let v = self.operand(&args[0]);
                let r = self.call_rt("kite_rt_text_width", &[v]).unwrap();
                self.def(dst, r);
            }
            Builtin::TextHeight => {
                let r = self.call_rt("kite_rt_text_height", &[]).unwrap();
                self.def(dst, r);
            }
            Builtin::TextFromCode => {
                let v = self.operand(&args[0]);
                let r = self.call_rt("kite_rt_text_from_code", &[v]).unwrap();
                self.def(dst, r);
            }
            Builtin::TaskSpawn => {
                let v = self.operand(&args[0]);
                self.call_rt("kite_rt_task_spawn", &[v]);
                self.def_zero(dst);
            }
            Builtin::TaskWakeAt => {
                let v = self.operand(&args[0]);
                self.call_rt("kite_rt_task_wake_at", &[v]);
                self.def_zero(dst);
            }
            Builtin::TaskPark => {
                self.call_rt("kite_rt_task_park", &[]);
                self.def_zero(dst);
            }
            Builtin::TaskWaitHost => {
                self.call_rt("kite_rt_task_wait_host", &[]);
                self.def_zero(dst);
            }
            Builtin::TimeNow => {
                let r = self.call_rt("kite_rt_time_now", &[]).unwrap();
                self.def(dst, r);
            }
            // A struct, an enum and a map are each a pointer here, so identity
            // is the same `icmp` that `==` on two ints compiles to — and it
            // yields an `i8`, which is what a `bool` is on this backend.
            Builtin::PtrSame => {
                use cranelift_codegen::ir::condcodes::IntCC;
                let a = self.operand(&args[0]);
                let b = self.operand(&args[1]);
                let r = self.b.ins().icmp(IntCC::Equal, a, b);
                self.def(dst, r);
            }
        }
    }

    // ---- terminators -----------------------------------------------------

    fn terminator(&mut self, t: &mir::Terminator, block_index: usize) {
        match t {
            mir::Terminator::Goto(target) => {
                let bb = self.blocks[target.index()];
                self.b.ins().jump(bb, &[]);
            }
            mir::Terminator::Branch { cond, then, else_ } => {
                let c = self.operand(cond);
                let tb = self.blocks[then.index()];
                let eb = self.blocks[else_.index()];
                self.b.ins().brif(c, tb, &[], eb, &[]);
            }
            mir::Terminator::Return(v) => {
                if self.f.ret == TyId::UNIT {
                    self.b.ins().return_(&[]);
                } else {
                    let val = match v {
                        Some(o) => self.operand(o),
                        None => {
                            let ty = cl_type(self.f.ret, self.cx.types);
                            if ty == F64 {
                                self.b.ins().f64const(0.0)
                            } else {
                                self.b.ins().iconst(ty, 0)
                            }
                        }
                    };
                    self.b.ins().return_(&[val]);
                }
            }
            mir::Terminator::Unreachable => {
                let args = [
                    self.iconst(trap_code::UNREACHABLE),
                    self.iconst(self.fn_index as i64),
                    self.iconst(block_index as i64),
                ];
                self.call_rt("kite_rt_trap", &args);
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Compile to a relocatable object file, for the linker.
/// Whether this host can run the native backend at all.
///
/// **Windows x86-64 cannot, yet, and the reason is the collector rather than
/// the code generator.** Roots are found by walking the frame-pointer chain,
/// and that walk assumes the caller's stack pointer at a call is the frame
/// record's address plus sixteen — which holds on the System V ABI and on
/// AArch64, and is what makes macOS and Linux work. Cranelift's Win64
/// prologue establishes the frame pointer differently, so the offsets the
/// stack maps are relative to do not land where the walk expects, and the
/// collector traces a stack word that was never a reference. It shows up as
/// a corrupted heap under a small nursery, which is exactly the failure a
/// precise collector must never have.
///
/// Refusing is the honest answer until someone with a Windows machine can
/// read the prologue and fix the offset. A backend that emitted code which
/// corrupts memory on one in three platforms would be worse than one that
/// says where it does not work.
pub fn supported_here() -> Result<(), String> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        return Err(
            "the native backend does not support Windows yet: the collector finds roots by \
             walking frame pointers, and Cranelift's Win64 prologue puts the frame record \
             where that walk does not expect it\n\
             note: the bytecode and WebAssembly targets work here — run without `--native`"
                .to_string(),
        );
    }
    Ok(())
}

pub fn compile_object(program: &mir::Program, types: &Types) -> Result<Vec<u8>, String> {
    supported_here()?;
    let isa = host_isa(true)?;
    let builder = cranelift_object::ObjectBuilder::new(
        isa,
        "kite",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| e.to_string())?;
    let mut module = cranelift_object::ObjectModule::new(builder);
    build(&mut module, program, types)?;
    module.finish().emit().map_err(|e| e.to_string())
}

/// Compile into this process and run to completion, writing the program's
/// output to `out`. This is `kitec run --native`, and it is also how the
/// differential suite runs the corpus without needing a linker.
///
/// A trap ends the process, exactly as it would in a linked executable — the
/// runtime prints the message first, so nothing is quieter than the VM.
pub fn run_jit(program: &mir::Program, types: &Types, out: &mut dyn std::io::Write) -> Result<(), String> {
    supported_here()?;
    let _guard = kite_rt::run_lock();
    kite_rt::begin_capture();

    let isa = host_isa(false)?;
    let mut builder = cranelift_jit::JITBuilder::with_isa(
        isa,
        cranelift_module::default_libcall_names(),
    );
    for (name, ptr) in kite_rt::jit_symbols() {
        builder.symbol(name, ptr);
    }
    let mut module = cranelift_jit::JITModule::new(builder);
    let arts = build(&mut module, program, types)?;
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let entry = module.get_finalized_function(arts.wrapper);
    let entry: extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry) };
    entry();
    out.write_all(&kite_rt::take_capture()).map_err(|e| e.to_string())?;
    // The code pages hold nothing live once the run is over; the next
    // startup resets the runtime's pointers into them.
    unsafe { module.free_memory() };
    Ok(())
}
