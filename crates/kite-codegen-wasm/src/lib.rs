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
    ExportSection, FieldType, Function, FunctionSection, HeapType, ImportSection, Instruction,
    Module, RefType, StorageType, SubType, TypeSection, ValType,
};

mod glue;
pub use glue::generate_glue;

/// Host functions the module imports. Deliberately small: the standard library
/// replaces them from Phase 6.
const IMPORTS: [(&str, ValType); 4] = [
    ("print_int", ValType::I64),
    ("print_float", ValType::F64),
    ("print_bool", ValType::I32),
    ("print_str", ValType::I32),
];

const IMPORT_COUNT: u32 = IMPORTS.len() as u32;

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
}

impl TypeLayout {
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
}

/// Compile a MIR program to a WebAssembly module.
pub fn compile(program: &mir::Program, types: &Types) -> WasmModule {
    let mut module = Module::new();

    // Type index space: import signatures, then structs, then each enum's base
    // record followed by its variants, then function signatures.
    let struct_base = IMPORT_COUNT;
    let mut next = struct_base + types.struct_count() as u32;
    let mut enum_base = Vec::with_capacity(types.enum_count());
    for i in 0..types.enum_count() {
        enum_base.push(next);
        next += 1 + types.enum_def(kite_hir::EnumId(i as u32)).variants.len() as u32;
    }
    let aggregate_count = next - IMPORT_COUNT;
    let layout = TypeLayout { struct_base, enum_base };

    // ---- types -------------------------------------------------------------
    let mut type_section = TypeSection::new();
    for (_, param) in IMPORTS.iter() {
        type_section.ty().function([*param], []);
    }

    // Every aggregate goes in one `rec` group: a field may name a type declared
    // later, and mutual recursion has to work — which it must, because every
    // Kite aggregate is a GC reference and recursion needs no annotation.
    if aggregate_count > 0 {
        let mut group: Vec<SubType> = Vec::with_capacity(aggregate_count as usize);

        for i in 0..types.struct_count() {
            let def = types.struct_def(kite_hir::StructId(i as u32));
            let fields: Vec<FieldType> = def
                .fields
                .iter()
                .map(|f| FieldType {
                    element_type: StorageType::Val(val_type_with(f.ty, types, &layout)),
                    // Kite's per-field `var` marker is exactly WasmGC's
                    // per-field mutability flag. Immutable fields let the
                    // engine hoist loads without alias analysis.
                    mutable: f.mutable,
                })
                .collect();
            group.push(struct_subtype(fields, None, true));
        }

        // An enum becomes a base record holding just the tag, plus one subtype
        // per variant carrying its payload. A `match` reads the tag, and a
        // payload read casts to the variant it has already established.
        for i in 0..types.enum_count() {
            let eid = kite_hir::EnumId(i as u32);
            let base = layout.enum_base_type(eid);
            group.push(struct_subtype(
                vec![FieldType {
                    element_type: StorageType::Val(ValType::I32),
                    mutable: false,
                }],
                None,
                false,
            ));
            for variant in &types.enum_def(eid).variants {
                let mut fields = vec![FieldType {
                    element_type: StorageType::Val(ValType::I32),
                    mutable: false,
                }];
                for f in &variant.fields {
                    fields.push(FieldType {
                        element_type: StorageType::Val(val_type_with(f.ty, types, &layout)),
                        mutable: false,
                    });
                }
                group.push(struct_subtype(fields, Some(base), true));
            }
        }

        type_section.ty().rec(group);
    }

    // Indices are computed rather than read back: `TypeSection::len` counts a
    // `rec` group as one entry, not as the types inside it, so trusting it here
    // would collide function types with every struct after the first.
    let fn_type_base = IMPORT_COUNT + aggregate_count;
    let mut fn_type_index = Vec::with_capacity(program.fns.len());
    for (i, f) in program.fns.iter().enumerate() {
        let params: Vec<ValType> = (0..f.param_count)
            .map(|j| val_type_with(f.locals[j].ty, types, &layout))
            .collect();
        let results: Vec<ValType> = wasm_result_with(f.ret, types, &layout).into_iter().collect();
        fn_type_index.push(fn_type_base + i as u32);
        type_section.ty().function(params, results);
    }
    module.section(&type_section);

    // ---- imports -----------------------------------------------------------
    let mut imports = ImportSection::new();
    for (i, (name, _)) in IMPORTS.iter().enumerate() {
        imports.import("kite", name, EntityType::Function(i as u32));
    }
    module.section(&imports);

    // ---- functions ---------------------------------------------------------
    let mut functions = FunctionSection::new();
    for idx in &fn_type_index {
        functions.function(*idx);
    }
    module.section(&functions);

    // ---- exports -----------------------------------------------------------
    let mut exports = ExportSection::new();
    for (i, f) in program.fns.iter().enumerate() {
        exports.export(&f.name, ExportKind::Func, IMPORT_COUNT + i as u32);
    }
    module.section(&exports);

    // ---- code --------------------------------------------------------------
    // A call's result type decides whether the value has to be stored, so the
    // whole table is needed before any body is emitted.
    let fn_returns: Vec<TyId> = program.fns.iter().map(|f| f.ret).collect();
    let mut code = CodeSection::new();
    for f in &program.fns {
        code.function(&compile_fn(f, types, &fn_returns, &layout));
    }
    module.section(&code);

    WasmModule {
        bytes: module.finish(),
        strings: program.strings.clone(),
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
        // `str` is a constant index for now; with JS String Builtins this
        // becomes `externref` carrying the JS string with no copy.
        _ => ValType::I32,
    }
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
) -> Function {
    // Locals beyond the parameters, plus one synthetic program counter.
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for l in f.locals.iter().skip(f.param_count) {
        push_local(&mut locals, val_type_with(l.ty, types, layout));
    }
    push_local(&mut locals, ValType::I32); // $pc
    let pc = f.locals.len() as u32;

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
            layout,
            pc,
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
    layout: &'a TypeLayout,
    pc: u32,
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
        self.terminator(func, &block.term);
    }

    fn stmt(&mut self, func: &mut Function, stmt: &mir::Inst) {
        match stmt {
            mir::Inst::Assign { dst, value } => {
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
                    field_index: *index,
                });
            }
            // Slices are not lowered yet.
            mir::Inst::SetIndex { .. } | mir::Inst::SlicePush { .. } => {
                func.instruction(&Instruction::Unreachable);
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
                self.binop(func, *op);
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
                func.instruction(&Instruction::Call(IMPORT_COUNT + callee.0));
                return self.fn_returns[callee.index()] != TyId::UNIT;
            }

            // Every builtin returns unit today.
            mir::Rvalue::CallBuiltin { builtin, args } => {
                self.builtin(func, *builtin, args);
                return false;
            }

            mir::Rvalue::StructNew { struct_id, fields } => {
                for f in fields {
                    self.operand(func, f);
                }
                func.instruction(&Instruction::StructNew(self.layout.struct_type(*struct_id)));
                return true;
            }

            mir::Rvalue::FieldGet { base, index } => {
                let Some(sid) = self.struct_of(base) else {
                    func.instruction(&Instruction::Unreachable);
                    return true;
                };
                self.operand(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: self.layout.struct_type(sid),
                    field_index: *index,
                });
                return true;
            }

            mir::Rvalue::EnumNew { enum_id, variant, fields } => {
                // Field 0 is the tag; the payload follows.
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
                    field_index: 0,
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
                    // Field 0 is the tag, so the payload starts at 1.
                    field_index: index + 1,
                });
                return true;
            }

            // Enums, slices, and fallible pairs are not lowered yet.
            // `unreachable` makes the stack polymorphic, so reporting a value
            // keeps the surrounding code well-typed.
            _ => {
                func.instruction(&Instruction::Unreachable);
                return true;
            }
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
                    3
                } else {
                    match self.operand_type(arg) {
                        ValType::I64 => 0,
                        ValType::F64 => 1,
                        _ => 2,
                    }
                };
                self.operand(func, arg);
                func.instruction(&Instruction::Call(import));
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
            mir::Operand::Unit | mir::Operand::Nil => {
                func.instruction(&Instruction::I32Const(0));
            }
        }
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
            // Not lowered yet.
            EqStr | NeStr | ConcatStr | EqValue | NeValue => Instruction::Unreachable,
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
                    let usable = matches!(
                        v,
                        Some(mir::Operand::Local(_))
                            | Some(mir::Operand::Int(_))
                            | Some(mir::Operand::Float(_))
                            | Some(mir::Operand::Bool(_))
                            | Some(mir::Operand::Str(_))
                    );
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
