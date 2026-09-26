//! Exhaustiveness and reachability.
//!
//! A `match` must cover every possible value. Exhaustiveness is what makes
//! adding an enum variant safe: the compiler shows you every place that must
//! change.
//!
//! This is Maranget's usefulness algorithm ("Warnings for pattern matching",
//! 2007), over the pattern forms Kite has. A pattern vector is *useful* with
//! respect to a matrix of earlier rows when some value matches it and none of
//! them. Both questions a `match` asks are that one:
//!
//! - it is **exhaustive** when a row of wildcards is not useful after every
//!   arm — and when it is, the values it found are the missing patterns,
//!   reconstructed by name, so the message says `Rect(_, _)` and `Point`
//!   rather than telling the reader to go and work it out;
//! - an arm is **unreachable** when it is not useful after the arms above it.
//!
//! Every column is split by its type's constructors: an enum's variants,
//! `true` and `false`, `nil` and a present value, and the single constructor
//! of a tuple or a struct, whose fields are columns of their own — which is
//! what lets nesting work, `On(true) | On(false) | Off` and `(true, _) |
//! (false, _)` included. An optional matched by a pattern for its payload is
//! the payload's pattern under an implicit "present".
//!
//! Numbers and strings have no finite set of constructors. Their literals are
//! compared exactly, and a column of them is never complete, so it needs a
//! catch-all — conservative in the one direction that matters: a range
//! overlapping a literal is not used to call either unreachable, and nothing
//! is ever called exhaustive that is not.

use kite_hir::{Pattern, TyId, TyKind, Types};

/// How many missing cases one diagnostic names. Past a handful a list stops
/// being read; the first few say what kind of case was forgotten.
const MAX_WITNESSES: usize = 16;

/// A missing case, rendered as source-like text.
pub struct Missing(pub String);

/// A constructor: what the first step of matching a value decides.
#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    Variant(u32),
    Bool(bool),
    Nil,
    /// A present optional, or a present `error`.
    Present,
    /// The one constructor of a tuple or a struct.
    Single,
    /// A literal, or a range, of a type with no finite set of values.
    Lit(Lit),
}

#[derive(Clone, Debug, PartialEq)]
enum Lit {
    Int(i64),
    Float(u64),
    Str(String),
    Range(i64, i64, bool),
}

/// A pattern reduced to what matching decides.
#[derive(Clone, Debug)]
enum Pat {
    Any,
    Ctor(Ctor, Vec<Pat>),
    Or(Vec<Pat>),
}

/// A value no row matches, built back up as the algorithm returns.
#[derive(Clone, Debug)]
enum Wit {
    Any,
    Ctor(Ctor, Vec<Wit>),
}

/// The values of a type, as far as matching them goes.
enum Signature {
    /// Every constructor, each with the types of its fields.
    Finite(Vec<(Ctor, Vec<TyId>)>),
    /// Too many to list: only a catch-all covers them.
    Infinite,
}

fn signature(ty: TyId, types: &Types) -> Signature {
    match types.kind(ty) {
        TyKind::Enum(e) => Signature::Finite(
            types
                .enum_def(*e)
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| (Ctor::Variant(i as u32), v.fields.iter().map(|f| f.ty).collect()))
                .collect(),
        ),
        TyKind::Bool => Signature::Finite(vec![(Ctor::Bool(false), Vec::new()), (Ctor::Bool(true), Vec::new())]),
        TyKind::Optional(inner) => {
            Signature::Finite(vec![(Ctor::Nil, Vec::new()), (Ctor::Present, vec![*inner])])
        }
        // `error` is nil-able, and nothing but a catch-all takes a present
        // one apart.
        TyKind::Err => Signature::Finite(vec![(Ctor::Nil, Vec::new()), (Ctor::Present, Vec::new())]),
        TyKind::Tuple(elems) => Signature::Finite(vec![(Ctor::Single, elems.clone())]),
        TyKind::Struct(s) => Signature::Finite(vec![(
            Ctor::Single,
            types.struct_def(*s).fields.iter().map(|f| f.ty).collect(),
        )]),
        _ => Signature::Infinite,
    }
}

/// The types of a constructor's fields, for a column of type `ty`.
fn fields_of(ctor: &Ctor, ty: TyId, types: &Types) -> Vec<TyId> {
    match signature(ty, types) {
        Signature::Finite(all) => all
            .into_iter()
            .find(|(c, _)| c == ctor)
            .map(|(_, fs)| fs)
            .unwrap_or_default(),
        Signature::Infinite => Vec::new(),
    }
}

/// A checked pattern, against a value of type `ty`.
fn lower(p: &Pattern, ty: TyId, types: &Types) -> Pat {
    match p {
        Pattern::Wildcard | Pattern::Binding { .. } => Pat::Any,
        Pattern::Or(alts) => Pat::Or(alts.iter().map(|a| lower(a, ty, types)).collect()),
        Pattern::Nil => Pat::Ctor(Ctor::Nil, Vec::new()),
        // Anything else written against an optional is a pattern for the
        // value inside it: `nil | A | B` over an `Option<E>`.
        _ if matches!(types.kind(ty), TyKind::Optional(_)) => {
            let TyKind::Optional(inner) = *types.kind(ty) else { unreachable!() };
            Pat::Ctor(Ctor::Present, vec![lower(p, inner, types)])
        }
        Pattern::Bool(b) => Pat::Ctor(Ctor::Bool(*b), Vec::new()),
        Pattern::Variant { variant, fields, .. } => {
            let tys = fields_of(&Ctor::Variant(*variant), ty, types);
            let subs = fields
                .iter()
                .enumerate()
                .map(|(i, f)| lower(f, tys.get(i).copied().unwrap_or(TyId::ERROR), types))
                .collect();
            Pat::Ctor(Ctor::Variant(*variant), subs)
        }
        Pattern::Struct { fields, .. } => {
            let tys = fields_of(&Ctor::Single, ty, types);
            let mut subs = vec![Pat::Any; tys.len()];
            for (index, sub) in fields {
                if let Some(slot) = subs.get_mut(*index as usize) {
                    *slot = lower(sub, tys[*index as usize], types);
                }
            }
            Pat::Ctor(Ctor::Single, subs)
        }
        Pattern::Tuple { elems, .. } => {
            let tys = fields_of(&Ctor::Single, ty, types);
            let subs = elems
                .iter()
                .enumerate()
                .map(|(i, e)| lower(e, tys.get(i).copied().unwrap_or(TyId::ERROR), types))
                .collect();
            Pat::Ctor(Ctor::Single, subs)
        }
        Pattern::Int(v) => Pat::Ctor(Ctor::Lit(Lit::Int(*v)), Vec::new()),
        Pattern::Float(f) => Pat::Ctor(Ctor::Lit(Lit::Float(f.to_bits())), Vec::new()),
        Pattern::Str(s) => Pat::Ctor(Ctor::Lit(Lit::Str(s.clone())), Vec::new()),
        Pattern::IntRange { start, end, inclusive } => {
            Pat::Ctor(Ctor::Lit(Lit::Range(*start, *end, *inclusive)), Vec::new())
        }
    }
}

/// One row of the matrix: a pattern per column still to be decided.
type Row = Vec<Pat>;

/// Rows with an or-pattern first, split into a row per alternative.
fn expand(rows: &[Row]) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        push_expanded(r.clone(), &mut out);
    }
    out
}

fn push_expanded(row: Row, out: &mut Vec<Row>) {
    match row.first() {
        Some(Pat::Or(alts)) => {
            for a in alts {
                let mut next = vec![a.clone()];
                next.extend(row[1..].iter().cloned());
                push_expanded(next, out);
            }
        }
        _ => out.push(row),
    }
}

/// The rows a value built by `ctor` can reach, with the constructor's fields
/// in place of the first column. A catch-all reaches them all.
fn specialise(rows: &[Row], ctor: &Ctor, arity: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for r in expand(rows) {
        let fields: Vec<Pat> = match &r[0] {
            Pat::Any => vec![Pat::Any; arity],
            Pat::Ctor(c, subs) if c == ctor => {
                let mut subs = subs.clone();
                subs.resize(arity, Pat::Any);
                subs
            }
            _ => continue,
        };
        let mut next = fields;
        next.extend(r[1..].iter().cloned());
        out.push(next);
    }
    out
}

/// The rows whose first column matches anything, without that column: what
/// is left for a constructor no row names.
fn default(rows: &[Row]) -> Vec<Row> {
    expand(rows)
        .into_iter()
        .filter(|r| matches!(r[0], Pat::Any))
        .map(|r| r[1..].to_vec())
        .collect()
}

/// The constructors named in the first column.
fn heads(rows: &[Row]) -> Vec<Ctor> {
    let mut out: Vec<Ctor> = Vec::new();
    for r in expand(rows) {
        if let Pat::Ctor(c, _) = &r[0] {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
    }
    out
}

/// How many steps the algorithm may take on one `match`. Far beyond what a
/// written one needs; it stops a generated or adversarial one from making the
/// compiler the slow part, and when it runs out the answer given is the
/// cautious one.
const WORK: usize = 200_000;

/// Whether some value matches `q` and no row of `rows`.
fn useful(rows: &[Row], q: &[Pat], tys: &[TyId], types: &Types, work: &mut usize) -> bool {
    if *work == 0 {
        // Out of budget: say "reachable", which is never a false warning.
        return true;
    }
    *work -= 1;
    let Some(first) = q.first() else {
        return rows.is_empty();
    };
    let ty = tys[0];
    match first {
        Pat::Or(alts) => alts.iter().any(|a| {
            let mut next = vec![a.clone()];
            next.extend(q[1..].iter().cloned());
            useful(rows, &next, tys, types, work)
        }),
        Pat::Ctor(c, subs) => {
            let ftys = fields_of(c, ty, types);
            let mut next = subs.clone();
            next.resize(ftys.len(), Pat::Any);
            next.extend(q[1..].iter().cloned());
            let mut next_tys = ftys.clone();
            next_tys.extend_from_slice(&tys[1..]);
            useful(&specialise(rows, c, ftys.len()), &next, &next_tys, types, work)
        }
        Pat::Any => match signature(ty, types) {
            // Every constructor is named somewhere, so a value is uncovered
            // only if it is uncovered under one of them.
            Signature::Finite(all) if all.iter().all(|(c, _)| heads(rows).contains(c)) => {
                all.iter().any(|(c, ftys)| {
                    let mut next = vec![Pat::Any; ftys.len()];
                    next.extend(q[1..].iter().cloned());
                    let mut next_tys = ftys.clone();
                    next_tys.extend_from_slice(&tys[1..]);
                    useful(&specialise(rows, c, ftys.len()), &next, &next_tys, types, work)
                })
            }
            // Some constructor no row names: any value built by it gets past
            // every row that is not a catch-all.
            _ => useful(&default(rows), &q[1..], &tys[1..], types, work),
        },
    }
}

/// Every value, up to [`MAX_WITNESSES`], that no row matches — one witness
/// pattern per column.
///
/// `top` is set for the scrutinee's own column. There, a type none of whose
/// constructors is named is spelled out constructor by constructor — every
/// variant a `match` forgot. Deeper down, `_` says the same thing shorter.
fn missing(rows: &[Row], tys: &[TyId], types: &Types, top: bool, work: &mut usize) -> Vec<Vec<Wit>> {
    if *work == 0 {
        // Out of budget: say "something is missing", which is never a false
        // claim that a match is exhaustive.
        return vec![vec![Wit::Any; tys.len()]];
    }
    *work -= 1;
    let Some(&ty) = tys.first() else {
        return if rows.is_empty() { vec![Vec::new()] } else { Vec::new() };
    };
    let rows = expand(rows);
    let mut out: Vec<Vec<Wit>> = Vec::new();
    match signature(ty, types) {
        Signature::Finite(all) if top || !heads(&rows).is_empty() => {
            let present = heads(&rows);
            // Every constructor no row names is missing alike — under each,
            // whatever the catch-all rows leave uncovered in the rest.
            let absent_rest = if all.iter().any(|(c, _)| !present.contains(c)) {
                missing(&default(&rows), &tys[1..], types, false, work)
            } else {
                Vec::new()
            };
            for (c, ftys) in &all {
                let found: Vec<Vec<Wit>> = if present.contains(c) {
                    let mut next_tys = ftys.clone();
                    next_tys.extend_from_slice(&tys[1..]);
                    missing(&specialise(&rows, c, ftys.len()), &next_tys, types, false, work)
                        .into_iter()
                        .map(|w| {
                            let (fields, rest) = w.split_at(ftys.len());
                            let mut v = vec![Wit::Ctor(c.clone(), fields.to_vec())];
                            v.extend(rest.iter().cloned());
                            v
                        })
                        .collect()
                } else {
                    absent_rest
                        .iter()
                        .map(|w| {
                            let mut v = vec![Wit::Ctor(c.clone(), vec![Wit::Any; ftys.len()])];
                            v.extend(w.iter().cloned());
                            v
                        })
                        .collect()
                };
                out.extend(found);
                if out.len() >= MAX_WITNESSES {
                    break;
                }
            }
        }
        // Only a catch-all covers a number or a string, so whatever the
        // catch-all rows leave uncovered is missing under `_` — as it is for
        // a column no row takes apart at all.
        _ => {
            for w in missing(&default(&rows), &tys[1..], types, false, work) {
                let mut v = vec![Wit::Any];
                v.extend(w);
                out.push(v);
            }
        }
    }
    out.truncate(MAX_WITNESSES);
    out
}

/// A witness as source-like text, against a value of type `ty`.
fn render(w: &Wit, ty: TyId, types: &Types) -> String {
    let Wit::Ctor(c, args) = w else { return "_".to_string() };
    match (c, types.kind(ty)) {
        (Ctor::Variant(v), TyKind::Enum(e)) => {
            let variant = &types.enum_def(*e).variants[*v as usize];
            if args.is_empty() {
                return variant.name.clone();
            }
            let inner: Vec<String> = args
                .iter()
                .zip(variant.fields.iter())
                .map(|(a, f)| render(a, f.ty, types))
                .collect();
            format!("{}({})", variant.name, inner.join(", "))
        }
        (Ctor::Bool(b), _) => b.to_string(),
        (Ctor::Nil, _) => "nil".to_string(),
        (Ctor::Present, TyKind::Optional(inner)) => match args.first() {
            None | Some(Wit::Any) => "a present value".to_string(),
            Some(a) => render(a, *inner, types),
        },
        (Ctor::Present, _) => "a present value".to_string(),
        (Ctor::Single, TyKind::Tuple(elems)) => {
            let inner: Vec<String> =
                args.iter().zip(elems.iter()).map(|(a, t)| render(a, *t, types)).collect();
            format!("({})", inner.join(", "))
        }
        (Ctor::Single, TyKind::Struct(s)) => {
            let def = types.struct_def(*s);
            let named: Vec<String> = args
                .iter()
                .zip(def.fields.iter())
                .filter(|(a, _)| !matches!(a, Wit::Any))
                .map(|(a, f)| format!("{}: {}", f.name, render(a, f.ty, types)))
                .collect();
            if named.is_empty() {
                format!("{}{{ .. }}", def.name)
            } else if named.len() == def.fields.len() {
                format!("{}{{ {} }}", def.name, named.join(", "))
            } else {
                format!("{}{{ {}, .. }}", def.name, named.join(", "))
            }
        }
        _ => "_".to_string(),
    }
}

/// Patterns not covered by `patterns`, or an empty vector when the match is
/// exhaustive.
///
/// Guards are deliberately ignored when deciding coverage: a guarded arm may
/// fail at run time, so it cannot make a match exhaustive. Callers pass only
/// unguarded patterns.
pub fn missing_patterns(scrutinee: TyId, patterns: &[&Pattern], types: &Types) -> Vec<Missing> {
    let rows: Vec<Row> = patterns.iter().map(|p| vec![lower(p, scrutinee, types)]).collect();
    let mut work = WORK;
    let mut out: Vec<Missing> = Vec::new();
    for w in missing(&rows, &[scrutinee], types, true, &mut work) {
        let text = render(&w[0], scrutinee, types);
        if !out.iter().any(|m| m.0 == text) {
            out.push(Missing(text));
        }
    }
    out
}

/// The arms no value can reach: each is matched only by values an unguarded
/// arm above it already took. Indices into `arms`, whose flag says whether an
/// arm has a guard — a guarded arm may be unreachable itself, but it shadows
/// nothing, because its guard may fail.
pub fn unreachable_arms(scrutinee: TyId, arms: &[(&Pattern, bool)], types: &Types) -> Vec<usize> {
    let mut rows: Vec<Row> = Vec::new();
    let mut out = Vec::new();
    let mut work = WORK;
    for (i, (p, guarded)) in arms.iter().enumerate() {
        let row = vec![lower(p, scrutinee, types)];
        if !useful(&rows, &row, &[scrutinee], types, &mut work) {
            out.push(i);
        }
        if !guarded {
            rows.push(row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kite_hir::{EnumId, FieldDef, LocalId};
    use kite_span::{FileId, Span};

    fn span() -> Span {
        Span::new(FileId(0), 0, 0)
    }

    /// An enum shaped like the specification's `Shape`.
    fn shape(types: &mut Types) -> (EnumId, TyId) {
        let eid = types.declare_enum("Shape", true, span());
        types.set_enum_variants(
            eid,
            vec![
                kite_hir::VariantDef {
                    name: "Circle".into(),
                    fields: vec![FieldDef {
                        name: "radius".into(),
                        ty: TyId::FLOAT,
                        mutable: false,
                        is_pub: true,
                        span: span(),
                    }],
                    named: true,
                    span: span(),
                },
                kite_hir::VariantDef {
                    name: "Rect".into(),
                    fields: vec![
                        FieldDef {
                            name: "w".into(),
                            ty: TyId::FLOAT,
                            mutable: false,
                            is_pub: true,
                            span: span(),
                        },
                        FieldDef {
                            name: "h".into(),
                            ty: TyId::FLOAT,
                            mutable: false,
                            is_pub: true,
                            span: span(),
                        },
                    ],
                    named: true,
                    span: span(),
                },
                kite_hir::VariantDef {
                    name: "Point".into(),
                    fields: Vec::new(),
                    named: false,
                    span: span(),
                },
            ],
        );
        let ty = types.enum_ty(eid);
        (eid, ty)
    }

    fn variant(eid: EnumId, v: u32, arity: usize) -> Pattern {
        Pattern::Variant {
            enum_id: eid,
            variant: v,
            fields: (0..arity).map(|_| Pattern::Wildcard).collect(),
        }
    }

    fn names(m: &[Missing]) -> Vec<&str> {
        m.iter().map(|x| x.0.as_str()).collect()
    }

    #[test]
    fn covering_every_variant_is_exhaustive() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let ps = [variant(eid, 0, 1), variant(eid, 1, 2), variant(eid, 2, 0)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());
    }

    /// The property that matters: the missing variants are named, not merely
    /// counted.
    #[test]
    fn missing_variants_are_named_with_their_arity() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let ps = [variant(eid, 0, 1)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        let m = missing_patterns(ty, &refs, &types);
        assert_eq!(names(&m), vec!["Rect(_, _)", "Point"]);
    }

    #[test]
    fn a_wildcard_covers_everything() {
        let mut types = Types::new();
        let (_, ty) = shape(&mut types);
        let ps = [Pattern::Wildcard];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());
    }

    #[test]
    fn a_binding_covers_everything() {
        let mut types = Types::new();
        let (_, ty) = shape(&mut types);
        let ps = [Pattern::Binding { local: LocalId(0), unwrap: false }];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());
    }

    #[test]
    fn an_or_pattern_counts_each_alternative() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let ps = [
            Pattern::Or(vec![variant(eid, 0, 1), variant(eid, 1, 2)]),
            variant(eid, 2, 0),
        ];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());
    }

    #[test]
    fn bool_needs_both_values() {
        let types = Types::new();
        let ps = [Pattern::Bool(true)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(TyId::BOOL, &refs, &types)), vec!["false"]);

        let ps = [Pattern::Bool(true), Pattern::Bool(false)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(TyId::BOOL, &refs, &types).is_empty());
    }

    /// No finite set of literals covers an integer, so a catch-all is required.
    /// An optional has exactly two cases, so both must be named.
    #[test]
    fn optionals_need_nil_and_a_present_value() {
        let mut types = Types::new();
        let opt = types.optional_of(TyId::INT);

        let ps = [Pattern::Nil];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(
            names(&missing_patterns(opt, &refs, &types)),
            vec!["a present value"]
        );

        let ps = [Pattern::Binding { local: LocalId(0), unwrap: false }];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(opt, &refs, &types)), Vec::<&str>::new());

        let ps = [Pattern::Nil, Pattern::Binding { local: LocalId(0), unwrap: false }];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(opt, &refs, &types).is_empty());
    }

    #[test]
    fn integers_always_need_a_catch_all() {
        let types = Types::new();
        let ps = [Pattern::Int(0), Pattern::Int(1)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(TyId::INT, &refs, &types)), vec!["_"]);
    }

    /// A variant named only with a refutable sub-pattern is not covered: some
    /// other payload value could still arrive.
    #[test]
    fn a_refutable_subpattern_does_not_cover_its_variant() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let ps = [
            Pattern::Variant {
                enum_id: eid,
                variant: 0,
                fields: vec![Pattern::Float(1.0)],
            },
            variant(eid, 1, 2),
            variant(eid, 2, 0),
        ];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(ty, &refs, &types)), vec!["Circle(_)"]);
    }

    fn field(name: &str, ty: TyId) -> FieldDef {
        FieldDef { name: name.into(), ty, mutable: false, is_pub: true, span: span() }
    }

    /// `enum Light { On(bool) Off }`.
    fn light(types: &mut Types) -> (EnumId, TyId) {
        let eid = types.declare_enum("Light", true, span());
        types.set_enum_variants(
            eid,
            vec![
                kite_hir::VariantDef {
                    name: "On".into(),
                    fields: vec![field("0", TyId::BOOL)],
                    named: false,
                    span: span(),
                },
                kite_hir::VariantDef {
                    name: "Off".into(),
                    fields: Vec::new(),
                    named: false,
                    span: span(),
                },
            ],
        );
        let ty = types.enum_ty(eid);
        (eid, ty)
    }

    fn on(eid: EnumId, b: bool) -> Pattern {
        Pattern::Variant { enum_id: eid, variant: 0, fields: vec![Pattern::Bool(b)] }
    }

    fn tuple(ty: TyId, elems: Vec<Pattern>) -> Pattern {
        Pattern::Tuple { ty, elems }
    }

    fn any() -> Pattern {
        Pattern::Wildcard
    }

    fn unreachable(ty: TyId, arms: &[Pattern], types: &Types) -> Vec<usize> {
        let arms: Vec<(&Pattern, bool)> = arms.iter().map(|p| (p, false)).collect();
        unreachable_arms(ty, &arms, types)
    }

    /// The nesting a single-level check could not see: each `bool` inside
    /// `On` is named, so `On` is covered.
    #[test]
    fn a_variant_is_covered_by_its_payloads_together() {
        let mut types = Types::new();
        let (eid, ty) = light(&mut types);
        let ps = [on(eid, true), on(eid, false), variant(eid, 1, 0)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());

        let ps = [on(eid, true), variant(eid, 1, 0)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(ty, &refs, &types)), vec!["On(false)"]);
    }

    #[test]
    fn a_tuple_is_covered_column_by_column() {
        let mut types = Types::new();
        let ty = types.tuple_of(vec![TyId::BOOL, TyId::INT]);
        let ps = [
            tuple(ty, vec![Pattern::Bool(true), any()]),
            tuple(ty, vec![Pattern::Bool(false), any()]),
        ];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());

        let ps = [tuple(ty, vec![Pattern::Bool(true), Pattern::Int(0)])];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(
            names(&missing_patterns(ty, &refs, &types)),
            vec!["(false, _)", "(true, _)"]
        );
    }

    /// Over an `Option<E>`, a variant stands for "present, and this one".
    #[test]
    fn an_optional_enum_is_covered_by_nil_and_every_variant() {
        let mut types = Types::new();
        let (eid, inner) = light(&mut types);
        let ty = types.optional_of(inner);
        let ps = [Pattern::Nil, Pattern::Or(vec![on(eid, true), on(eid, false)]), variant(eid, 1, 0)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());

        let ps = [Pattern::Nil, variant(eid, 1, 0)];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(names(&missing_patterns(ty, &refs, &types)), vec!["On(_)"]);
    }

    #[test]
    fn a_struct_is_covered_field_by_field() {
        let mut types = Types::new();
        let sid = types.declare_struct("S", true, span());
        types.set_struct_fields(sid, vec![field("a", TyId::BOOL), field("b", TyId::BOOL)]);
        let ty = types.struct_ty(sid);
        let s = |fields: Vec<(u32, Pattern)>| Pattern::Struct { struct_id: sid, fields };
        let ps = [
            s(vec![(0, Pattern::Bool(true)), (1, Pattern::Bool(true))]),
            s(vec![(0, Pattern::Bool(false))]),
            s(vec![(0, Pattern::Bool(true)), (1, Pattern::Bool(false))]),
        ];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert!(missing_patterns(ty, &refs, &types).is_empty());

        let refs: Vec<&Pattern> = ps[..2].iter().collect();
        assert_eq!(
            names(&missing_patterns(ty, &refs, &types)),
            vec!["S{ a: true, b: false }"]
        );
    }

    /// A pattern nested inside a recursive variant names what it left out,
    /// and no more than it has to.
    #[test]
    fn a_nested_miss_is_named_where_it_is() {
        let mut types = Types::new();
        let eid = types.declare_enum("E", true, span());
        let ety = types.enum_ty(eid);
        types.set_enum_variants(
            eid,
            vec![
                kite_hir::VariantDef {
                    name: "Num".into(),
                    fields: vec![field("v", TyId::INT)],
                    named: true,
                    span: span(),
                },
                kite_hir::VariantDef {
                    name: "Add".into(),
                    fields: vec![field("l", ety), field("r", ety)],
                    named: true,
                    span: span(),
                },
            ],
        );
        let ps = [
            variant(eid, 0, 1),
            Pattern::Variant {
                enum_id: eid,
                variant: 1,
                fields: vec![
                    Pattern::Variant { enum_id: eid, variant: 0, fields: vec![Pattern::Int(0)] },
                    any(),
                ],
            },
        ];
        let refs: Vec<&Pattern> = ps.iter().collect();
        assert_eq!(
            names(&missing_patterns(ety, &refs, &types)),
            vec!["Add(Num(_), _)", "Add(Add(_, _), _)"]
        );
    }

    /// An arm after a catch-all, or after arms that already cover it, can
    /// never run.
    #[test]
    fn an_arm_covered_by_those_above_is_unreachable() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let ps = [variant(eid, 2, 0), variant(eid, 0, 1), variant(eid, 2, 0), any()];
        assert_eq!(unreachable(ty, &ps, &types), vec![2]);

        let ps = [
            Pattern::Binding { local: LocalId(0), unwrap: false },
            variant(eid, 0, 1),
            variant(eid, 1, 2),
        ];
        assert_eq!(unreachable(ty, &ps, &types), vec![1, 2]);

        let (lid, lty) = light(&mut types);
        let ps = [on(lid, true), on(lid, false), variant(lid, 1, 0), any()];
        assert_eq!(unreachable(lty, &ps, &types), vec![3]);
    }

    /// A guarded arm may fail, so it shadows nothing below it.
    #[test]
    fn a_guarded_arm_shadows_nothing() {
        let mut types = Types::new();
        let (eid, ty) = shape(&mut types);
        let point = variant(eid, 2, 0);
        let arms = [(&point, true), (&point, false)];
        assert!(unreachable_arms(ty, &arms, &types).is_empty());
    }

    /// Literals compare exactly, and nothing about a range overlapping one is
    /// used to call either unreachable.
    #[test]
    fn literals_are_unreachable_only_when_repeated() {
        let types = Types::new();
        let ps = [Pattern::Int(1), Pattern::Int(2), Pattern::Int(1), any()];
        assert_eq!(unreachable(TyId::INT, &ps, &types), vec![2]);

        let ps = [
            Pattern::IntRange { start: 0, end: 9, inclusive: true },
            Pattern::Int(5),
            any(),
        ];
        assert!(unreachable(TyId::INT, &ps, &types).is_empty());
    }
}
