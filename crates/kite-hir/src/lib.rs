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

pub mod mono;
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
    /// Host functions the program declared. The glue is generated from these,
    /// so the boundary is written once, in Kite, and cannot drift.
    pub externs: Vec<ExternDef>,
    pub fns: Vec<Function>,
    /// The `main` function, if this program has one.
    pub entry: Option<FnId>,
    /// One per trait that is used as an object. Built here rather than in a
    /// backend so both backends dispatch over the same set in the same order.
    pub vtables: Vec<VTable>,
}

/// Everything a virtual call needs: which concrete types implement a trait, and
/// which function each one supplies for each of its methods.
#[derive(Clone, Debug)]
pub struct VTable {
    pub trait_id: TraitId,
    /// In `TypeTag` order, which is stable across compilations.
    pub entries: Vec<VTableEntry>,
}

#[derive(Clone, Debug)]
pub struct VTableEntry {
    pub tag: TypeTag,
    /// One function per trait method, in the trait's declaration order. A
    /// default method resolves to the trait's own body.
    pub methods: Vec<FnId>,
}

/// A concrete type as identified at run time. This is what a virtual call reads
/// off the receiver to pick a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeTag {
    Struct(StructId),
    Enum(EnumId),
}

impl TypeTag {
    /// A single integer identifying the type at run time. Enums are shifted
    /// clear of structs so the two id spaces cannot collide.
    pub fn encode(self) -> u32 {
        match self {
            TypeTag::Struct(s) => s.0,
            TypeTag::Enum(e) => 0x8000_0000 | e.0,
        }
    }
}

impl Program {
    pub fn vtable(&self, trait_id: TraitId) -> Option<&VTable> {
        self.vtables.iter().find(|v| v.trait_id == trait_id)
    }

    pub fn function(&self, id: FnId) -> &Function {
        &self.fns[id.index()]
    }
}

/// A declared host function.
#[derive(Clone, Debug)]
pub struct ExternDef {
    /// The `@host("…")` group. The glue puts a group together, so a host
    /// implements one object rather than a pile of loose functions.
    pub host: String,
    pub name: String,
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    /// A free function, as opposed to a method or a lifted closure body. Only
    /// these have names unique enough to export.
    pub is_free: bool,
    /// How many type parameters this function declares. Non-zero means it is a
    /// template: monomorphisation replaces it with one copy per instantiation,
    /// and no backend ever sees it.
    pub generic_count: usize,
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

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug)]
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
    /// `m[k] = v`. Maps are copy-on-write values too.
    MapSet { local: LocalId, key: Expr, value: Expr, span: Span },
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

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Local(LocalId),
    /// `targs` are the solved type arguments, in declaration order. Empty for
    /// a call to a function with no type parameters, which is nearly all of
    /// them — monomorphisation keys on this.
    Call { callee: FnId, args: Vec<Expr>, targs: Vec<TyId> },
    /// A call through a trait object. `args[0]` is the receiver; which function
    /// runs is decided at run time from its concrete type.
    CallVirtual { trait_id: TraitId, method: u32, args: Vec<Expr> },
    /// A concrete value standing where a `dyn Trait` is wanted. Explicit so the
    /// representation change is visible in the IR.
    ToDyn { value: Box<Expr>, trait_id: TraitId },
    /// A value rendered as text, from `\(expr)` in a string literal.
    ToStr { value: Box<Expr> },
    /// `x as float` / `x as int`. The only conversions Kite performs, and
    /// only when written: an `int` reaching a `float` context is an error, not
    /// a widening, because a silent one is how precision is lost unnoticed.
    Cast { value: Box<Expr>, to: TyId },
    /// A string operation. Every one is a host call — a `str` is the host's
    /// string, not bytes Kite owns — so they share a node rather than each
    /// getting one.
    StrOp { op: StrKind, args: Vec<Expr> },
    /// A closure value: the lifted function, plus the values it captured.
    /// Captures are by value and evaluated here, at the point the closure is
    /// made — not when it runs.
    ///
    /// `targs` are the enclosing function's type arguments. A closure written
    /// inside `f<T>` mentions `T`, and its lifted body is a separate function,
    /// so it is specialised alongside every instantiation of `f`.
    ClosureNew { func: FnId, captures: Vec<Expr>, targs: Vec<TyId> },
    /// Calling a function value. The callee's captures are passed ahead of the
    /// arguments, which is why a lifted function takes them as leading
    /// parameters rather than as a separate environment record.
    CallClosure { callee: Box<Expr>, args: Vec<Expr> },
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
    /// `(a, b)`. A positional record; the arena keeps its shape.
    TupleNew { elems: Vec<Expr> },
    /// `{"a": 1}`. Entries are flattened key, value, key, value — insertion
    /// order is part of the semantics, so the representation preserves it.
    MapNew { entries: Vec<Expr> },
    /// `m[k]`, which yields `Option<V>`: a missing key is a runtime condition,
    /// never a zero value.
    MapGet { base: Box<Expr>, key: Box<Expr> },
    MapLen { base: Box<Expr> },
    /// `m.keys()` and `m.values()`, in insertion order — which the
    /// specification guarantees, so both are ordinary slices and iterating one
    /// alongside the other lines up.
    MapKeys { base: Box<Expr> },
    MapValues { base: Box<Expr> },
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
    /// `await t` — the value of a task, once it has one.
    ///
    /// This never reaches a backend. The state-machine transform in MIR
    /// replaces it with a test of the task and, where it is not finished, a
    /// suspension: the enclosing function stores where it got to and returns
    /// to the scheduler.
    Await { value: Box<Expr> },
    /// `task.yield()` — suspend unconditionally and come back on the next
    /// sweep. This is the primitive `sleep`, `race` and `timeout` are written
    /// on top of, which is why they are Kite rather than compiler code.
    Yield,
    /// A call across the declared host boundary. `index` is into
    /// [`Program::externs`].
    CallExtern { index: u32, args: Vec<Expr> },
    /// A block in expression position — a `match` arm written with braces.
    /// Runs for its effects and produces unit.
    Block(Block),
    /// Produced where a type error was already reported. Poisons downstream
    /// checks so one mistake yields one diagnostic.
    Error,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// Locals the pattern binds, in the order the pattern introduces them.
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

/// A resolved pattern. Names are already bound to local slots, and variants to
/// positions, so lowering is a mechanical walk.
#[derive(Clone, Debug)]
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
    /// `(a, b)` — one sub-pattern per element. Carries the tuple's own type so
    /// lowering can name each element's type without a second lookup.
    Tuple { ty: TyId, elems: Vec<Pattern> },
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
            // A tuple has one constructor, so it matches everything exactly
            // when each of its elements does.
            Pattern::Tuple { elems, .. } => elems.iter().all(|e| e.is_irrefutable()),
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
    /// `draw.rect(x, y, w, h, colour)`
    DrawRect,
    /// `draw.rrect(x, y, w, h, radius, colour)` — a rounded rectangle, which
    /// no amount of square ones adds up to.
    DrawRRect,
    DrawFont,
    DrawDRRect,
    DrawAlpha,
    /// `draw.text(x, y, body, colour)`
    DrawText,
    /// `draw.field(x, y, w, h, value, hint, colour)` — a text input goes here. The only
    /// call whose renderers differ in kind: a real element on the DOM, drawn
    /// text everywhere else.
    DrawField,
    /// `text.width(body)` — a host call, because only the host has the font.
    TextWidth,
    /// `text.height()` — the host font's line height.
    TextHeight,
    /// `draw.clip(x, y, w, h)` / `draw.unclip()`. Clipping is the one thing a
    /// layout needs that is neither a rectangle nor a run of text.
    DrawClip,
    DrawUnclip,
    /// `task.spawn(poll)` — hand a task's resume closure to the scheduler.
    /// Emitted by the state-machine transform; no program writes it.
    TaskSpawn,
    /// `task.wake_at(ms)` — tell the scheduler not to poll the running task
    /// again before a deadline. A hint, not a guarantee: a scheduler is free
    /// to poll early, and the code that asked must re-check the clock.
    TaskWakeAt,
    /// `task.park()` — suspend until some other task finishes.
    TaskPark,
    /// `task.wait_host()` — suspend until the host has had a turn.
    TaskWaitHost,
    /// `time.now()` — milliseconds since the program started.
    TimeNow,
    /// `ptr.same(a, b)` — whether two references are one cell. Not a host
    /// call: every backend already has the comparison, it is `ref.eq` on
    /// WasmGC, an integer compare on native, and `Rc::ptr_eq` in the VM.
    PtrSame,
    /// `require(cond, message)` — trap when the claim is false. `assert`
    /// lowers to this too, in a debug build; in a release build it lowers to
    /// nothing at all.
    Require,
}

impl Builtin {
    pub fn path(self) -> &'static str {
        match self {
            Builtin::IoPrint => "io.print",
            Builtin::DrawRect => "draw.rect",
            Builtin::DrawRRect => "draw.rrect",
            Builtin::DrawFont => "draw.font",
            Builtin::DrawDRRect => "draw.drrect",
            Builtin::DrawAlpha => "draw.alpha",
            Builtin::DrawText => "draw.text",
            Builtin::DrawField => "draw.field",
            Builtin::TextWidth => "text.width",
            Builtin::TextHeight => "text.height",
            Builtin::DrawClip => "draw.clip",
            Builtin::DrawUnclip => "draw.unclip",
            Builtin::TaskSpawn => "task.spawn",
            Builtin::TaskWakeAt => "task.wake_at",
            Builtin::TaskPark => "task.park",
            Builtin::TaskWaitHost => "task.wait_host",
            Builtin::TimeNow => "time.now",
            Builtin::PtrSame => "ptr.same",
            Builtin::Require => "require",
        }
    }
}

/// What a program can ask of a string.
///
/// Deliberately few. `split`, `starts_with`, `replace` and the rest are
/// writable in Kite on top of these, and belong in the standard library where
/// they can be read — a host call is a boundary, and every one added is a
/// thing two runtimes have to agree about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrKind {
    /// Characters, not bytes: `"héllo"` is five either way only by accident of
    /// encoding, and text layout counts what a reader would call a character.
    Len,
    /// `s.slice(start, end)` — characters `start..end`, clamped to the string
    /// rather than trapping. An out-of-range slice is a runtime condition in
    /// text processing, not a program bug the way an out-of-range index is.
    Slice,
    /// `s.index_of(needle)` — the character index, or -1.
    IndexOf,
    /// `s.trim()` — leading and trailing whitespace removed.
    Trim,
    /// `s.code_at(i)` — the code point at character `i`, or -1 past the end.
    ///
    /// The one string primitive that is not itself a string operation, and it
    /// is here because it is the one thing no amount of `slice` and `index_of`
    /// can reach: without a way to see a character as a number, a hash, an
    /// ordering and a parser all have to be host calls of their own. One
    /// general primitive is cheaper than three special ones.
    CodeAt,
}

impl StrKind {
    pub fn name(self) -> &'static str {
        match self {
            StrKind::Len => "str.len",
            StrKind::Slice => "str.slice",
            StrKind::IndexOf => "str.index_of",
            StrKind::Trim => "str.trim",
            StrKind::CodeAt => "str.code_at",
        }
    }

    /// Including the receiver.
    pub fn arity(self) -> usize {
        match self {
            StrKind::Len | StrKind::Trim => 1,
            StrKind::IndexOf | StrKind::CodeAt => 2,
            StrKind::Slice => 3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    AddInt,
    SubInt,
    MulInt,
    /// The release-build forms of the three above. Section 3.1 has integer
    /// overflow trap in debug builds and wrap in release ones, and putting the
    /// choice in the operation rather than in each backend's configuration is
    /// what stops the three of them from deciding it separately — which is how
    /// debug Wasm came to wrap while debug bytecode trapped.
    AddIntWrap,
    SubIntWrap,
    MulIntWrap,
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
    /// Ordering on strings, by code point. A host call on the Wasm target,
    /// because a `str` there is the host's string and only the host can look
    /// inside one.
    LtStr,
    LeStr,
    GtStr,
    GeStr,
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
            Stmt::MapSet { local, key, value, .. } => {
                writeln!(f, "_{}[{}] = {}", local.0, self.expr(key), self.expr(value))
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
            ExprKind::Call { callee, args, .. } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("fn{}({})", callee.0, a.join(", "))
            }
            ExprKind::CallVirtual { trait_id, method, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!(
                    "virtual {}#{}({})",
                    self.types.trait_def(*trait_id).name,
                    method,
                    a.join(", ")
                )
            }
            ExprKind::ToStr { value } => format!("(str {})", self.expr(value)),
            ExprKind::StrOp { op, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("({} {})", op.name(), a.join(" "))
            }
            ExprKind::Cast { value, to } => {
                format!("({} as {})", self.expr(value), self.types.name(*to))
            }
            ExprKind::ClosureNew { func, captures, .. } => {
                let c: Vec<String> = captures.iter().map(|x| self.expr(x)).collect();
                format!("closure fn{}[{}]", func.0, c.join(", "))
            }
            ExprKind::CallClosure { callee, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                format!("{}({})", self.expr(callee), a.join(", "))
            }
            ExprKind::ToDyn { value, trait_id } => {
                format!("(as dyn {} {})", self.types.trait_def(*trait_id).name, self.expr(value))
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
            ExprKind::MapNew { entries } => format!("{{{} entries}}", entries.len() / 2),
            ExprKind::MapGet { base, key } => format!("{}[{}]", self.expr(base), self.expr(key)),
            ExprKind::MapLen { base } => format!("{}.len()", self.expr(base)),
            ExprKind::MapKeys { base } => format!("{}.keys()", self.expr(base)),
            ExprKind::MapValues { base } => format!("{}.values()", self.expr(base)),
            ExprKind::TupleNew { elems } => {
                let a: Vec<String> = elems.iter().map(|x| self.expr(x)).collect();
                format!("({})", a.join(", "))
            }
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
            ExprKind::Await { value } => format!("(await {})", self.expr(value)),
            ExprKind::Yield => "(yield)".to_string(),
            ExprKind::CallExtern { index, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr(x)).collect();
                let def = &self.externs[*index as usize];
                format!("host {}.{}({})", def.host, def.name, a.join(", "))
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
