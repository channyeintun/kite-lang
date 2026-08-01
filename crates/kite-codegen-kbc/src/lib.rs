//! Kite bytecode (`.kbc`) and the code generator that produces it.
//!
//! The VM is **register-based**, in the Lua 5 and Dart tradition rather than
//! the JVM's stack tradition. A register machine executes far fewer dispatches
//! per operation and maps almost directly from MIR locals, which already are
//! numbered slots.
//!
//! The bytecode backend exists for three reasons beyond running code: it is the
//! fast dev loop, it is the embedding target, and it is the differential-testing
//! oracle for the Wasm and native backends. Three independent implementations
//! that must agree is what makes codegen bugs findable.

use kite_hir::{BinOp, Builtin, UnOp};
use std::fmt;
use std::rc::Rc;

mod emit;
pub use emit::compile;

/// A register index within the current frame.
pub type Reg = u16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Native {
    IoPrint,
}

impl Native {
    pub fn from_builtin(b: Builtin) -> Native {
        match b {
            Builtin::IoPrint => Native::IoPrint,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Native::IoPrint => "io.print",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Op {
    // ---- loads ------------------------------------------------------------
    LoadInt { dst: Reg, value: i64 },
    LoadFloat { dst: Reg, value: f64 },
    LoadBool { dst: Reg, value: bool },
    LoadStr { dst: Reg, idx: u32 },
    LoadUnit { dst: Reg },
    LoadNil { dst: Reg },
    IsNil { dst: Reg, obj: Reg },
    NewPair { dst: Reg, value: Reg, error: Reg },
    PairValue { dst: Reg, obj: Reg },
    PairError { dst: Reg, obj: Reg },
    NewError { dst: Reg, message: Reg },
    ErrorMessage { dst: Reg, obj: Reg },
    Move { dst: Reg, src: Reg },

    // ---- arithmetic -------------------------------------------------------
    AddInt { dst: Reg, a: Reg, b: Reg },
    SubInt { dst: Reg, a: Reg, b: Reg },
    MulInt { dst: Reg, a: Reg, b: Reg },
    DivInt { dst: Reg, a: Reg, b: Reg },
    RemInt { dst: Reg, a: Reg, b: Reg },
    AddFloat { dst: Reg, a: Reg, b: Reg },
    SubFloat { dst: Reg, a: Reg, b: Reg },
    MulFloat { dst: Reg, a: Reg, b: Reg },
    DivFloat { dst: Reg, a: Reg, b: Reg },
    ConcatStr { dst: Reg, a: Reg, b: Reg },

    BitAnd { dst: Reg, a: Reg, b: Reg },
    BitOr { dst: Reg, a: Reg, b: Reg },
    BitXor { dst: Reg, a: Reg, b: Reg },
    Shl { dst: Reg, a: Reg, b: Reg },
    Shr { dst: Reg, a: Reg, b: Reg },

    NegInt { dst: Reg, a: Reg },
    NegFloat { dst: Reg, a: Reg },
    Not { dst: Reg, a: Reg },

    // ---- comparison -------------------------------------------------------
    EqInt { dst: Reg, a: Reg, b: Reg },
    NeInt { dst: Reg, a: Reg, b: Reg },
    LtInt { dst: Reg, a: Reg, b: Reg },
    LeInt { dst: Reg, a: Reg, b: Reg },
    GtInt { dst: Reg, a: Reg, b: Reg },
    GeInt { dst: Reg, a: Reg, b: Reg },

    EqFloat { dst: Reg, a: Reg, b: Reg },
    NeFloat { dst: Reg, a: Reg, b: Reg },
    LtFloat { dst: Reg, a: Reg, b: Reg },
    LeFloat { dst: Reg, a: Reg, b: Reg },
    GtFloat { dst: Reg, a: Reg, b: Reg },
    GeFloat { dst: Reg, a: Reg, b: Reg },

    EqBool { dst: Reg, a: Reg, b: Reg },
    NeBool { dst: Reg, a: Reg, b: Reg },
    EqStr { dst: Reg, a: Reg, b: Reg },
    NeStr { dst: Reg, a: Reg, b: Reg },
    /// Structural comparison, used for aggregates.
    EqValue { dst: Reg, a: Reg, b: Reg },
    NeValue { dst: Reg, a: Reg, b: Reg },

    // ---- control ----------------------------------------------------------
    Jump { target: u32 },
    JumpIfFalse { cond: Reg, target: u32 },

    // ---- aggregates -------------------------------------------------------
    /// Field values occupy `base .. base + count`, in declaration order.
    NewStruct { dst: Reg, struct_id: u32, base: Reg, count: u8 },
    GetField { dst: Reg, obj: Reg, index: u16 },
    /// Payload values occupy `base .. base + count`.
    NewEnum { dst: Reg, enum_id: u32, variant: u32, base: Reg, count: u8 },
    /// The variant index of an enum value, as an int.
    TagOf { dst: Reg, obj: Reg },
    /// Elements occupy `base .. base + count`.
    NewSlice { dst: Reg, base: Reg, count: u8 },
    /// Traps when out of range: an index bug is a program bug.
    GetIndex { dst: Reg, obj: Reg, index: Reg },
    SetIndex { obj: Reg, index: Reg, src: Reg },
    SliceLen { dst: Reg, obj: Reg },
    /// Yields an optional rather than trapping.
    SliceGet { dst: Reg, obj: Reg, index: Reg },
    /// Appends in place, copy-on-write.
    SlicePush { obj: Reg, src: Reg },
    SetField { obj: Reg, index: u16, src: Reg },

    /// Arguments occupy `base .. base + argc`.
    Call { dst: Reg, func: u32, base: Reg, argc: u8 },
    CallNative { dst: Reg, native: Native, base: Reg, argc: u8 },
    Return { src: Option<Reg> },

    /// A trap. Never catchable — Kite has no `recover`.
    Unreachable,
}

#[derive(Debug)]
pub struct FnProto {
    pub name: String,
    pub param_count: usize,
    /// Registers to allocate: MIR locals plus the outgoing-argument window.
    pub frame_size: usize,
    pub code: Vec<Op>,
}

#[derive(Debug, Default)]
pub struct Chunk {
    pub functions: Vec<FnProto>,
    pub strings: Vec<Rc<str>>,
    pub entry: Option<u32>,
}

impl Chunk {
    pub fn function(&self, i: u32) -> &FnProto {
        &self.functions[i as usize]
    }
}

/// The instruction constructor for a binary operation, or `None` for the
/// short-circuit operators, which MIR has already turned into branches.
pub fn binop_instruction(op: BinOp) -> Option<fn(Reg, Reg, Reg) -> Op> {
    use BinOp::*;
    Some(match op {
        AddInt => |dst, a, b| Op::AddInt { dst, a, b },
        SubInt => |dst, a, b| Op::SubInt { dst, a, b },
        MulInt => |dst, a, b| Op::MulInt { dst, a, b },
        DivInt => |dst, a, b| Op::DivInt { dst, a, b },
        RemInt => |dst, a, b| Op::RemInt { dst, a, b },
        AddFloat => |dst, a, b| Op::AddFloat { dst, a, b },
        SubFloat => |dst, a, b| Op::SubFloat { dst, a, b },
        MulFloat => |dst, a, b| Op::MulFloat { dst, a, b },
        DivFloat => |dst, a, b| Op::DivFloat { dst, a, b },
        ConcatStr => |dst, a, b| Op::ConcatStr { dst, a, b },
        BitAnd => |dst, a, b| Op::BitAnd { dst, a, b },
        BitOr => |dst, a, b| Op::BitOr { dst, a, b },
        BitXor => |dst, a, b| Op::BitXor { dst, a, b },
        Shl => |dst, a, b| Op::Shl { dst, a, b },
        Shr => |dst, a, b| Op::Shr { dst, a, b },
        EqInt => |dst, a, b| Op::EqInt { dst, a, b },
        NeInt => |dst, a, b| Op::NeInt { dst, a, b },
        LtInt => |dst, a, b| Op::LtInt { dst, a, b },
        LeInt => |dst, a, b| Op::LeInt { dst, a, b },
        GtInt => |dst, a, b| Op::GtInt { dst, a, b },
        GeInt => |dst, a, b| Op::GeInt { dst, a, b },
        EqFloat => |dst, a, b| Op::EqFloat { dst, a, b },
        NeFloat => |dst, a, b| Op::NeFloat { dst, a, b },
        LtFloat => |dst, a, b| Op::LtFloat { dst, a, b },
        LeFloat => |dst, a, b| Op::LeFloat { dst, a, b },
        GtFloat => |dst, a, b| Op::GtFloat { dst, a, b },
        GeFloat => |dst, a, b| Op::GeFloat { dst, a, b },
        EqBool => |dst, a, b| Op::EqBool { dst, a, b },
        NeBool => |dst, a, b| Op::NeBool { dst, a, b },
        EqStr => |dst, a, b| Op::EqStr { dst, a, b },
        NeStr => |dst, a, b| Op::NeStr { dst, a, b },
        EqValue => |dst, a, b| Op::EqValue { dst, a, b },
        NeValue => |dst, a, b| Op::NeValue { dst, a, b },
        And | Or => return None,
    })
}

pub fn unop_instruction(op: UnOp) -> fn(Reg, Reg) -> Op {
    match op {
        UnOp::NegInt => |dst, a| Op::NegInt { dst, a },
        UnOp::NegFloat => |dst, a| Op::NegFloat { dst, a },
        UnOp::Not => |dst, a| Op::Not { dst, a },
    }
}

// ---------------------------------------------------------------------------
// Disassembly, for `kitec --emit kbc`
// ---------------------------------------------------------------------------

impl fmt::Display for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.strings.iter().enumerate() {
            writeln!(f, "str{} = {:?}", i, s)?;
        }
        if !self.strings.is_empty() {
            writeln!(f)?;
        }
        for (i, func) in self.functions.iter().enumerate() {
            let entry = if self.entry == Some(i as u32) { "   ; entry" } else { "" };
            writeln!(
                f,
                "fn{} {}({} params, {} registers){}",
                i, func.name, func.param_count, func.frame_size, entry
            )?;
            for (pc, op) in func.code.iter().enumerate() {
                writeln!(f, "  {:>4}  {}", pc, op)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn bin(f: &mut fmt::Formatter<'_>, n: &str, d: Reg, a: Reg, b: Reg) -> fmt::Result {
            write!(f, "{:<12} r{}, r{}, r{}", n, d, a, b)
        }
        fn un(f: &mut fmt::Formatter<'_>, n: &str, d: Reg, a: Reg) -> fmt::Result {
            write!(f, "{:<12} r{}, r{}", n, d, a)
        }
        use Op::*;
        match *self {
            LoadInt { dst, value } => write!(f, "{:<12} r{}, {}", "load.int", dst, value),
            LoadFloat { dst, value } => write!(f, "{:<12} r{}, {:?}", "load.float", dst, value),
            LoadBool { dst, value } => write!(f, "{:<12} r{}, {}", "load.bool", dst, value),
            LoadStr { dst, idx } => write!(f, "{:<12} r{}, str{}", "load.str", dst, idx),
            LoadUnit { dst } => write!(f, "{:<12} r{}", "load.unit", dst),
            LoadNil { dst } => write!(f, "{:<12} r{}", "load.nil", dst),
            IsNil { dst, obj } => write!(f, "{:<12} r{}, r{}", "is.nil", dst, obj),
            NewPair { dst, value, error } => {
                write!(f, "{:<12} r{}, r{}, r{}", "new.pair", dst, value, error)
            }
            PairValue { dst, obj } => write!(f, "{:<12} r{}, r{}", "pair.value", dst, obj),
            PairError { dst, obj } => write!(f, "{:<12} r{}, r{}", "pair.error", dst, obj),
            NewError { dst, message } => write!(f, "{:<12} r{}, r{}", "new.error", dst, message),
            ErrorMessage { dst, obj } => write!(f, "{:<12} r{}, r{}", "err.message", dst, obj),
            Move { dst, src } => write!(f, "{:<12} r{}, r{}", "move", dst, src),

            AddInt { dst, a, b } => bin(f, "add.int", dst, a, b),
            SubInt { dst, a, b } => bin(f, "sub.int", dst, a, b),
            MulInt { dst, a, b } => bin(f, "mul.int", dst, a, b),
            DivInt { dst, a, b } => bin(f, "div.int", dst, a, b),
            RemInt { dst, a, b } => bin(f, "rem.int", dst, a, b),
            AddFloat { dst, a, b } => bin(f, "add.float", dst, a, b),
            SubFloat { dst, a, b } => bin(f, "sub.float", dst, a, b),
            MulFloat { dst, a, b } => bin(f, "mul.float", dst, a, b),
            DivFloat { dst, a, b } => bin(f, "div.float", dst, a, b),
            ConcatStr { dst, a, b } => bin(f, "concat.str", dst, a, b),
            BitAnd { dst, a, b } => bin(f, "and", dst, a, b),
            BitOr { dst, a, b } => bin(f, "or", dst, a, b),
            BitXor { dst, a, b } => bin(f, "xor", dst, a, b),
            Shl { dst, a, b } => bin(f, "shl", dst, a, b),
            Shr { dst, a, b } => bin(f, "shr", dst, a, b),

            NegInt { dst, a } => un(f, "neg.int", dst, a),
            NegFloat { dst, a } => un(f, "neg.float", dst, a),
            Not { dst, a } => un(f, "not", dst, a),

            EqInt { dst, a, b } => bin(f, "eq.int", dst, a, b),
            NeInt { dst, a, b } => bin(f, "ne.int", dst, a, b),
            LtInt { dst, a, b } => bin(f, "lt.int", dst, a, b),
            LeInt { dst, a, b } => bin(f, "le.int", dst, a, b),
            GtInt { dst, a, b } => bin(f, "gt.int", dst, a, b),
            GeInt { dst, a, b } => bin(f, "ge.int", dst, a, b),
            EqFloat { dst, a, b } => bin(f, "eq.float", dst, a, b),
            NeFloat { dst, a, b } => bin(f, "ne.float", dst, a, b),
            LtFloat { dst, a, b } => bin(f, "lt.float", dst, a, b),
            LeFloat { dst, a, b } => bin(f, "le.float", dst, a, b),
            GtFloat { dst, a, b } => bin(f, "gt.float", dst, a, b),
            GeFloat { dst, a, b } => bin(f, "ge.float", dst, a, b),
            EqBool { dst, a, b } => bin(f, "eq.bool", dst, a, b),
            NeBool { dst, a, b } => bin(f, "ne.bool", dst, a, b),
            EqStr { dst, a, b } => bin(f, "eq.str", dst, a, b),
            NeStr { dst, a, b } => bin(f, "ne.str", dst, a, b),
            EqValue { dst, a, b } => bin(f, "eq.value", dst, a, b),
            NeValue { dst, a, b } => bin(f, "ne.value", dst, a, b),

            NewStruct { dst, struct_id, base, count } => write!(
                f,
                "{:<12} r{}, struct{}, r{}, {}",
                "new.struct", dst, struct_id, base, count
            ),
            GetField { dst, obj, index } => {
                write!(f, "{:<12} r{}, r{}, {}", "get.field", dst, obj, index)
            }
            NewEnum { dst, enum_id, variant, base, count } => write!(
                f,
                "{:<12} r{}, enum{}#{}, r{}, {}",
                "new.enum", dst, enum_id, variant, base, count
            ),
            TagOf { dst, obj } => write!(f, "{:<12} r{}, r{}", "tag", dst, obj),
            NewSlice { dst, base, count } => {
                write!(f, "{:<12} r{}, r{}, {}", "new.slice", dst, base, count)
            }
            GetIndex { dst, obj, index } => {
                write!(f, "{:<12} r{}, r{}, r{}", "get.index", dst, obj, index)
            }
            SetIndex { obj, index, src } => {
                write!(f, "{:<12} r{}, r{}, r{}", "set.index", obj, index, src)
            }
            SliceLen { dst, obj } => write!(f, "{:<12} r{}, r{}", "len", dst, obj),
            SliceGet { dst, obj, index } => {
                write!(f, "{:<12} r{}, r{}, r{}", "slice.get", dst, obj, index)
            }
            SlicePush { obj, src } => write!(f, "{:<12} r{}, r{}", "push", obj, src),
            SetField { obj, index, src } => {
                write!(f, "{:<12} r{}, {}, r{}", "set.field", obj, index, src)
            }
            Jump { target } => write!(f, "{:<12} {}", "jump", target),
            JumpIfFalse { cond, target } => write!(f, "{:<12} r{}, {}", "jump.false", cond, target),
            Call { dst, func: fi, base, argc } => {
                write!(f, "{:<12} r{}, fn{}, r{}, {}", "call", dst, fi, base, argc)
            }
            CallNative { dst, native, base, argc } => write!(
                f,
                "{:<12} r{}, {}, r{}, {}",
                "call.native",
                dst,
                native.name(),
                base,
                argc
            ),
            Return { src: Some(r) } => write!(f, "{:<12} r{}", "return", r),
            Return { src: None } => write!(f, "return"),
            Unreachable => write!(f, "unreachable"),
        }
    }
}

