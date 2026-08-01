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

use kite_hir::{BinOp, Builtin, TyId, Types, UnOp};
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
}

#[derive(Debug)]
pub enum Rvalue {
    Use(Operand),
    Binary { op: BinOp, lhs: Operand, rhs: Operand },
    Unary { op: UnOp, operand: Operand },
    Call { callee: FnId, args: Vec<Operand> },
    CallBuiltin { builtin: Builtin, args: Vec<Operand> },
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
            Rvalue::CallBuiltin { builtin, args } => {
                write!(f, "call {}(", builtin.path())?;
                write_operands(f, args)?;
                write!(f, ")")
            }
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
