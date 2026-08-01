//! Mid-level IR: explicit basic blocks and terminators.
//!
//! MIR is where control flow becomes a graph. That matters most for loops:
//! `continue` must jump to a block that runs the increment *before* testing
//! again, which is exactly why `for` survives HIR intact rather than being
//! flattened into an increment at the end of the body.
//!
//! Locals are not yet in SSA form. Phase 2 introduces the SSA construction the
//! optimisation passes want; the bytecode backend maps locals to registers
//! directly and does not need it.

use kite_hir::{BinOp, Builtin, EnumId, StructId, TraitId, TyId, Types, UnOp};
use kite_span::Span;
use std::fmt;

mod lower;
pub use lower::lower;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Local(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FnId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StrId(pub u32);

impl Local {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl BlockId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl FnId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Default)]
pub struct Program {
    pub fns: Vec<Function>,
    pub entry: Option<FnId>,
    /// Interned string constants, referenced by [`Operand::Str`].
    pub strings: Vec<String>,
    /// Dispatch tables, carried through unchanged from HIR. Lowering does not
    /// need them; the backends do.
    pub vtables: Vec<kite_hir::VTable>,
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// Parameters occupy locals `0..param_count`.
    pub param_count: usize,
    pub locals: Vec<LocalDecl>,
    pub ret: TyId,
    pub blocks: Vec<BasicBlock>,
    pub span: Span,
}

impl Function {
    pub fn entry_block(&self) -> BlockId {
        BlockId(0)
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.index()]
    }
}

#[derive(Debug)]
pub struct LocalDecl {
    pub ty: TyId,
    /// Source name, or `None` for a compiler temporary.
    pub name: Option<String>,
}

#[derive(Debug, Default)]
pub struct BasicBlock {
    pub stmts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Debug)]
pub enum Inst {
    Assign { dst: Local, value: Rvalue },
    /// Write through a reference. Separate from `Assign` because the
    /// destination is a heap slot rather than a local.
    SetField { base: Operand, index: u32, value: Operand },
    SetIndex { base: Operand, index: Operand, value: Operand },
    /// Appends in place. Slices are copy-on-write, so this targets a local
    /// rather than an arbitrary operand.
    SlicePush { local: Local, value: Operand },
    MapSet { local: Local, key: Operand, value: Operand },
}

#[derive(Debug)]
pub enum Rvalue {
    Use(Operand),
    Binary { op: BinOp, lhs: Operand, rhs: Operand },
    Unary { op: UnOp, operand: Operand },
    Call { callee: FnId, args: Vec<Operand> },
    /// Dispatched at run time from the receiver's concrete type.
    CallVirtual { trait_id: TraitId, method: u32, args: Vec<Operand> },
    /// A value rendered as text. `from` is its type, which the Wasm backend
    /// needs because its host calls are typed and the VM's values are not.
    ToStr { operand: Operand, from: TyId },
    CallBuiltin { builtin: Builtin, args: Vec<Operand> },
    StructNew { struct_id: StructId, fields: Vec<Operand> },
    FieldGet { base: Operand, index: u32 },
    EnumNew { enum_id: EnumId, variant: u32, fields: Vec<Operand> },
    /// The variant index of an enum value, for dispatching a `match`.
    TagOf { base: Operand },
    /// A payload field of a known enum variant. Separate from `FieldGet`
    /// because a backend using subtyped variant records has to cast first, and
    /// only the pattern that matched knows which variant it is.
    VariantGet { base: Operand, enum_id: EnumId, variant: u32, index: u32 },
    TupleNew { elems: Vec<Operand> },
    MapNew { entries: Vec<Operand> },
    MapGet { base: Operand, key: Operand },
    MapLen { base: Operand },
    SliceNew { elems: Vec<Operand> },
    IsNil { value: Operand },
    Wrap { value: Operand },
    Unwrap { value: Operand },
    PairNew { value: Operand, error: Operand },
    PairValue { base: Operand },
    PairError { base: Operand },
    ErrorNew { message: Operand },
    ErrorMessage { base: Operand },
    /// Bounds-checked; traps on failure.
    IndexGet { base: Operand, index: Operand },
    SliceLen { base: Operand },
    /// Bounds-checked; yields an optional rather than trapping.
    SliceGet { base: Operand, index: Operand },
}

#[derive(Clone, Debug)]
pub enum Operand {
    Local(Local),
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(StrId),
    /// The unit value. Never materialised by a backend.
    Unit,
    Nil,
}

#[derive(Debug, Default)]
pub enum Terminator {
    Goto(BlockId),
    Branch { cond: Operand, then: BlockId, else_: BlockId },
    Return(Option<Operand>),
    /// A block that cannot be reached. Left in place rather than pruned so
    /// block indices stay stable.
    #[default]
    Unreachable,
}

impl Terminator {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::Branch { then, else_, .. } => vec![*then, *else_],
            Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
        }
    }
}

/// Blocks reachable from the entry.
pub fn reachable_blocks(func: &Function) -> Vec<bool> {
    let mut seen = vec![false; func.blocks.len()];
    let mut stack = vec![func.entry_block()];
    while let Some(b) = stack.pop() {
        if seen[b.index()] {
            continue;
        }
        seen[b.index()] = true;
        for s in func.blocks[b.index()].term.successors() {
            stack.push(s);
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Display, for `kitec --emit mir`
// ---------------------------------------------------------------------------

/// Pairs a program with the arena its `TyId`s belong to, so it can be printed.
pub struct Display_<'a> {
    pub program: &'a Program,
    pub types: &'a Types,
}

impl Program {
    /// Render for `kitec --emit mir`. A `TyId` is meaningless without the
    /// arena, so it must be supplied.
    pub fn render<'a>(&'a self, types: &'a Types) -> Display_<'a> {
        Display_ { program: self, types }
    }
}

impl fmt::Display for Display_<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.program.strings.iter().enumerate() {
            writeln!(f, "str{} = {:?}", i, s)?;
        }
        if !self.program.strings.is_empty() {
            writeln!(f)?;
        }
        for func in &self.program.fns {
            self.write_fn(f, func)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

impl Display_<'_> {
    fn write_fn(&self, f: &mut fmt::Formatter<'_>, func: &Function) -> fmt::Result {
        write!(f, "fn {}(", func.name)?;
        for i in 0..func.param_count {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "_{}: {}", i, self.types.name(func.locals[i].ty))?;
        }
        writeln!(f, ") -> {} {{", self.types.name(func.ret))?;

        for (i, l) in func.locals.iter().enumerate().skip(func.param_count) {
            let ty = self.types.name(l.ty);
            match &l.name {
                Some(n) => writeln!(f, "  let _{}: {}   // {}", i, ty, n)?,
                None => writeln!(f, "  let _{}: {}", i, ty)?,
            }
        }

        for (i, b) in func.blocks.iter().enumerate() {
            writeln!(f, "  bb{}:", i)?;
            for s in &b.stmts {
                writeln!(f, "    {}", s)?;
            }
            writeln!(f, "    {}", b.term)?;
        }
        writeln!(f, "}}")
    }
}

impl fmt::Display for Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Inst::Assign { dst, value } => write!(f, "_{} = {}", dst.0, value),
            Inst::SetField { base, index, value } => {
                write!(f, "{}.{} = {}", base, index, value)
            }
            Inst::SetIndex { base, index, value } => write!(f, "{}[{}] = {}", base, index, value),
            Inst::SlicePush { local, value } => write!(f, "_{}.push({})", local.0, value),
            Inst::MapSet { local, key, value } => {
                write!(f, "_{}[{}] = {}", local.0, key, value)
            }
        }
    }
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(o) => write!(f, "{}", o),
            Rvalue::Binary { op, lhs, rhs } => write!(f, "{:?}({}, {})", op, lhs, rhs),
            Rvalue::Unary { op, operand } => write!(f, "{:?}({})", op, operand),
            Rvalue::Call { callee, args } => {
                write!(f, "call fn{}(", callee.0)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::ToStr { operand, .. } => write!(f, "str {}", operand),
            Rvalue::CallVirtual { trait_id, method, args } => {
                write!(f, "virtual trait{}#{}(", trait_id.0, method)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::CallBuiltin { builtin, args } => {
                write!(f, "call {}(", builtin.path())?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::StructNew { struct_id, fields } => {
                write!(f, "struct{}{{", struct_id.0)?;
                write_operands(f, fields)?;
                write!(f, "}}")
            }
            Rvalue::FieldGet { base, index } => write!(f, "{}.{}", base, index),
            Rvalue::EnumNew { enum_id, variant, fields } => {
                write!(f, "enum{}#{}(", enum_id.0, variant)?;
                write_operands(f, fields)?;
                write!(f, ")")
            }
            Rvalue::TagOf { base } => write!(f, "tag {}", base),
            Rvalue::VariantGet { base, variant, index, .. } => {
                write!(f, "{}#{}.{}", base, variant, index)
            }
            Rvalue::MapNew { entries } => write!(f, "{{{} entries}}", entries.len() / 2),
            Rvalue::MapGet { base, key } => write!(f, "{}[{}]", base, key),
            Rvalue::MapLen { base } => write!(f, "len {}", base),
            Rvalue::TupleNew { elems } => {
                write!(f, "(")?;
                write_operands(f, elems)?;
                write!(f, ")")
            }
            Rvalue::SliceNew { elems } => {
                write!(f, "[")?;
                write_operands(f, elems)?;
                write!(f, "]")
            }
            Rvalue::IndexGet { base, index } => write!(f, "{}[{}]", base, index),
            Rvalue::IsNil { value } => write!(f, "is-nil {}", value),
            Rvalue::Wrap { value } => write!(f, "wrap {}", value),
            Rvalue::Unwrap { value } => write!(f, "unwrap {}", value),
            Rvalue::PairNew { value, error } => write!(f, "pair({}, {})", value, error),
            Rvalue::PairValue { base } => write!(f, "{}.0", base),
            Rvalue::PairError { base } => write!(f, "{}.1", base),
            Rvalue::ErrorNew { message } => write!(f, "error({})", message),
            Rvalue::ErrorMessage { base } => write!(f, "{}.message()", base),
            Rvalue::SliceLen { base } => write!(f, "len {}", base),
            Rvalue::SliceGet { base, index } => write!(f, "{}.get({})", base, index),
        }
    }
}

fn write_operands(f: &mut fmt::Formatter<'_>, args: &[Operand]) -> fmt::Result {
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{}", a)?;
    }
    Ok(())
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Local(l) => write!(f, "_{}", l.0),
            Operand::Int(v) => write!(f, "{}", v),
            Operand::Float(v) => write!(f, "{:?}", v),
            Operand::Bool(v) => write!(f, "{}", v),
            Operand::Str(s) => write!(f, "str{}", s.0),
            Operand::Unit => write!(f, "()"),
            Operand::Nil => write!(f, "nil"),
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(b) => write!(f, "goto bb{}", b.0),
            Terminator::Branch { cond, then, else_ } => {
                write!(f, "branch {} ? bb{} : bb{}", cond, then.0, else_.0)
            }
            Terminator::Return(Some(o)) => write!(f, "return {}", o),
            Terminator::Return(None) => write!(f, "return"),
            Terminator::Unreachable => write!(f, "unreachable"),
        }
    }
}

#[cfg(test)]
mod tests;
