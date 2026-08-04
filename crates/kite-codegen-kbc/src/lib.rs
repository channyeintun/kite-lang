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
    /// Hand the scheduler a task's resume closure. The state-machine
    /// transform emits this; no program writes it.
    TaskSpawn,
    /// Ask the scheduler not to poll the running task before a deadline.
    TaskWakeAt,
    /// Ask not to be polled again until some other task finishes.
    TaskPark,
    /// Ask not to be polled again until the host has had a turn.
    TaskWaitHost,
    /// Milliseconds since the program started.
    TimeNow,
    /// Whether two references are one cell.
    PtrSame,
    /// Trap when a claim is false.
    Require,
    DrawRect,
    DrawRRect,
    DrawFont,
    DrawDRRect,
    DrawAlpha,
    DrawText,
    DrawField,
    DrawImage,
    DrawSemantics,
    TextWidth,
    TextHeight,
    DrawClip,
    DrawUnclip,
}

impl Native {
    pub fn from_builtin(b: Builtin) -> Native {
        match b {
            Builtin::IoPrint => Native::IoPrint,
            Builtin::DrawRect => Native::DrawRect,
            Builtin::DrawRRect => Native::DrawRRect,
            Builtin::DrawFont => Native::DrawFont,
            Builtin::DrawDRRect => Native::DrawDRRect,
            Builtin::DrawAlpha => Native::DrawAlpha,
            Builtin::DrawText => Native::DrawText,
            Builtin::DrawField => Native::DrawField,
            Builtin::DrawImage => Native::DrawImage,
            Builtin::DrawSemantics => Native::DrawSemantics,
            Builtin::TextWidth => Native::TextWidth,
            Builtin::TextHeight => Native::TextHeight,
            Builtin::DrawClip => Native::DrawClip,
            Builtin::DrawUnclip => Native::DrawUnclip,
            Builtin::TaskSpawn => Native::TaskSpawn,
            Builtin::TaskWakeAt => Native::TaskWakeAt,
            Builtin::TaskPark => Native::TaskPark,
            Builtin::TaskWaitHost => Native::TaskWaitHost,
            Builtin::TimeNow => Native::TimeNow,
            Builtin::PtrSame => Native::PtrSame,
            Builtin::Require => Native::Require,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Native::IoPrint => "io.print",
            Native::DrawRect => "draw.rect",
            Native::DrawRRect => "draw.rrect",
            Native::DrawFont => "draw.font",
            Native::DrawDRRect => "draw.drrect",
            Native::DrawAlpha => "draw.alpha",
            Native::DrawText => "draw.text",
            Native::DrawField => "draw.field",
            Native::DrawImage => "draw.image",
            Native::DrawSemantics => "draw.semantics",
            Native::TextWidth => "text.width",
            Native::TextHeight => "text.height",
            Native::DrawClip => "draw.clip",
            Native::DrawUnclip => "draw.unclip",
            Native::TaskSpawn => "task.spawn",
            Native::TaskWakeAt => "task.wake_at",
            Native::TaskPark => "task.park",
            Native::TaskWaitHost => "task.wait_host",
            Native::TimeNow => "time.now",
            Native::PtrSame => "ptr.same",
            Native::Require => "require",
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
    /// The release-build forms: these wrap where the three above trap.
    AddIntWrap { dst: Reg, a: Reg, b: Reg },
    SubIntWrap { dst: Reg, a: Reg, b: Reg },
    MulIntWrap { dst: Reg, a: Reg, b: Reg },
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
    /// Ordering on strings, by code point.
    LtStr { dst: Reg, a: Reg, b: Reg },
    LeStr { dst: Reg, a: Reg, b: Reg },
    GtStr { dst: Reg, a: Reg, b: Reg },
    GeStr { dst: Reg, a: Reg, b: Reg },
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
    NewTuple { dst: Reg, base: Reg, count: u8 },
    /// Key/value pairs occupy `base .. base + count`, flattened.
    NewMap { dst: Reg, base: Reg, count: u8 },
    /// Yields an optional: a missing key is a runtime condition.
    MapGet { dst: Reg, obj: Reg, key: Reg },
    MapSet { obj: Reg, key: Reg, src: Reg },
    MapLen { dst: Reg, obj: Reg },
    /// A map's keys or values as a slice, in insertion order.
    MapKeys { dst: Reg, obj: Reg },
    MapValues { dst: Reg, obj: Reg },
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
    /// A call dispatched from the concrete type of the receiver, which sits at
    /// `base`. `table` indexes [`Chunk::vtables`].
    CallVirtual { dst: Reg, table: u32, method: u32, base: Reg, argc: u8 },
    /// Render a value as text. One instruction covers every type because a VM
    /// value carries its own tag; the Wasm backend needs three host calls for
    /// the same job.
    ToStr { dst: Reg, src: Reg },
    /// A string operation. Arguments occupy `base .. base + argc`, the
    /// receiver first.
    StrOp { dst: Reg, op: kite_hir::StrKind, base: Reg, argc: u8 },
    IntToFloat { dst: Reg, src: Reg },
    FloatToInt { dst: Reg, src: Reg },
    /// Build a closure: a function index plus the captured values, which sit
    /// at `base .. base + count`.
    Closure { dst: Reg, func: u32, base: Reg, count: u8 },
    /// Call a closure. Its captures are prepended to the arguments at `base`,
    /// so the callee is entered exactly like a named function.
    CallClosure { dst: Reg, callee: Reg, base: Reg, argc: u8 },
    CallNative { dst: Reg, native: Native, base: Reg, argc: u8 },
    /// A call across the declared host boundary. `index` is into
    /// [`Chunk::externs`]; what answers it is the embedder's business.
    CallExtern { dst: Reg, index: u32, base: Reg, argc: u8 },
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
    /// Host functions this program declared, as `group.name`. The bytecode
    /// target has no host of its own: an embedder supplies these, and one that
    /// is asked for and not supplied is a trap naming it.
    pub externs: Vec<String>,
    pub strings: Vec<Rc<str>>,
    pub entry: Option<u32>,
    /// Dispatch tables, one per trait used as an object.
    pub vtables: Vec<VTable>,
}

/// A trait's dispatch table: rows keyed by the receiver's runtime type tag.
/// A linear scan is right here — a program has few implementers per trait, and
/// a sorted-array probe beats a hash map at these sizes.
#[derive(Debug, Default)]
pub struct VTable {
    /// `(type tag, one function per trait method)`, sorted by tag.
    pub rows: Vec<(u32, Vec<u32>)>,
}

impl VTable {
    pub fn lookup(&self, tag: u32, method: u32) -> Option<u32> {
        let row = self.rows.binary_search_by_key(&tag, |(t, _)| *t).ok()?;
        self.rows[row].1.get(method as usize).copied()
    }
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
        AddIntWrap => |dst, a, b| Op::AddIntWrap { dst, a, b },
        SubIntWrap => |dst, a, b| Op::SubIntWrap { dst, a, b },
        MulIntWrap => |dst, a, b| Op::MulIntWrap { dst, a, b },
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
        LtStr => |dst, a, b| Op::LtStr { dst, a, b },
        LeStr => |dst, a, b| Op::LeStr { dst, a, b },
        GtStr => |dst, a, b| Op::GtStr { dst, a, b },
        GeStr => |dst, a, b| Op::GeStr { dst, a, b },
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
            CallExtern { dst, index, base, argc } => {
                write!(f, "{:<12} r{}, host{}, r{}, {}", "call.host", dst, index, base, argc)
            }
            LtStr { dst, a, b } => bin(f, "lt.str", dst, a, b),
            LeStr { dst, a, b } => bin(f, "le.str", dst, a, b),
            GtStr { dst, a, b } => bin(f, "gt.str", dst, a, b),
            GeStr { dst, a, b } => bin(f, "ge.str", dst, a, b),
            MapKeys { dst, obj } => un(f, "map.keys", dst, obj),
            MapValues { dst, obj } => un(f, "map.values", dst, obj),
            PairValue { dst, obj } => write!(f, "{:<12} r{}, r{}", "pair.value", dst, obj),
            PairError { dst, obj } => write!(f, "{:<12} r{}, r{}", "pair.error", dst, obj),
            NewError { dst, message } => write!(f, "{:<12} r{}, r{}", "new.error", dst, message),
            ErrorMessage { dst, obj } => write!(f, "{:<12} r{}, r{}", "err.message", dst, obj),
            Move { dst, src } => write!(f, "{:<12} r{}, r{}", "move", dst, src),

            AddInt { dst, a, b } => bin(f, "add.int", dst, a, b),
            AddIntWrap { dst, a, b } => bin(f, "add.int.wrap", dst, a, b),
            SubIntWrap { dst, a, b } => bin(f, "sub.int.wrap", dst, a, b),
            MulIntWrap { dst, a, b } => bin(f, "mul.int.wrap", dst, a, b),
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
            NewTuple { dst, base, count } => {
                write!(f, "{:<12} r{}, r{}, {}", "new.tuple", dst, base, count)
            }
            NewMap { dst, base, count } => {
                write!(f, "{:<12} r{}, r{}, {}", "new.map", dst, base, count)
            }
            MapGet { dst, obj, key } => {
                write!(f, "{:<12} r{}, r{}, r{}", "map.get", dst, obj, key)
            }
            MapSet { obj, key, src } => {
                write!(f, "{:<12} r{}, r{}, r{}", "map.set", obj, key, src)
            }
            MapLen { dst, obj } => write!(f, "{:<12} r{}, r{}", "map.len", dst, obj),
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
            ToStr { dst, src } => write!(f, "{:<12} r{}, r{}", "str", dst, src),
            StrOp { dst, op, base, argc } => {
                write!(f, "{:<12} r{}, r{}, {}", op.name(), dst, base, argc)
            }
            IntToFloat { dst, src } => write!(f, "{:<12} r{}, r{}", "int.float", dst, src),
            FloatToInt { dst, src } => write!(f, "{:<12} r{}, r{}", "float.int", dst, src),
            Closure { dst, func: fi, base, count } => {
                write!(f, "{:<12} r{}, fn{}, r{}, {}", "closure", dst, fi, base, count)
            }
            CallClosure { dst, callee, base, argc } => {
                write!(f, "{:<12} r{}, r{}, r{}, {}", "call.closure", dst, callee, base, argc)
            }
            CallVirtual { dst, table, method, base, argc } => write!(
                f,
                "{:<12} r{}, vt{}#{}, r{}, {}",
                "call.virtual", dst, table, method, base, argc
            ),
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

