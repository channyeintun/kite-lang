//! The Kite concrete syntax tree.
//!
//! Every node carries a [`Span`]. Literal *values* are not stored — they are
//! extracted from source via the node's span when a later pass needs them,
//! which keeps the tree small and keeps the source as the single truth.

use kite_span::Span;

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Ident {
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Ident { name: name.into(), span }
    }
}

// ---------------------------------------------------------------------------
// Compilation unit
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct SourceFile {
    pub uses: Vec<Use>,
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub struct Use {
    /// `use std/http` becomes `["std", "http"]`.
    pub path: Vec<Ident>,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    TypeAlias(TypeAlias),
    /// A declaration the parser could not recover into a real item.
    Error(Span),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Fn(f) => f.span,
            Item::Struct(s) => s.span,
            Item::Enum(e) => e.span,
            Item::Trait(t) => t.span,
            Item::Impl(i) => i.span,
            Item::TypeAlias(a) => a.span,
            Item::Error(s) => *s,
        }
    }

    /// The declared name, where the item has one. `impl` blocks do not.
    pub fn name(&self) -> Option<&Ident> {
        match self {
            Item::Fn(f) => Some(&f.name),
            Item::Struct(s) => Some(&s.name),
            Item::Enum(e) => Some(&e.name),
            Item::Trait(t) => Some(&t.name),
            Item::TypeAlias(a) => Some(&a.name),
            Item::Impl(_) | Item::Error(_) => None,
        }
    }

    /// What kind of thing this is, for diagnostics.
    pub fn describe(&self) -> &'static str {
        match self {
            Item::Fn(_) => "function",
            Item::Struct(_) => "struct",
            Item::Enum(_) => "enum",
            Item::Trait(_) => "trait",
            Item::Impl(_) => "impl block",
            Item::TypeAlias(_) => "type alias",
            Item::Error(_) => "item",
        }
    }
}

// ---------------------------------------------------------------------------
// Type declarations
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct StructDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FieldDecl {
    pub is_pub: bool,
    /// Declared `var`. Fields are immutable by default, which is what removes
    /// the value-versus-pointer distinction and makes most types `Share`.
    pub is_var: bool,
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug)]
pub struct EnumDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

#[derive(Debug)]
pub struct VariantDecl {
    pub name: Ident,
    pub payload: VariantPayload,
    pub span: Span,
}

#[derive(Debug)]
pub enum VariantPayload {
    /// `Point`
    Unit,
    /// `Circle(radius: float)`
    Named(Vec<FieldDecl>),
    /// `Number(float)`
    Positional(Vec<Type>),
}

impl VariantPayload {
    pub fn len(&self) -> usize {
        match self {
            VariantPayload::Unit => 0,
            VariantPayload::Named(f) => f.len(),
            VariantPayload::Positional(t) => t.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub struct TypeAlias {
    pub is_pub: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub ty: Type,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Traits and implementations
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TraitDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<MethodDecl>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ImplDecl {
    pub generics: Vec<GenericParam>,
    /// Present for `impl Trait for Type`; absent for an inherent `impl Type`.
    pub trait_path: Option<TypePath>,
    pub self_ty: TypePath,
    pub methods: Vec<MethodDecl>,
    pub span: Span,
}

#[derive(Debug)]
pub struct MethodDecl {
    pub is_pub: bool,
    pub is_async: bool,
    pub name: Ident,
    /// Absent for an associated function such as `Rect.square(2.0)`.
    pub self_param: Option<SelfParam>,
    pub params: Vec<Param>,
    pub ret: Option<RetType>,
    /// Absent for a trait method with no default body.
    pub body: Option<Block>,
    pub span: Span,
    pub sig_span: Span,
}

#[derive(Debug)]
pub struct SelfParam {
    /// `var self` — required to mutate a `var` field.
    pub is_var: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<TypePath>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FnDecl {
    pub is_pub: bool,
    pub is_async: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<RetType>,
    pub body: Block,
    pub span: Span,
    /// Span of just `fn name(...)`, so diagnostics about a signature do not
    /// underline the whole body.
    pub sig_span: Span,
}

#[derive(Debug)]
pub struct Param {
    pub is_var: bool,
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

/// A function's declared result.
#[derive(Debug)]
pub enum RetType {
    /// `-> T`
    Simple(Type),
    /// `-> (T, error)` — a correlated pair. The value is only meaningful when
    /// the error is nil; see the taint analysis.
    Fallible { value: Type, span: Span },
}

impl RetType {
    pub fn span(&self) -> Span {
        match self {
            RetType::Simple(t) => t.span(),
            RetType::Fallible { span, .. } => *span,
        }
    }

    pub fn is_fallible(&self) -> bool {
        matches!(self, RetType::Fallible { .. })
    }

    pub fn value_type(&self) -> &Type {
        match self {
            RetType::Simple(t) => t,
            RetType::Fallible { value, .. } => value,
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Type {
    /// `int`, `Point`, `Cache<K, V>`, `mod.Type`
    Path(TypePath),
    /// `Option<T>`. Spelled as a word rather than a sigil: Kite has no `?`
    /// operator of any kind, and a type that may be absent should say so.
    Optional { inner: Box<Type>, span: Span },
    /// `[T]`
    Slice { elem: Box<Type>, span: Span },
    /// `{K: V}`
    Map { key: Box<Type>, value: Box<Type>, span: Span },
    /// `(A, B)`
    Tuple { elems: Vec<Type>, span: Span },
    /// `fn(A) -> B`
    Fn { params: Vec<Type>, ret: Option<Box<Type>>, span: Span },
    /// `dyn Trait`
    Dyn { path: TypePath, span: Span },
    Error(Span),
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Path(p) => p.span,
            Type::Optional { span, .. }
            | Type::Slice { span, .. }
            | Type::Map { span, .. }
            | Type::Tuple { span, .. }
            | Type::Fn { span, .. }
            | Type::Dyn { span, .. }
            | Type::Error(span) => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TypePath {
    pub segments: Vec<Ident>,
    pub args: Vec<Type>,
    pub span: Span,
}

impl TypePath {
    /// The final segment — the type's own name.
    pub fn name(&self) -> &str {
        &self.segments.last().expect("type path is never empty").name
    }

    pub fn is_simple(&self) -> bool {
        self.segments.len() == 1 && self.args.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Stmt {
    Let(LetStmt),
    Var(VarStmt),
    Assign(AssignStmt),
    Return(ReturnStmt),
    Check { expr: Expr, span: Span },
    Defer { expr: Expr, span: Span },
    If(IfStmt),
    For(ForStmt),
    Match(MatchExpr),
    Break { label: Option<Ident>, span: Span },
    Continue { label: Option<Ident>, span: Span },
    Expr(Expr),
    Error(Span),
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let(s) => s.span,
            Stmt::Var(s) => s.span,
            Stmt::Assign(s) => s.span,
            Stmt::Return(s) => s.span,
            Stmt::Check { span, .. }
            | Stmt::Defer { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Error(span) => *span,
            Stmt::If(s) => s.span,
            Stmt::For(s) => s.span,
            Stmt::Match(m) => m.span,
            Stmt::Expr(e) => e.span(),
        }
    }
}

#[derive(Debug)]
pub struct LetStmt {
    pub binding: Binding,
    pub ty: Option<Type>,
    /// Absent for deferred initialisation: `let x: int` then assigned in
    /// branches. Definite-assignment analysis proves exactly one write.
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Debug)]
pub struct VarStmt {
    pub name: Ident,
    pub ty: Option<Type>,
    pub init: Expr,
    pub span: Span,
}

/// The left side of a `let`. The tuple form is how `(T, error)` results bind.
#[derive(Debug)]
pub enum Binding {
    Name(Ident),
    /// `let (value, err) = f()`
    Tuple { elems: Vec<BindElem>, span: Span },
}

impl Binding {
    pub fn span(&self) -> Span {
        match self {
            Binding::Name(i) => i.span,
            Binding::Tuple { span, .. } => *span,
        }
    }
}

#[derive(Debug)]
pub enum BindElem {
    Name(Ident),
    /// `_` — discards the value. Discarding an *error* this way is rejected by
    /// the taint analysis; only the value slot may be dropped.
    Wildcard(Span),
}

impl BindElem {
    pub fn span(&self) -> Span {
        match self {
            BindElem::Name(i) => i.span,
            BindElem::Wildcard(s) => *s,
        }
    }
}

#[derive(Debug)]
pub struct AssignStmt {
    pub target: Expr,
    pub op: AssignOp,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl AssignOp {
    pub fn text(self) -> &'static str {
        match self {
            AssignOp::Assign => "=",
            AssignOp::Add => "+=",
            AssignOp::Sub => "-=",
            AssignOp::Mul => "*=",
            AssignOp::Div => "/=",
            AssignOp::Rem => "%=",
        }
    }

    /// The binary operation an compound assignment expands to.
    pub fn to_binary(self) -> Option<BinaryOp> {
        Some(match self {
            AssignOp::Assign => return None,
            AssignOp::Add => BinaryOp::Add,
            AssignOp::Sub => BinaryOp::Sub,
            AssignOp::Mul => BinaryOp::Mul,
            AssignOp::Div => BinaryOp::Div,
            AssignOp::Rem => BinaryOp::Rem,
        })
    }
}

#[derive(Debug)]
pub struct ReturnStmt {
    pub value: Option<ReturnValue>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ReturnValue {
    /// `return expr`
    Single(Expr),
    /// `return value, nil` — the success arm of a fallible function.
    Pair { value: Expr, error: Expr, span: Span },
    /// `return _, err` — the failure arm. There is deliberately no value on
    /// this path, which is what stops Go's zero-value leak.
    Fail { error: Expr, span: Span },
}

#[derive(Debug)]
pub struct IfStmt {
    pub cond: Expr,
    pub then: Block,
    pub else_: Option<Box<ElseBranch>>,
    pub span: Span,
}

#[derive(Debug)]
pub enum ElseBranch {
    Block(Block),
    If(IfStmt),
}

#[derive(Debug)]
pub struct ForStmt {
    pub label: Option<Ident>,
    pub header: ForHeader,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug)]
pub enum ForHeader {
    /// `for x in xs`
    In { binding: Binding, iter: Expr },
    /// `for cond`
    While(Expr),
    /// `for`
    Loop,
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// One piece of an interpolated string literal.
#[derive(Debug)]
pub enum StrPart {
    /// A run of literal text, spanning the source between holes. Escapes are
    /// still encoded; they are decoded where a plain literal's are.
    Text(Span),
    Hole(Expr),
}

#[derive(Debug)]
pub enum Expr {
    Int(Span),
    Float(Span),
    /// Span covers the quotes. A literal with no `\(...)` in it.
    Str(Span),
    /// A string literal containing at least one `\(expr)`. The parser has
    /// already split it, so nothing downstream re-scans the text.
    Interpolated { parts: Vec<StrPart>, span: Span },
    Char(Span),
    Bool { value: bool, span: Span },
    Nil(Span),
    /// A bare name, or a dotted path such as `io.print`.
    Path(Path),
    SelfExpr(Span),
    Unary { op: UnaryOp, operand: Box<Expr>, span: Span },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    /// `f(a, b)`, or `Circle(radius: 2.0)` for a named-payload variant.
    /// `arg_names` is the same length as `args`; entries are `None` for
    /// positional arguments.
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        arg_names: Vec<Option<Ident>>,
        span: Span,
    },
    Field { base: Box<Expr>, name: Ident, span: Span },
    Index { base: Box<Expr>, index: Box<Expr>, span: Span },
    Range { start: Box<Expr>, end: Box<Expr>, inclusive: bool, span: Span },
    /// `if c { a } else { b }` used for its value.
    If { cond: Box<Expr>, then: Block, else_: Box<ElseBranch>, span: Span },
    Cast { expr: Box<Expr>, ty: Type, span: Span },
    Await { expr: Box<Expr>, span: Span },
    Paren { inner: Box<Expr>, span: Span },
    Tuple { elems: Vec<Expr>, span: Span },
    Slice { elems: Vec<Expr>, span: Span },
    /// `{"a": 1}`
    Map { entries: Vec<MapEntry>, span: Span },
    /// `Point{ x: 1.0, y: 2.0 }`, optionally with `..base`.
    StructLit(StructLit),
    Match(MatchExpr),
    Closure {
        params: Vec<ClosureParam>,
        /// `|x| -> T { … }`. Needed for a block body used where no expected
        /// type says what it returns.
        ret: Option<Box<Type>>,
        body: Box<ClosureBody>,
        span: Span,
    },
    Error(Span),
}

#[derive(Debug)]
pub struct MapEntry {
    pub key: Expr,
    pub value: Expr,
}

#[derive(Debug)]
pub struct StructLit {
    pub path: TypePath,
    /// `Point{ ..p, y: 5.0 }` — produces a new value, never mutates `p`.
    pub base: Option<Box<Expr>>,
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug)]
pub struct MatchExpr {
    pub scrutinee: Box<Expr>,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// `Rect(w, h) if w == h => ...`
    pub guard: Option<Expr>,
    pub body: MatchBody,
    pub span: Span,
}

#[derive(Debug)]
pub enum MatchBody {
    Expr(Expr),
    Block(Block),
}

impl MatchBody {
    pub fn span(&self) -> Span {
        match self {
            MatchBody::Expr(e) => e.span(),
            MatchBody::Block(b) => b.span,
        }
    }
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Pattern {
    /// `_`
    Wildcard(Span),
    Nil(Span),
    /// An integer, float, string, char, or boolean literal.
    Literal(Expr),
    /// `4..=9`
    Range { start: Expr, end: Expr, inclusive: bool, span: Span },
    /// A lowercase name binds; an uppercase one may name a unit variant, which
    /// resolution decides.
    Binding(Ident),
    /// `Circle(radius)` or `Circle(radius: r)`
    Variant { path: TypePath, args: PatternArgs, span: Span },
    /// `Point{ x: 0.0, y }`
    Struct { path: TypePath, fields: Vec<FieldPattern>, rest: bool, span: Span },
    /// `(a, b)`
    Tuple { elems: Vec<Pattern>, span: Span },
    /// `1 | 2 | 3`
    Or { alts: Vec<Pattern>, span: Span },
    Error(Span),
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(s) | Pattern::Nil(s) | Pattern::Error(s) => *s,
            Pattern::Literal(e) => e.span(),
            Pattern::Range { span, .. }
            | Pattern::Variant { span, .. }
            | Pattern::Struct { span, .. }
            | Pattern::Tuple { span, .. }
            | Pattern::Or { span, .. } => *span,
            Pattern::Binding(i) => i.span,
        }
    }

    /// Whether this pattern matches every value, which is what makes a `match`
    /// exhaustive without listing variants.
    pub fn is_irrefutable(&self) -> bool {
        match self {
            Pattern::Wildcard(_) | Pattern::Binding(_) => true,
            Pattern::Or { alts, .. } => alts.iter().any(|a| a.is_irrefutable()),
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum PatternArgs {
    Positional(Vec<Pattern>),
    Named(Vec<(Ident, Pattern)>),
}

impl PatternArgs {
    pub fn len(&self) -> usize {
        match self {
            PatternArgs::Positional(p) => p.len(),
            PatternArgs::Named(p) => p.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub struct FieldPattern {
    pub name: Ident,
    /// Absent for the shorthand `Point{ x }`, which binds `x`.
    pub pattern: Option<Pattern>,
    pub span: Span,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(s)
            | Expr::Float(s)
            | Expr::Str(s)
            | Expr::Char(s)
            | Expr::Nil(s)
            | Expr::SelfExpr(s)
            | Expr::Error(s) => *s,
            Expr::Bool { span, .. }
            | Expr::Interpolated { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Field { span, .. }
            | Expr::Index { span, .. }
            | Expr::Range { span, .. }
            | Expr::If { span, .. }
            | Expr::Cast { span, .. }
            | Expr::Await { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Tuple { span, .. }
            | Expr::Slice { span, .. }
            | Expr::Map { span, .. }
            | Expr::Closure { span, .. } => *span,
            Expr::Path(p) => p.span,
            Expr::StructLit(s) => s.span,
            Expr::Match(m) => m.span,
        }
    }

    /// Whether this expression may appear on the left of an assignment.
    pub fn is_place(&self) -> bool {
        match self {
            Expr::Path(p) => p.is_simple(),
            Expr::Field { .. } => true,
            Expr::Index { .. } => true,
            _ => false,
        }
    }

    /// A short rendering for diagnostics.
    pub fn describe(&self) -> &'static str {
        match self {
            Expr::Int(_) => "an integer literal",
            Expr::Float(_) => "a float literal",
            Expr::Str(_) => "a string literal",
            Expr::Char(_) => "a character literal",
            Expr::Bool { .. } => "a boolean literal",
            Expr::Nil(_) => "`nil`",
            Expr::Path(_) => "a name",
            Expr::Call { .. } => "a call",
            Expr::Binary { .. } => "a binary expression",
            Expr::Unary { .. } => "a unary expression",
            Expr::Range { .. } => "a range",
            Expr::StructLit(_) => "a struct literal",
            Expr::Match(_) => "a match",
            Expr::Map { .. } => "a map literal",
            Expr::Closure { .. } => "a closure",
            _ => "an expression",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Path {
    pub segments: Vec<Ident>,
    pub span: Span,
}

impl Path {
    pub fn is_simple(&self) -> bool {
        self.segments.len() == 1
    }

    pub fn last(&self) -> &Ident {
        self.segments.last().expect("path is never empty")
    }

    /// Dotted rendering, for messages.
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Debug)]
pub struct ClosureParam {
    pub name: Ident,
    pub ty: Option<Type>,
}

#[derive(Debug)]
pub enum ClosureBody {
    Expr(Expr),
    Block(Block),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
    Neg,
    Not,
}

impl UnaryOp {
    pub fn text(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinaryOp {
    pub fn text(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Shl => "<<",
            Shr => ">>",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            And => "&&",
            Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        use BinaryOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }

    pub fn is_arithmetic(self) -> bool {
        use BinaryOp::*;
        matches!(self, Add | Sub | Mul | Div | Rem)
    }

    pub fn is_bitwise(self) -> bool {
        use BinaryOp::*;
        matches!(self, BitAnd | BitOr | BitXor | Shl | Shr)
    }

    pub fn is_logical(self) -> bool {
        matches!(self, BinaryOp::And | BinaryOp::Or)
    }
}
