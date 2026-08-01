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
pub use ty::{
    EnumDef, EnumId, FieldDef, StructDef, StructId, TraitDef, TraitId, TraitMethodDef, TyId,
    TyKind, Types, VariantDef,
};

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
    /// The interned type arena. Owned here so every consumer of a `Program`
    /// can resolve a [`TyId`] without a second argument threaded alongside.
    pub types: Types,
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
    pub ret: TyId,
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
    pub ty: TyId,
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
    /// `p.label = "x"` — only reachable for a field declared `var`.
    SetField { base: Expr, index: u32, value: Expr, span: Span },
    /// `xs[i] = v`
    SetIndex { base: Expr, index: Expr, value: Expr, span: Span },
    /// `xs.push(v)`. Slices are copy-on-write values, so this mutates the
    /// binding, which must therefore be `var`.
    SlicePush { local: LocalId, value: Expr, span: Span },
    /// `for x in xs`
    ForSlice {
        var: LocalId,
        slice: Expr,
        body: Block,
        label: Option<String>,
        span: Span,
    },
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
    /// A group of statements the checker emitted for one source statement, such
    /// as the three bindings a `let (v, err) = f()` expands to.
    Block(Block),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: TyId,
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
    /// `Point{ x: 1.0, y: 2.0 }`. Fields are in declaration order, and all of
    /// them are present — Kite has no zero values, so a literal that omits one
    /// never reaches HIR.
    StructNew { struct_id: StructId, fields: Vec<Expr> },
    /// `p.x`, by field position.
    FieldGet { base: Box<Expr>, index: u32 },
    /// `Circle(radius: 1.0)`, or a unit variant such as `Point`.
    EnumNew { enum_id: EnumId, variant: u32, fields: Vec<Expr> },
    /// Exhaustive by construction: the checker has already proved every value
    /// is covered, so lowering needs no fallback arm.
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    /// `(value, err)` — the result of a fallible function.
    PairNew { value: Box<Expr>, error: Box<Expr> },
    /// The value slot of a correlated pair. Only emitted where the taint
    /// analysis has proved the error was checked and found nil.
    PairValue { base: Box<Expr> },
    /// The error slot of a correlated pair.
    PairError { base: Box<Expr> },
    /// `errors.new("...")`
    ErrorNew { message: Box<Expr> },
    /// `err.message()`
    ErrorMessage { base: Box<Expr> },
    /// The absent optional. Only ever produced where the type is `?T`; Kite
    /// has no null anywhere else.
    Nil,
    /// Tests for nil. Used by `check`, `nil` patterns, and the narrowing an
    /// inline `if` performs.
    IsNil { value: Box<Expr> },
    /// `T` into `Option<T>`. Emitted at every subsumption site, so the
    /// representation change is explicit in the IR rather than implied.
    Wrap { value: Box<Expr> },
    /// `Option<T>` back to `T`, where narrowing has proved it is present.
    /// Never emitted on a path where the value could be nil.
    Unwrap { value: Box<Expr> },
    /// `[1, 2, 3]`
    SliceNew { elems: Vec<Expr> },
    /// `xs[i]` — bounds-checked, and traps on failure because an out-of-range
    /// index is a program bug, not a runtime condition.
    Index { base: Box<Expr>, index: Box<Expr> },
    /// `xs.len()`
    SliceLen { base: Box<Expr> },
    /// `xs.get(i)` — yields `?T` for the case where the index genuinely is a
    /// runtime condition.
    SliceGet { base: Box<Expr>, index: Box<Expr> },
    /// A block in expression position — a `match` arm written with braces.
    /// Runs for its effects and produces unit.
    Block(Block),
    /// Produced where a type error was already reported. Poisons downstream
    /// checks so one mistake yields one diagnostic.
    Error,
}

#[derive(Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// Locals the pattern binds, in the order the pattern introduces them.
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

/// A resolved pattern. Names are already bound to local slots, and variants to
/// positions, so lowering is a mechanical walk.
#[derive(Debug)]
pub enum Pattern {
    /// `_`, and any binding — both match everything. A binding also writes the
    /// scrutinee into its local.
    Wildcard,
    /// Binds the scrutinee. `unwrap` is set where narrowing has proved an
    /// optional is present, so the local holds the payload rather than the
    /// optional — the same explicitness an inline `if` gets.
    Binding { local: LocalId, unwrap: bool },
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    /// `4..=9`
    IntRange { start: i64, end: i64, inclusive: bool },
    /// `Circle(r)`. Sub-patterns are in declaration order, one per field.
    Variant { enum_id: EnumId, variant: u32, fields: Vec<Pattern> },
    /// `Point{ x: 0.0, y }`. Only the named fields are tested.
    Struct { struct_id: StructId, fields: Vec<(u32, Pattern)> },
    /// `nil`
    Nil,
    /// `1 | 2 | 3`
    Or(Vec<Pattern>),
}

impl Pattern {
    /// Whether this pattern matches every value, so no test is needed.
    pub fn is_irrefutable(&self) -> bool {
        match self {
            Pattern::Wildcard | Pattern::Binding { .. } => true,
            Pattern::Or(alts) => alts.iter().any(|a| a.is_irrefutable()),
            _ => false,
        }
    }
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
    /// Structural comparison for aggregates. Two structs are equal when their
    /// fields are; there is no reference-equality operator in the surface
    /// language.
    EqValue,
    NeValue,
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
//
// A `TyId` is only meaningful against the arena, so rendering hangs off
// `Program`, which owns it, rather than off the individual nodes.

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for func in &self.fns {
            self.write_fn(f, func)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

impl Program {
    fn ty(&self, id: TyId) -> String {
        self.types.name(id)
    }

    fn write_fn(&self, f: &mut fmt::Formatter<'_>, func: &Function) -> fmt::Result {
        write!(f, "fn {}(", func.name)?;
        for (i, p) in func.params().iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "_{}: {}", i, self.ty(p.ty))?;
        }
        writeln!(f, ") -> {} {{", self.ty(func.ret))?;
        for (i, l) in func.locals.iter().enumerate().skip(func.param_count) {
            writeln!(f, "  // _{} {}: {}", i, l.name, self.ty(l.ty))?;
        }
        self.write_block(f, &func.body, 1)?;
        writeln!(f, "}}")
    }

    fn write_block(&self, f: &mut fmt::Formatter<'_>, b: &Block, depth: usize) -> fmt::Result {
        for s in &b.stmts {
            self.write_stmt(f, s, depth)?;
        }
        Ok(())
    }

    fn write_stmt(&self, f: &mut fmt::Formatter<'_>, s: &Stmt, depth: usize) -> fmt::Result {
        indent(f, depth)?;
        match s {
            Stmt::Let { local, init, .. } => match init {
                Some(e) => writeln!(f, "let _{} = {}", local.0, self.expr(e)),
                None => writeln!(f, "let _{}", local.0),
            },
            Stmt::Assign { local, value, .. } => writeln!(f, "_{} = {}", local.0, self.expr(value)),
            Stmt::SetField { base, index, value, .. } => {
                writeln!(f, "{}.{} = {}", self.expr(base), index, self.expr(value))
            }
            Stmt::SetIndex { base, index, value, .. } => writeln!(
                f,
                "{}[{}] = {}",
                self.expr(base),
                self.expr(index),
                self.expr(value)
            ),
            Stmt::SlicePush { local, value, .. } => {
                writeln!(f, "_{}.push({})", local.0, self.expr(value))
            }
            Stmt::ForSlice { var, slice, body, .. } => {
                writeln!(f, "for _{} in {} {{", var.0, self.expr(slice))?;
                self.write_block(f, body, depth + 1)?;
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::Expr(e) => writeln!(f, "{}", self.expr(e)),
            Stmt::Return { value: Some(e), .. } => writeln!(f, "return {}", self.expr(e)),
            Stmt::Return { value: None, .. } => writeln!(f, "return"),
            Stmt::If { cond, then, else_, .. } => {
                writeln!(f, "if {} {{", self.expr(cond))?;
                self.write_block(f, then, depth + 1)?;
                if let Some(e) = else_ {
                    indent(f, depth)?;
                    writeln!(f, "}} else {{")?;
                    self.write_block(f, e, depth + 1)?;
                }
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::ForRange { var, start, end, inclusive, body, .. } => {
                writeln!(
                    f,
                    "for _{} in {}{}{} {{",
                    var.0,
                    self.expr(start),
                    if *inclusive { "..=" } else { ".." },
                    self.expr(end)
                )?;
                self.write_block(f, body, depth + 1)?;
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::While { cond, body, .. } => {
                writeln!(f, "while {} {{", self.expr(cond))?;
                self.write_block(f, body, depth + 1)?;
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::Loop { body, .. } => {
                writeln!(f, "loop {{")?;
                self.write_block(f, body, depth + 1)?;
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::Block(b) => {
                writeln!(f, "{{")?;
                self.write_block(f, b, depth + 1)?;
                indent(f, depth)?;
                writeln!(f, "}}")
            }
            Stmt::Break { .. } => writeln!(f, "break"),
            Stmt::Continue { .. } => writeln!(f, "continue"),
        }
    }

    fn expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(v) => v.to_string(),
            ExprKind::Float(v) => format!("{:?}", v),
            ExprKind::Str(s) => format!("{:?}", s),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Local(l) => format!("_{}", l.0),
            ExprKind::Call { callee, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("fn{}({})", callee.0, a.join(", "))
            }
            ExprKind::CallBuiltin { builtin, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("{}({})", builtin.path(), a.join(", "))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                format!("({:?} {} {})", op, self.expr(lhs), self.expr(rhs))
            }
            ExprKind::Unary { op, operand } => format!("({:?} {})", op, self.expr(operand)),
            ExprKind::If { cond, then, else_ } => format!(
                "(if {} {} {})",
                self.expr(cond),
                self.expr(then),
                self.expr(else_)
            ),
            ExprKind::StructNew { struct_id, fields } => {
                let a: Vec<String> = fields.iter().map(|x| self.expr(x)).collect();
                format!("{}{{{}}}", self.types.struct_def(*struct_id).name, a.join(", "))
            }
            ExprKind::FieldGet { base, index } => format!("{}.{}", self.expr(base), index),
            ExprKind::EnumNew { enum_id, variant, fields } => {
                let def = self.types.enum_def(*enum_id);
                let a: Vec<String> = fields.iter().map(|x| self.expr(x)).collect();
                format!(
                    "{}.{}({})",
                    def.name,
                    def.variants[*variant as usize].name,
                    a.join(", ")
                )
            }
            ExprKind::Match { scrutinee, arms } => {
                format!("(match {} {} arms)", self.expr(scrutinee), arms.len())
            }
            ExprKind::Block(b) => format!("(block {} stmts)", b.stmts.len()),
            ExprKind::SliceNew { elems } => {
                let a: Vec<String> = elems.iter().map(|x| self.expr(x)).collect();
                format!("[{}]", a.join(", "))
            }
            ExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr(base), self.expr(index))
            }
            ExprKind::Nil => "nil".to_string(),
            ExprKind::PairNew { value, error } => {
                format!("({}, {})", self.expr(value), self.expr(error))
            }
            ExprKind::PairValue { base } => format!("{}.0", self.expr(base)),
            ExprKind::PairError { base } => format!("{}.1", self.expr(base)),
            ExprKind::ErrorNew { message } => format!("errors.new({})", self.expr(message)),
            ExprKind::ErrorMessage { base } => format!("{}.message()", self.expr(base)),
            ExprKind::IsNil { value } => format!("(is-nil {})", self.expr(value)),
            ExprKind::Wrap { value } => format!("(wrap {})", self.expr(value)),
            ExprKind::Unwrap { value } => format!("(unwrap {})", self.expr(value)),
            ExprKind::SliceLen { base } => format!("{}.len()", self.expr(base)),
            ExprKind::SliceGet { base, index } => {
                format!("{}.get({})", self.expr(base), self.expr(index))
            }
            ExprKind::Error => "<error>".to_string(),
        }
    }
}

fn indent(f: &mut fmt::Formatter<'_>, n: usize) -> fmt::Result {
    for _ in 0..n {
        write!(f, "  ")?;
    }
    Ok(())
}
