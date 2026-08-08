//! The language-owned WebAssembly string runtime.
//!
//! A `str` is an array of Unicode scalar values (`i32` code points). All
//! language operations stay in Wasm; only `from_host` and `to_host` cross the
//! JavaScript boundary, a chunk at a time through one imported memory page.

use crate::*;

/// Four bytes per scalar and sixteen KiB per crossing call. This leaves most
/// of the fixed 64 KiB scratch page unused and keeps `String.fromCodePoint`'s
/// argument list comfortably below engine limits.
const SCALAR_CHUNK: i32 = 4096;

pub const FUNCTION_COUNT: usize = 12;

// Slots, in emission order. The order is the contract between [`add_types`],
// [`emit`] and [`Needed`]: all three walk it, so a function added here is
// added to each of them or none.
const FROM_HOST: usize = 0;
const TO_HOST: usize = 1;
const CONCAT: usize = 2;
const EQ: usize = 3;
const COMPARE: usize = 4;
const LEN: usize = 5;
const SLICE: usize = 6;
const INDEX_OF: usize = 7;
const TRIM: usize = 8;
const CODE_AT: usize = 9;
const FROM_CODE: usize = 10;
const IS_SPACE: usize = 11;

const NAMES: [&str; FUNCTION_COUNT] = [
    "from_host",
    "to_host",
    "concat",
    "eq",
    "compare",
    "len",
    "slice",
    "index_of",
    "trim",
    "code_at",
    "from_code",
    "is_space",
];

/// A slot the module does not contain.
const ABSENT: u32 = u32::MAX;

/// Which runtime functions a program can reach.
///
/// The whole runtime used to go into every module, so a program that never
/// handled text still paid for `slice`, `index_of` and the rest: a module
/// containing no strings at all was 1,619 bytes against a printing program's
/// 1,625, which is the cost of the runtime being *present* rather than used.
///
/// Answered conservatively. Marking a function that is never called wastes a
/// hundred bytes; failing to mark one that is called would emit a call to a
/// function that is not there, so where the analysis cannot be exact it
/// over-declares — and [`StringRuntime::at`] asserts rather than defaulting,
/// so a miss is a loud panic in this compiler's own tests rather than a module
/// that will not instantiate.
#[derive(Clone, Copy, Default)]
pub struct Needed([bool; FUNCTION_COUNT]);

impl Needed {
    fn mark(&mut self, slot: usize) {
        self.0[slot] = true;
    }

    fn has(self, slot: usize) -> bool {
        self.0[slot]
    }

    pub fn count(self) -> u32 {
        self.0.iter().filter(|on| **on).count() as u32
    }
}

/// Function indices of the string runtime, by slot.
///
/// Sparse: a slot the module does not contain holds [`ABSENT`], and asking for
/// it is a bug in the analysis rather than something to paper over.
#[derive(Clone, Copy)]
pub struct StringRuntime {
    slots: [u32; FUNCTION_COUNT],
}

impl StringRuntime {
    /// Lay the needed functions out consecutively from `base`.
    pub fn new(base: u32, needed: Needed) -> StringRuntime {
        let mut slots = [ABSENT; FUNCTION_COUNT];
        let mut next = base;
        for (slot, on) in needed.0.iter().enumerate() {
            if *on {
                slots[slot] = next;
                next += 1;
            }
        }
        StringRuntime { slots }
    }

    fn at(self, slot: usize) -> u32 {
        assert!(
            self.slots[slot] != ABSENT,
            "the string runtime's `{}` is called but was not emitted — `strings::needed` \
             did not see the call that reaches it",
            NAMES[slot]
        );
        self.slots[slot]
    }

    pub fn from_host(self) -> u32 {
        self.at(FROM_HOST)
    }

    pub fn to_host(self) -> u32 {
        self.at(TO_HOST)
    }

    pub fn concat(self) -> u32 {
        self.at(CONCAT)
    }

    pub fn eq(self) -> u32 {
        self.at(EQ)
    }

    pub fn compare(self) -> u32 {
        self.at(COMPARE)
    }

    pub fn len(self) -> u32 {
        self.at(LEN)
    }

    pub fn slice(self) -> u32 {
        self.at(SLICE)
    }

    pub fn index_of(self) -> u32 {
        self.at(INDEX_OF)
    }

    pub fn trim(self) -> u32 {
        self.at(TRIM)
    }

    pub fn code_at(self) -> u32 {
        self.at(CODE_AT)
    }

    pub fn from_code(self) -> u32 {
        self.at(FROM_CODE)
    }

    fn is_space(self) -> u32 {
        self.at(IS_SPACE)
    }
}

/// Whether an operand is a `str`, for deciding how a map compares its keys.
fn operand_is_str(f: &mir::Function, o: &mir::Operand) -> bool {
    match o {
        mir::Operand::Str(_) => true,
        mir::Operand::Local(l) => f.locals[l.index()].ty == TyId::STR,
        _ => false,
    }
}

/// Which runtime functions the module must contain.
///
/// `from_host` and `to_host` are unconditional: the glue exports `str` and
/// `text` for the module's typed JavaScript API whether or not the program
/// itself handles text, and those exports call these two.
pub fn needed(program: &mir::Program, types: &Types, has_eq_fns: bool) -> Needed {
    let mut needed = Needed::default();
    needed.mark(FROM_HOST);
    needed.mark(TO_HOST);

    // A generated deep-equality function compares a `str` field by calling
    // `eq`, and whether any of them reaches one is a walk of the type graph
    // this does not do. Any generated equality at all marks it: the programs
    // this costs are ones already comparing aggregates, which are not the
    // small modules the analysis exists for.
    if has_eq_fns {
        needed.mark(EQ);
    }

    // A literal too long for one `array.new_fixed` is built from chunks and
    // joined, so a program with no `+` in it can still need `concat`.
    if program
        .strings
        .iter()
        .any(|s| s.chars().count() > crate::STR_LITERAL_CHUNK)
    {
        needed.mark(CONCAT);
    }

    for f in &program.fns {
        for b in &f.blocks {
            for s in &b.stmts {
                if let mir::Inst::MapSet { local, key, .. } = s {
                    let _ = local;
                    if operand_is_str(f, key) {
                        needed.mark(EQ);
                    }
                }
                let mir::Inst::Assign { value, .. } = s else {
                    continue;
                };
                match value {
                    mir::Rvalue::Binary { op, .. } => match op {
                        BinOp::ConcatStr => needed.mark(CONCAT),
                        BinOp::EqStr | BinOp::NeStr => needed.mark(EQ),
                        BinOp::LtStr | BinOp::LeStr | BinOp::GtStr | BinOp::GeStr => {
                            needed.mark(COMPARE)
                        }
                        _ => {}
                    },
                    mir::Rvalue::StrOp { op, .. } => match op {
                        kite_hir::StrKind::Len => needed.mark(LEN),
                        kite_hir::StrKind::Slice => needed.mark(SLICE),
                        kite_hir::StrKind::IndexOf => needed.mark(INDEX_OF),
                        kite_hir::StrKind::Trim => needed.mark(TRIM),
                        kite_hir::StrKind::CodeAt => needed.mark(CODE_AT),
                    },
                    mir::Rvalue::CallBuiltin { builtin, .. } => {
                        if matches!(builtin, Builtin::TextFromCode) {
                            needed.mark(FROM_CODE);
                        }
                    }
                    // A map compares its keys on every read and every write.
                    mir::Rvalue::MapGet { key, .. } => {
                        if operand_is_str(f, key) {
                            needed.mark(EQ);
                        }
                    }
                    mir::Rvalue::MapNew { entries } => {
                        if entries.iter().step_by(2).any(|k| operand_is_str(f, k)) {
                            needed.mark(EQ);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = types;

    // `trim` is the one runtime function that calls another.
    if needed.has(TRIM) {
        needed.mark(IS_SPACE);
    }
    needed
}

fn str_type(layout: &TypeLayout) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(layout.str_array),
    })
}

/// Declare the signatures of the needed runtime functions, and return their
/// type indices in function emission order.
///
/// Only the needed ones are declared, so the returned vector is as long as the
/// module's runtime rather than always twelve.
pub fn add_types(
    section: &mut TypeSection,
    base: u32,
    layout: &TypeLayout,
    needed: Needed,
) -> Vec<u32> {
    let text = str_type(layout);
    let signatures = [
        (vec![EXTERN_REF_NULL], vec![text]),
        (vec![text], vec![EXTERN_REF_NULL]),
        (vec![text, text], vec![text]),
        (vec![text, text], vec![ValType::I32]),
        (vec![text, text], vec![ValType::I64]),
        (vec![text], vec![ValType::I64]),
        (vec![text, ValType::I64, ValType::I64], vec![text]),
        (vec![text, text], vec![ValType::I64]),
        (vec![text], vec![text]),
        (vec![text, ValType::I64], vec![ValType::I64]),
        (vec![ValType::I64], vec![text]),
        (vec![ValType::I32], vec![ValType::I32]),
    ];
    let mut indices = Vec::with_capacity(needed.count() as usize);
    let mut next = base;
    for (slot, (params, results)) in signatures.into_iter().enumerate() {
        if !needed.has(slot) {
            continue;
        }
        section.ty().function(params, results);
        indices.push(next);
        next += 1;
    }
    indices
}

/// Emit the needed runtime bodies, in the same order as [`add_types`].
pub fn emit(
    code: &mut CodeSection,
    runtime: StringRuntime,
    layout: &TypeLayout,
    hosts: &Hosts,
    needed: Needed,
) {
    for slot in 0..FUNCTION_COUNT {
        if !needed.has(slot) {
            continue;
        }
        match slot {
            FROM_HOST => code.function(&from_host(layout, hosts)),
            TO_HOST => code.function(&to_host(layout, hosts)),
            CONCAT => code.function(&concat(layout)),
            EQ => code.function(&eq(layout)),
            COMPARE => code.function(&compare(layout)),
            LEN => code.function(&len()),
            SLICE => code.function(&slice(layout)),
            INDEX_OF => code.function(&index_of(layout)),
            TRIM => code.function(&trim(layout, runtime)),
            CODE_AT => code.function(&code_at(layout)),
            FROM_CODE => code.function(&from_code(layout)),
            _ => code.function(&is_space()),
        };
    }
}

/// Convert a JavaScript string to a scalar array.
fn from_host(layout: &TypeLayout, hosts: &Hosts) -> Function {
    let text = str_type(layout);
    // 0 host value; 1 answer; 2 length; 3 offset; 4 chunk; 5 cursor.
    let mut f = Function::new(vec![(1, text), (4, ValType::I32)]);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_LEN)));
    f.instruction(&Instruction::LocalTee(2));
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    // chunk = min(length - offset, SCALAR_CHUNK)
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalTee(4));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::Select);
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_FILL)));
    f.instruction(&Instruction::LocalTee(4));
    // A bridge returning no progress would otherwise make this loop eternal.
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    f.instruction(&Instruction::ArraySet(layout.str_array));

    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::End);
    f
}

/// Convert a scalar array to a JavaScript string.
fn to_host(layout: &TypeLayout, hosts: &Hosts) -> Function {
    // 0 text; 1 length; 2 offset; 3 chunk; 4 cursor; 5 answer.
    let mut f = Function::new(vec![(4, ValType::I32), (1, EXTERN_REF_NULL)]);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalTee(1));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_PUSH)));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalTee(3));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::Select);
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Store(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_PUSH)));
    f.instruction(&Instruction::LocalSet(5));

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::End);
    f
}

fn concat(layout: &TypeLayout) -> Function {
    let mut f = Function::new(vec![(1, str_type(layout))]);
    // answer = new array(a.len + b.len)
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(2));

    for (src, dest_offset) in [(0, None), (1, Some(0))] {
        f.instruction(&Instruction::LocalGet(2));
        match dest_offset {
            None => {
                f.instruction(&Instruction::I32Const(0));
            }
            Some(first) => {
                f.instruction(&Instruction::LocalGet(first));
                f.instruction(&Instruction::ArrayLen);
            }
        };
        f.instruction(&Instruction::LocalGet(src));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(src));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: layout.str_array,
            array_type_index_src: layout.str_array,
        });
    }
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
}

fn eq(layout: &TypeLayout) -> Function {
    // 0 and 1 strings; 2 cursor.
    let mut f = Function::new(vec![(1, ValType::I32)]);
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
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::End);
    f
}

fn compare(layout: &TypeLayout) -> Function {
    // 0 and 1 strings; 2 cursor; 3 and 4 current scalars.
    let mut f = Function::new(vec![(3, ValType::I32)]);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::I64Const(1));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::End);
    f
}

fn len() -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::End);
    f
}

fn slice(layout: &TypeLayout) -> Function {
    // 0 text; 1 from; 2 to; 3 start; 4 end; 5 answer.
    let mut f = Function::new(vec![(2, ValType::I32), (1, str_type(layout))]);

    // start = clamp(from, 0, length)
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalSet(3));

    // end = clamp(to, start, length)
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalSet(4));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: layout.str_array,
        array_type_index_src: layout.str_array,
    });
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::End);
    f
}

fn index_of(layout: &TypeLayout) -> Function {
    // 0 haystack; 1 needle; 2 start; 3 needle cursor; 4 last possible start.
    let mut f = Function::new(vec![(3, ValType::I32)]);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::End);
    f
}

fn trim(layout: &TypeLayout, runtime: StringRuntime) -> Function {
    // 0 text; 1 start; 2 end; 3 answer.
    let mut f = Function::new(vec![(2, ValType::I32), (1, str_type(layout))]);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalSet(2));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::Call(runtime.is_space()));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::Call(runtime.is_space()));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: layout.str_array,
        array_type_index_src: layout.str_array,
    });
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::End);
    f
}

fn code_at(layout: &TypeLayout) -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64LtS);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GeS);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::End);
    f
}

fn from_code(layout: &TypeLayout) -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64GeS);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0x10ffff));
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0xd800));
    f.instruction(&Instruction::I64LtS);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0xdfff));
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::I32Or);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::If(BlockType::Result(str_type(layout))));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::ArrayNewFixed {
        array_type_index: layout.str_array,
        array_size: 1,
    });
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::ArrayNewFixed {
        array_type_index: layout.str_array,
        array_size: 0,
    });
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f
}

/// Unicode White_Space, matching Rust's `char::is_whitespace`, which defines
/// the VM and native backend behavior.
fn is_space() -> Function {
    let mut f = Function::new(Vec::new());
    // U+0009..U+000D
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x09));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x0d));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::I32And);
    for cp in [0x20, 0x85, 0xa0, 0x1680] {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(cp));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::I32Or);
    }
    // U+2000..U+200A
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x2000));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x200a));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::I32Or);
    for cp in [0x2028, 0x2029, 0x202f, 0x205f, 0x3000] {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(cp));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::I32Or);
    }
    f.instruction(&Instruction::End);
    f
}
