//! The type representation.
//!
//! Types are **interned**: a [`TyId`] is a `u32` index into [`Types`], so
//! copying is free and equality is an integer compare. Structural types such as
//! `[int]` are interned too, which means two independently constructed `[int]`
//! types share one id and compare equal without a recursive walk.
//!
//! Nominal types — structs, enums, traits — are declared in two steps
//! ([`Types::declare_struct`] then [`Types::set_struct_fields`]) so that mutually
//! recursive definitions can refer to each other. Every Kite aggregate is a GC
//! reference, so recursion needs no boxing annotation from the user.

use kite_span::Span;
use std::collections::HashMap;

macro_rules! def_id {
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

def_id!(TyId, "An interned type.");
def_id!(StructId, "A declared struct.");
def_id!(EnumId, "A declared enum.");
def_id!(TraitId, "A declared trait.");

impl TyId {
    // Primitives occupy fixed ids so they need no lookup.
    pub const UNIT: TyId = TyId(0);
    pub const BOOL: TyId = TyId(1);
    pub const INT: TyId = TyId(2);
    pub const FLOAT: TyId = TyId(3);
    pub const STR: TyId = TyId(4);
    pub const NEVER: TyId = TyId(5);
    pub const ERROR: TyId = TyId(6);
    /// The `error` type. Distinct from [`TyId::ERROR`], which is compiler
    /// poison: this one is a value a program can hold.
    pub const ERR: TyId = TyId(7);

    const PRIMITIVE_COUNT: usize = 8;
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
    Unit,
    Bool,
    /// 64-bit signed. The default integer.
    Int,
    /// 64-bit IEEE-754. The default float.
    Float,
    /// Immutable UTF-8 string. A JS string reference on the Wasm target.
    Str,
    /// The type of an expression that never produces a value: `return`,
    /// `break`, `continue`. Satisfies every expectation.
    Never,
    /// Poison, produced where an error was already reported. Compatible with
    /// everything so one mistake yields one diagnostic.
    Error,
    /// The `error` type: nil, or a value describing a failure. Kite's errors
    /// are ordinary values, never exceptions.
    Err,
    /// The result of a fallible function, `(T, error)`. A **correlated pair**:
    /// the value is only meaningful when the error is nil, which is what the
    /// taint analysis enforces.
    Fallible(TyId),

    Struct(StructId),
    Enum(EnumId),
    /// `[T]`
    Slice(TyId),
    /// `{K: V}`
    Map(TyId, TyId),
    /// `?T`
    Optional(TyId),
    /// `(A, B)`
    Tuple(Vec<TyId>),
    /// `fn(A) -> B`
    Fn { params: Vec<TyId>, ret: TyId },
    /// `dyn Trait`
    Dyn(TraitId),
    /// A generic parameter, before monomorphisation. `index` is its position in
    /// the enclosing declaration's parameter list.
    Param { index: u32, name: &'static str },
}

// ---------------------------------------------------------------------------
// Nominal definitions
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct StructDef {
    pub name: String,
    pub is_pub: bool,
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

impl StructDef {
    pub fn field(&self, name: &str) -> Option<(usize, &FieldDef)> {
        self.fields.iter().enumerate().find(|(_, f)| f.name == name)
    }
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    /// Declared `var`. Immutability is the default, which is what makes most
    /// types automatically shareable across tasks.
    pub mutable: bool,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug)]
pub struct EnumDef {
    pub name: String,
    pub is_pub: bool,
    pub variants: Vec<VariantDef>,
    pub span: Span,
}

impl EnumDef {
    pub fn variant(&self, name: &str) -> Option<(usize, &VariantDef)> {
        self.variants.iter().enumerate().find(|(_, v)| v.name == name)
    }
}

#[derive(Debug)]
pub struct VariantDef {
    pub name: String,
    /// Empty for a unit variant such as `Point`.
    pub fields: Vec<FieldDef>,
    /// Whether the payload was written with names — `Circle(radius: float)`
    /// rather than `Circle(float)`. Affects which patterns are accepted.
    pub named: bool,
    pub span: Span,
}

impl VariantDef {
    pub fn is_unit(&self) -> bool {
        self.fields.is_empty()
    }
}

#[derive(Debug)]
pub struct TraitDef {
    pub name: String,
    pub is_pub: bool,
    pub methods: Vec<TraitMethodDef>,
    pub span: Span,
}

impl TraitDef {
    pub fn method(&self, name: &str) -> Option<(usize, &TraitMethodDef)> {
        self.methods.iter().enumerate().find(|(_, m)| m.name == name)
    }
}

#[derive(Debug)]
pub struct TraitMethodDef {
    pub name: String,
    /// Excluding `self`.
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub takes_self: bool,
    /// Whether the trait supplied a body.
    pub has_default: bool,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// The arena
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Types {
    kinds: Vec<TyKind>,
    index: HashMap<TyKind, TyId>,
    structs: Vec<StructDef>,
    enums: Vec<EnumDef>,
    traits: Vec<TraitDef>,
    /// Parameter names, interned so a `TyKind` can hold a `&'static str` and
    /// stay `Hash` — which is what lets the arena deduplicate parameters.
    param_names: Vec<&'static str>,
}

impl Default for Types {
    fn default() -> Self {
        Types::new()
    }
}

impl Types {
    pub fn new() -> Self {
        let kinds = vec![
            TyKind::Unit,
            TyKind::Bool,
            TyKind::Int,
            TyKind::Float,
            TyKind::Str,
            TyKind::Never,
            TyKind::Error,
            TyKind::Err,
        ];
        debug_assert_eq!(kinds.len(), TyId::PRIMITIVE_COUNT);
        let index = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), TyId(i as u32)))
            .collect();
        Types {
            kinds,
            index,
            structs: Vec::new(),
            enums: Vec::new(),
            traits: Vec::new(),
            param_names: Vec::new(),
        }
    }

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.index.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.index.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TyId) -> &TyKind {
        &self.kinds[id.index()]
    }

    // ---- convenience constructors ----------------------------------------

    pub fn slice_of(&mut self, elem: TyId) -> TyId {
        self.intern(TyKind::Slice(elem))
    }

    pub fn map_of(&mut self, key: TyId, value: TyId) -> TyId {
        self.intern(TyKind::Map(key, value))
    }

    pub fn optional_of(&mut self, inner: TyId) -> TyId {
        // `Option<Option<T>>` is just `Option<T>`; flattening keeps the
        // representation canonical.
        match self.kind(inner) {
            TyKind::Optional(_) => inner,
            _ => self.intern(TyKind::Optional(inner)),
        }
    }

    pub fn tuple_of(&mut self, elems: Vec<TyId>) -> TyId {
        self.intern(TyKind::Tuple(elems))
    }

    pub fn fn_of(&mut self, params: Vec<TyId>, ret: TyId) -> TyId {
        self.intern(TyKind::Fn { params, ret })
    }

    pub fn struct_ty(&mut self, id: StructId) -> TyId {
        self.intern(TyKind::Struct(id))
    }

    pub fn enum_ty(&mut self, id: EnumId) -> TyId {
        self.intern(TyKind::Enum(id))
    }

    pub fn dyn_ty(&mut self, id: TraitId) -> TyId {
        self.intern(TyKind::Dyn(id))
    }

    /// A generic parameter. Interned by index and name, so the same parameter
    /// of the same declaration is always the same `TyId`.
    /// An existing function type, without interning a new one. Used where the
    /// arena is shared and immutable — a backend, which can only ever ask about
    /// types the front end already made.
    pub fn find_fn(&self, params: &[TyId], ret: TyId) -> Option<TyId> {
        self.index
            .get(&TyKind::Fn { params: params.to_vec(), ret })
            .copied()
    }

    pub fn param_ty(&mut self, index: u32, name: &str) -> TyId {
        let name: &'static str = match self.param_names.iter().find(|n| **n == name) {
            Some(n) => n,
            None => {
                let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
                self.param_names.push(leaked);
                leaked
            }
        };
        self.intern(TyKind::Param { index, name })
    }

    pub fn fallible_of(&mut self, value: TyId) -> TyId {
        self.intern(TyKind::Fallible(value))
    }

    /// The value type carried by a fallible result.
    pub fn fallible_value(&self, id: TyId) -> Option<TyId> {
        match self.kind(id) {
            TyKind::Fallible(v) => Some(*v),
            _ => None,
        }
    }

    // ---- nominal declarations --------------------------------------------

    /// Reserve a struct so other declarations can refer to it before its
    /// fields are known. Mutually recursive types need this.
    pub fn declare_struct(&mut self, name: impl Into<String>, is_pub: bool, span: Span) -> StructId {
        let id = StructId(self.structs.len() as u32);
        self.structs.push(StructDef {
            name: name.into(),
            is_pub,
            fields: Vec::new(),
            span,
        });
        id
    }

    pub fn set_struct_fields(&mut self, id: StructId, fields: Vec<FieldDef>) {
        self.structs[id.index()].fields = fields;
    }

    pub fn struct_def(&self, id: StructId) -> &StructDef {
        &self.structs[id.index()]
    }

    pub fn declare_enum(&mut self, name: impl Into<String>, is_pub: bool, span: Span) -> EnumId {
        let id = EnumId(self.enums.len() as u32);
        self.enums.push(EnumDef {
            name: name.into(),
            is_pub,
            variants: Vec::new(),
            span,
        });
        id
    }

    pub fn set_enum_variants(&mut self, id: EnumId, variants: Vec<VariantDef>) {
        self.enums[id.index()].variants = variants;
    }

    pub fn enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enums[id.index()]
    }

    pub fn declare_trait(&mut self, name: impl Into<String>, is_pub: bool, span: Span) -> TraitId {
        let id = TraitId(self.traits.len() as u32);
        self.traits.push(TraitDef {
            name: name.into(),
            is_pub,
            methods: Vec::new(),
            span,
        });
        id
    }

    pub fn set_trait_methods(&mut self, id: TraitId, methods: Vec<TraitMethodDef>) {
        self.traits[id.index()].methods = methods;
    }

    pub fn trait_def(&self, id: TraitId) -> &TraitDef {
        &self.traits[id.index()]
    }

    pub fn struct_count(&self) -> usize {
        self.structs.len()
    }

    pub fn enum_count(&self) -> usize {
        self.enums.len()
    }

    pub fn trait_count(&self) -> usize {
        self.traits.len()
    }

    // ---- queries ----------------------------------------------------------

    /// Whether a value of type `found` is acceptable where `expected` is
    /// required.
    ///
    /// Kite performs no implicit conversion, so this is identity plus two
    /// escape hatches: `Never` satisfies anything because it never produces a
    /// value, and `Error` satisfies anything to stop cascades.
    pub fn satisfies(&self, found: TyId, expected: TyId) -> bool {
        if found == expected
            || found == TyId::NEVER
            || found == TyId::ERROR
            || expected == TyId::ERROR
        {
            return true;
        }
        // A `T` is acceptable where a `?T` is wanted. This is subsumption, not
        // a conversion: the value is unchanged and nothing is lost. Without it
        // every optional would need an explicit wrap at every site, which is
        // why every language with optionals has this rule.
        if matches!(self.kind(expected), TyKind::Optional(inner) if *inner == found) {
            return true;
        }
        // `error` is itself nil-able: nil *is* the no-error value. That is what
        // makes `return value, nil` read the way it does.
        expected == TyId::ERR && found == TyId::ERROR
    }

    pub fn is_poisoned(&self, id: TyId) -> bool {
        id == TyId::ERROR || id == TyId::NEVER
    }

    pub fn is_numeric(&self, id: TyId) -> bool {
        id == TyId::INT || id == TyId::FLOAT
    }

    /// Whether `==` and `!=` are defined. Structural for aggregates, per the
    /// specification: two structs are equal when their fields are.
    pub fn is_equatable(&self, id: TyId) -> bool {
        match self.kind(id) {
            TyKind::Int | TyKind::Float | TyKind::Bool | TyKind::Str | TyKind::Err => true,
            TyKind::Optional(inner) => self.is_equatable(*inner),
            TyKind::Slice(elem) => self.is_equatable(*elem),
            TyKind::Tuple(elems) => elems.iter().all(|e| self.is_equatable(*e)),
            TyKind::Struct(s) => self
                .struct_def(*s)
                .fields
                .iter()
                .all(|f| self.is_equatable(f.ty)),
            TyKind::Enum(e) => self
                .enum_def(*e)
                .variants
                .iter()
                .all(|v| v.fields.iter().all(|f| self.is_equatable(f.ty))),
            _ => false,
        }
    }

    /// Whether `<`, `<=`, `>`, `>=` are defined.
    pub fn is_ordered(&self, id: TyId) -> bool {
        matches!(self.kind(id), TyKind::Int | TyKind::Float | TyKind::Str)
    }

    /// Whether `io.print` accepts this type. The real `Display` trait replaces
    /// this in Phase 6.
    pub fn is_printable(&self, id: TyId) -> bool {
        matches!(
            self.kind(id),
            TyKind::Int | TyKind::Float | TyKind::Bool | TyKind::Str
        )
    }

    /// Whether a value of this type is a heap reference at run time. Drives
    /// codegen. Slices are excluded: they are heap-allocated but have value
    /// semantics, so assignment does not alias.
    pub fn is_reference(&self, id: TyId) -> bool {
        matches!(
            self.kind(id),
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Map(..) | TyKind::Dyn(_) | TyKind::Fn { .. }
        )
    }

    /// The element type of a slice.
    pub fn slice_elem(&self, id: TyId) -> Option<TyId> {
        match self.kind(id) {
            TyKind::Slice(e) => Some(*e),
            _ => None,
        }
    }

    /// Whether a type is deeply immutable, and so may cross a task boundary.
    ///
    /// This is the `Share` rule from the specification. It is checked here
    /// rather than in a later pass because the answer is purely structural.
    /// Because struct fields are immutable by default, most user types satisfy
    /// it without the author doing anything.
    pub fn is_share(&self, id: TyId) -> bool {
        self.is_share_inner(id, &mut Vec::new())
    }

    fn is_share_inner(&self, id: TyId, visiting: &mut Vec<TyId>) -> bool {
        // A recursive type is Share when nothing else disqualifies it; assume
        // yes on the back edge and let the rest of the walk decide.
        if visiting.contains(&id) {
            return true;
        }
        visiting.push(id);
        let result = match self.kind(id) {
            TyKind::Unit
            | TyKind::Bool
            | TyKind::Int
            | TyKind::Float
            | TyKind::Str
            | TyKind::Never
            | TyKind::Error
            | TyKind::Err => true,
            // Slices are copy-on-write *values*, not shared references, so a
            // slice is shareable exactly when its elements are. This is what
            // keeps ordinary data types `Share` without the author noticing.
            TyKind::Slice(e) | TyKind::Optional(e) => self.is_share_inner(*e, visiting),
            TyKind::Map(k, v) => {
                self.is_share_inner(*k, visiting) && self.is_share_inner(*v, visiting)
            }
            TyKind::Tuple(elems) => elems.iter().all(|e| self.is_share_inner(*e, visiting)),
            TyKind::Struct(s) => self
                .struct_def(*s)
                .fields
                .iter()
                .all(|f| !f.mutable && self.is_share_inner(f.ty, visiting)),
            TyKind::Enum(e) => self.enum_def(*e).variants.iter().all(|v| {
                v.fields
                    .iter()
                    .all(|f| !f.mutable && self.is_share_inner(f.ty, visiting))
            }),
            // A closure may capture a mutable binding, and a trait object's
            // concrete type is not known here.
            TyKind::Fn { .. } | TyKind::Dyn(_) => false,
            TyKind::Fallible(v) => self.is_share_inner(*v, visiting),
            TyKind::Param { .. } => false,
        };
        visiting.pop();
        result
    }

    // ---- rendering --------------------------------------------------------

    /// The type's surface syntax, for diagnostics.
    pub fn name(&self, id: TyId) -> String {
        match self.kind(id) {
            TyKind::Unit => "()".into(),
            TyKind::Bool => "bool".into(),
            TyKind::Int => "int".into(),
            TyKind::Float => "float".into(),
            TyKind::Str => "str".into(),
            TyKind::Never => "!".into(),
            TyKind::Error => "<error>".into(),
            TyKind::Err => "error".into(),
            TyKind::Fallible(v) => format!("({}, error)", self.name(*v)),
            TyKind::Struct(s) => self.struct_def(*s).name.clone(),
            TyKind::Enum(e) => self.enum_def(*e).name.clone(),
            TyKind::Slice(t) => format!("[{}]", self.name(*t)),
            TyKind::Map(k, v) => format!("{{{}: {}}}", self.name(*k), self.name(*v)),
            TyKind::Optional(t) => format!("Option<{}>", self.name(*t)),
            TyKind::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| self.name(*t)).collect();
                format!("({})", inner.join(", "))
            }
            TyKind::Fn { params, ret } => {
                let ps: Vec<String> = params.iter().map(|t| self.name(*t)).collect();
                if *ret == TyId::UNIT {
                    format!("fn({})", ps.join(", "))
                } else {
                    format!("fn({}) -> {}", ps.join(", "), self.name(*ret))
                }
            }
            TyKind::Dyn(t) => format!("dyn {}", self.trait_def(*t).name),
            TyKind::Param { name, .. } => (*name).to_string(),
        }
    }

    /// The type's name with its indefinite article, for prose in diagnostics:
    /// "this is an `int`", not "this is a `int`".
    pub fn with_article(&self, id: TyId) -> String {
        let name = self.name(id);
        // "an int", but "a User" — `u` almost always reads as a consonant at
        // the start of an English word.
        let article = match name.chars().next() {
            Some(c) if "aeioAEIO".contains(c) => "an",
            _ => "a",
        };
        format!("{} `{}`", article, name)
    }

    /// Names in scope, for "did you mean" suggestions on an unknown type.
    pub fn known_type_names(&self) -> Vec<String> {
        let mut names: Vec<String> = ["bool", "int", "float", "str"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        names.extend(self.structs.iter().map(|s| s.name.clone()));
        names.extend(self.enums.iter().map(|e| e.name.clone()));
        names
    }

    /// Resolve a primitive type name.
    pub fn primitive_from_name(name: &str) -> Option<TyId> {
        Some(match name {
            "bool" => TyId::BOOL,
            "int" => TyId::INT,
            "float" => TyId::FLOAT,
            "str" => TyId::STR,
            "error" => TyId::ERR,
            _ => return None,
        })
    }

    pub const PRIMITIVE_NAMES: [&'static str; 5] = ["bool", "int", "float", "str", "error"];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span::new(kite_span::FileId(0), 0, 0)
    }

    #[test]
    fn primitives_have_fixed_ids() {
        let t = Types::new();
        assert_eq!(t.kind(TyId::INT), &TyKind::Int);
        assert_eq!(t.kind(TyId::STR), &TyKind::Str);
        assert_eq!(t.name(TyId::FLOAT), "float");
    }

    #[test]
    fn structural_types_are_interned() {
        let mut t = Types::new();
        let a = t.slice_of(TyId::INT);
        let b = t.slice_of(TyId::INT);
        assert_eq!(a, b, "two `[int]` types must share one id");
        let c = t.slice_of(TyId::STR);
        assert_ne!(a, c);
    }

    #[test]
    fn nested_optionals_collapse() {
        let mut t = Types::new();
        let a = t.optional_of(TyId::INT);
        let b = t.optional_of(a);
        assert_eq!(a, b, "`Option<Option<T>>` must canonicalise to `Option<T>`");
    }

    #[test]
    fn never_and_error_satisfy_everything() {
        let mut t = Types::new();
        let s = t.slice_of(TyId::INT);
        for target in [TyId::INT, TyId::STR, s] {
            assert!(t.satisfies(TyId::NEVER, target));
            assert!(t.satisfies(TyId::ERROR, target));
            assert!(t.satisfies(target, TyId::ERROR));
        }
    }

    #[test]
    fn no_implicit_numeric_conversion() {
        let t = Types::new();
        assert!(!t.satisfies(TyId::INT, TyId::FLOAT));
        assert!(!t.satisfies(TyId::FLOAT, TyId::INT));
    }

    /// A `T` is acceptable where a `?T` is wanted, but not the reverse.
    #[test]
    fn a_value_widens_to_its_optional() {
        let mut t = Types::new();
        let opt = t.optional_of(TyId::INT);
        assert!(t.satisfies(TyId::INT, opt));
        assert!(!t.satisfies(opt, TyId::INT), "an optional must be unwrapped");
        let other = t.optional_of(TyId::STR);
        assert!(!t.satisfies(TyId::INT, other));
    }

    #[test]
    fn type_names_render_as_surface_syntax() {
        let mut t = Types::new();
        let s = t.slice_of(TyId::INT);
        assert_eq!(t.name(s), "[int]");
        let m = t.map_of(TyId::STR, TyId::INT);
        assert_eq!(t.name(m), "{str: int}");
        let o = t.optional_of(TyId::STR);
        assert_eq!(t.name(o), "Option<str>");
        let tup = t.tuple_of(vec![TyId::INT, TyId::BOOL]);
        assert_eq!(t.name(tup), "(int, bool)");
        let f = t.fn_of(vec![TyId::INT], TyId::STR);
        assert_eq!(t.name(f), "fn(int) -> str");
        let g = t.fn_of(vec![], TyId::UNIT);
        assert_eq!(t.name(g), "fn()");
    }

    #[test]
    fn articles_match_the_type_name() {
        let t = Types::new();
        assert_eq!(t.with_article(TyId::INT), "an `int`");
        assert_eq!(t.with_article(TyId::BOOL), "a `bool`");
        assert_eq!(t.with_article(TyId::FLOAT), "a `float`");
    }

    // ---- Share ------------------------------------------------------------

    fn struct_with(t: &mut Types, name: &str, fields: Vec<(&str, TyId, bool)>) -> TyId {
        let id = t.declare_struct(name, true, span());
        let defs = fields
            .into_iter()
            .map(|(n, ty, mutable)| FieldDef {
                name: n.to_string(),
                ty,
                mutable,
                is_pub: true,
                span: span(),
            })
            .collect();
        t.set_struct_fields(id, defs);
        t.struct_ty(id)
    }

    /// Because fields are immutable by default, ordinary types are `Share`
    /// without the author doing anything.
    #[test]
    fn an_immutable_struct_is_share() {
        let mut t = Types::new();
        let ty = struct_with(&mut t, "Order", vec![("id", TyId::INT), ("name", TyId::STR)]
            .into_iter()
            .map(|(n, ty)| (n, ty, false))
            .collect());
        assert!(t.is_share(ty));
    }

    #[test]
    fn a_var_field_disqualifies_a_type_from_share() {
        let mut t = Types::new();
        let ty = struct_with(&mut t, "Counter", vec![("count", TyId::INT, true)]);
        assert!(!t.is_share(ty), "a mutable field is a data race waiting to happen");
    }

    /// The disqualification is transitive: holding a mutable type is enough.
    #[test]
    fn share_is_transitive_through_fields() {
        let mut t = Types::new();
        let counter = struct_with(&mut t, "Counter", vec![("count", TyId::INT, true)]);
        let holder = struct_with(&mut t, "Holder", vec![("c", counter, false)]);
        assert!(!t.is_share(holder));
    }

    #[test]
    fn collections_inherit_share_from_their_elements() {
        let mut t = Types::new();
        let counter = struct_with(&mut t, "Counter", vec![("count", TyId::INT, true)]);
        let bad = t.slice_of(counter);
        let good = t.slice_of(TyId::INT);
        assert!(!t.is_share(bad));
        assert!(t.is_share(good));
    }

    /// A recursive type must terminate the walk rather than hang.
    #[test]
    fn a_recursive_type_is_share_when_nothing_else_disqualifies_it() {
        let mut t = Types::new();
        let id = t.declare_struct("Node", true, span());
        let node_ty = t.struct_ty(id);
        let children = t.slice_of(node_ty);
        t.set_struct_fields(
            id,
            vec![
                FieldDef { name: "value".into(), ty: TyId::INT, mutable: false, is_pub: true, span: span() },
                FieldDef { name: "children".into(), ty: children, mutable: false, is_pub: true, span: span() },
            ],
        );
        assert!(t.is_share(node_ty));
    }

    #[test]
    fn closures_and_trait_objects_are_not_share() {
        let mut t = Types::new();
        let f = t.fn_of(vec![], TyId::UNIT);
        assert!(!t.is_share(f), "a closure may capture a mutable binding");
        let tr = t.declare_trait("Shape", true, span());
        let d = t.dyn_ty(tr);
        assert!(!t.is_share(d));
    }

    // ---- structural equality ----------------------------------------------

    #[test]
    fn structural_equality_follows_field_types() {
        let mut t = Types::new();
        let ok = struct_with(&mut t, "Point", vec![("x", TyId::FLOAT, false)]);
        assert!(t.is_equatable(ok));

        let f = t.fn_of(vec![], TyId::UNIT);
        let with_fn = struct_with(&mut t, "Handler", vec![("f", f, false)]);
        assert!(!t.is_equatable(with_fn), "functions have no equality");
    }

    /// Structs alias on assignment; slices do not, because they are
    /// copy-on-write values.
    #[test]
    fn structs_are_references_but_slices_are_values() {
        let mut t = Types::new();
        let s = struct_with(&mut t, "P", vec![("x", TyId::INT, false)]);
        assert!(t.is_reference(s));
        assert!(!t.is_reference(TyId::INT));
        let sl = t.slice_of(TyId::INT);
        assert!(!t.is_reference(sl), "a slice has value semantics");
    }
}
