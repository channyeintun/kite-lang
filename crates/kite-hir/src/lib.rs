//! Typed high-level IR, and the type representation itself.
//!
//! HIR is produced by the type checker. Names are resolved to indices, every
//! expression carries a [`Ty`], and surface sugar has been expanded.
//!
//! Loop *forms* survive into HIR rather than being desugared here. Flattening
//! `for i in a..b` into an increment at the end of the body would break
//! `continue`, which must still run the increment before testing again. MIR
//! builds the control-flow graph and gets that right by construction.

use kite_span::Span;
use std::fmt;

pub mod ty;
pub use ty::Ty;

// ---------------------------------------------------------------------------
// Indices
// ---------------------------------------------------------------------------

macro_rules! index {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index!(FnId, "Index into [`Program::fns`].");
index!(LocalId, "Index into [`Function::locals`]. Parameters come first.");

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Program {
    pub fns: Vec<Function>,
    /// The `main` function, if this program has one.
    pub entry: Option<FnId>,
}

impl Program {
    pub fn function(&self, id: FnId) -> &Function {
        &self.fns[id.index()]
    }
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub is_pub: bool,
    pub is_async: bool,
    /// Parameters occupy locals `0..param_count`.
    pub param_count: usize,
    pub locals: Vec<Local>,
    pub ret: Ty,
    pub body: Block,
    pub span: Span,
}

impl Function {
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.index()]
    }

    pub fn params(&self) -> &[Local] {
        &self.locals[..self.param_count]
    }
}

#[derive(Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub span: Span,
    /// Locals the compiler introduced, such as loop bounds. Excluded from
    /// "unused variable" reporting.
    pub synthetic: bool,
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug)]
pub enum Stmt {
    /// `let` / `var`. `init` is absent for deferred initialisation.
    Let { local: LocalId, init: Option<Expr>, span: Span },
    Assign { local: LocalId, value: Expr, span: Span },
    Expr(Expr),
    Return { value: Option<Expr>, span: Span },
    If { cond: Expr, then: Block, else_: Option<Block>, span: Span },
    /// `for i in a..b` — MIR places the increment in the continue target.
    ForRange {
        var: LocalId,
        start: Expr,
        end: Expr,
        inclusive: bool,
        body: Block,
        label: Option<String>,
        span: Span,
    },
    /// `for cond { }`
    While { cond: Expr, body: Block, label: Option<String>, span: Span },
    /// `for { }`
    Loop { body: Block, label: Option<String>, span: Span },
    Break { label: Option<String>, span: Span },
    Continue { label: Option<String>, span: Span },
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Local(LocalId),
    Call { callee: FnId, args: Vec<Expr> },
    CallBuiltin { builtin: Builtin, args: Vec<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Unary { op: UnOp, operand: Box<Expr> },
    /// `if c { a } else { b }` in value position.
    If { cond: Box<Expr>, then: Box<Expr>, else_: Box<Expr> },
    /// Produced where a type error was already reported. Poisons downstream
    /// checks so one mistake yields one diagnostic.
    Error,
}

/// Functions provided by the compiler rather than written in Kite. Phase 1 has
/// only output; the standard library replaces these from Phase 6.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Builtin {
    /// `io.print(v)` — writes `v` followed by a newline.
    IoPrint,
}

impl Builtin {
    pub fn path(self) -> &'static str {
        match self {
            Builtin::IoPrint => "io.print",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    AddInt,
    SubInt,
    MulInt,
    DivInt,
    RemInt,
    AddFloat,
    SubFloat,
    MulFloat,
    DivFloat,
    /// String concatenation — `+` on two `str` values.
    ConcatStr,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    EqInt,
    NeInt,
    LtInt,
    LeInt,
    GtInt,
    GeInt,
    EqFloat,
    NeFloat,
    LtFloat,
    LeFloat,
    GtFloat,
    GeFloat,
    EqBool,
    NeBool,
    EqStr,
    NeStr,
    /// Short-circuiting. MIR lowers these to branches, not to instructions.
    And,
    Or,
}

impl BinOp {
    pub fn is_short_circuit(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    NegInt,
    NegFloat,
    Not,
}

// ---------------------------------------------------------------------------
// Display, for `kitec --emit hir`
// ---------------------------------------------------------------------------

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for func in &self.fns {
            writeln!(f, "{}", func)?;
        }
        Ok(())
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}(", self.name)?;
        for (i, p) in self.params().iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "_{}: {}", i, p.ty)?;
        }
        writeln!(f, ") -> {} {{", self.ret)?;
        for (i, l) in self.locals.iter().enumerate().skip(self.param_count) {
            writeln!(f, "  // _{} {}: {}", i, l.name, l.ty)?;
        }
        write_block(f, &self.body, 1)?;
        writeln!(f, "}}")
    }
}

fn indent(f: &mut fmt::Formatter<'_>, n: usize) -> fmt::Result {
    for _ in 0..n {
        write!(f, "  ")?;
    }
    Ok(())
}

fn write_block(f: &mut fmt::Formatter<'_>, b: &Block, depth: usize) -> fmt::Result {
    for s in &b.stmts {
        write_stmt(f, s, depth)?;
    }
    Ok(())
}

fn write_stmt(f: &mut fmt::Formatter<'_>, s: &Stmt, depth: usize) -> fmt::Result {
    indent(f, depth)?;
    match s {
        Stmt::Let { local, init, .. } => match init {
            Some(e) => writeln!(f, "let _{} = {}", local.0, e),
            None => writeln!(f, "let _{}", local.0),
        },
        Stmt::Assign { local, value, .. } => writeln!(f, "_{} = {}", local.0, value),
        Stmt::Expr(e) => writeln!(f, "{}", e),
        Stmt::Return { value: Some(e), .. } => writeln!(f, "return {}", e),
        Stmt::Return { value: None, .. } => writeln!(f, "return"),
        Stmt::If { cond, then, else_, .. } => {
            writeln!(f, "if {} {{", cond)?;
            write_block(f, then, depth + 1)?;
            if let Some(e) = else_ {
                indent(f, depth)?;
                writeln!(f, "}} else {{")?;
                write_block(f, e, depth + 1)?;
            }
            indent(f, depth)?;
            writeln!(f, "}}")
        }
        Stmt::ForRange { var, start, end, inclusive, body, .. } => {
            writeln!(
                f,
                "for _{} in {}{}{} {{",
                var.0,
                start,
                if *inclusive { "..=" } else { ".." },
                end
            )?;
            write_block(f, body, depth + 1)?;
            indent(f, depth)?;
            writeln!(f, "}}")
        }
        Stmt::While { cond, body, .. } => {
            writeln!(f, "while {} {{", cond)?;
            write_block(f, body, depth + 1)?;
            indent(f, depth)?;
            writeln!(f, "}}")
        }
        Stmt::Loop { body, .. } => {
            writeln!(f, "loop {{")?;
            write_block(f, body, depth + 1)?;
            indent(f, depth)?;
            writeln!(f, "}}")
        }
        Stmt::Break { .. } => writeln!(f, "break"),
        Stmt::Continue { .. } => writeln!(f, "continue"),
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Int(v) => write!(f, "{}", v),
            ExprKind::Float(v) => write!(f, "{:?}", v),
            ExprKind::Str(s) => write!(f, "{:?}", s),
            ExprKind::Bool(b) => write!(f, "{}", b),
            ExprKind::Local(l) => write!(f, "_{}", l.0),
            ExprKind::Call { callee, args } => {
                write!(f, "fn{}(", callee.0)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
            ExprKind::CallBuiltin { builtin, args } => {
                write!(f, "{}(", builtin.path())?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
            ExprKind::Binary { op, lhs, rhs } => write!(f, "({:?} {} {})", op, lhs, rhs),
            ExprKind::Unary { op, operand } => write!(f, "({:?} {})", op, operand),
            ExprKind::If { cond, then, else_ } => write!(f, "(if {} {} {})", cond, then, else_),
            ExprKind::Error => write!(f, "<error>"),
        }
    }
}
