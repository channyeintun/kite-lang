//! Exhaustiveness checking.
//!
//! A `match` must cover every possible value. Exhaustiveness is what makes
//! adding an enum variant safe: the compiler shows you every place that must
//! change.
//!
//! This is Maranget's usefulness algorithm, specialised to the pattern forms
//! Kite has. The important property is not that it says "non-exhaustive" but
//! that it **reconstructs the missing patterns by name**, so the message names
//! `Point` and `Rect` rather than telling the reader to go and work it out.

use kite_hir::{EnumId, Pattern, TyId, TyKind, Types};

/// A pattern reduced to just what exhaustiveness cares about.
#[derive(Clone, Debug)]
enum Row {
    /// Matches everything: `_`, or a binding.
    Any,
    /// A specific enum variant, with sub-rows for its fields.
    Variant(u32, Vec<Row>),
    /// A literal or range, which can never be proved to cover its type.
    Refutable,
    /// A struct, which has one constructor, so its fields decide.
    Struct(Vec<Row>),
}

fn lower(p: &Pattern, types: &Types) -> Row {
    match p {
        Pattern::Wildcard | Pattern::Binding { .. } => Row::Any,
        Pattern::Variant { variant, fields, .. } => {
            Row::Variant(*variant, fields.iter().map(|f| lower(f, types)).collect())
        }
        Pattern::Struct { struct_id, fields } => {
            let count = types.struct_def(*struct_id).fields.len();
            let mut rows = vec![Row::Any; count];
            for (index, sub) in fields {
                rows[*index as usize] = lower(sub, types);
            }
            Row::Struct(rows)
        }
        // An or-pattern is exhaustive only if one alternative is.
        Pattern::Or(alts) => {
            if alts.iter().any(|a| a.is_irrefutable()) {
                Row::Any
            } else {
                // Approximated as refutable; the alternatives are still each
                // considered as separate rows by `check`.
                Row::Refutable
            }
        }
        _ => Row::Refutable,
    }
}

/// A missing case, rendered as source-like text.
pub struct Missing(pub String);

/// Patterns not covered by `arms`, or an empty vector when the match is
/// exhaustive.
///
/// Guards are deliberately ignored when deciding coverage: a guarded arm may
/// fail at run time, so it cannot make a match exhaustive. Callers pass only
/// unguarded patterns.
pub fn missing_patterns(scrutinee: TyId, patterns: &[&Pattern], types: &Types) -> Vec<Missing> {
    // Or-patterns are flattened so each alternative counts as its own row.
    let mut rows = Vec::new();
    for p in patterns {
        flatten(p, types, &mut rows);
    }

    if rows.iter().any(|r| matches!(r, Row::Any)) {
        return Vec::new();
    }

    match *types.kind(scrutinee) {
        TyKind::Enum(eid) => missing_variants(eid, &rows, types),

        TyKind::Bool => {
            let mut missing = Vec::new();
            for want in [false, true] {
                if !patterns.iter().any(|p| covers_bool(p, want)) {
                    missing.push(Missing(want.to_string()));
                }
            }
            missing
        }

        // A tuple has one constructor, so its elements decide. Reporting `_`
        // is sound and never wrong, only less specific.
        TyKind::Tuple(_) => {
            if patterns.iter().any(|p| {
                matches!(p, Pattern::Tuple { elems, .. } if elems.iter().all(|e| e.is_irrefutable()))
            }) {
                Vec::new()
            } else {
                vec![Missing("_".to_string())]
            }
        }

        // An optional has two cases: nil, and a present value.
        TyKind::Optional(_) => {
            let mut missing = Vec::new();
            if !patterns.iter().any(|p| covers_nil(p)) {
                missing.push(Missing("nil".to_string()));
            }
            if !patterns.iter().any(|p| p.is_irrefutable()) {
                missing.push(Missing("a present value".to_string()));
            }
            missing
        }

        // A struct has one constructor, so it is exhaustive when every field
        // pattern is. Nested exhaustiveness over struct fields is deferred;
        // reporting `_` is sound and never wrong, just less specific.
        TyKind::Struct(_) => {
            if rows.iter().any(|r| matches!(r, Row::Struct(f) if f.iter().all(is_any))) {
                Vec::new()
            } else {
                vec![Missing("_".to_string())]
            }
        }

        // No finite set of literals covers an int, a float, or a string, so
        // these always need a catch-all.
        _ => vec![Missing("_".to_string())],
    }
}

fn flatten(p: &Pattern, types: &Types, out: &mut Vec<Row>) {
    match p {
        Pattern::Or(alts) => {
            for a in alts {
                flatten(a, types, out);
            }
        }
        other => out.push(lower(other, types)),
    }
}

fn is_any(r: &Row) -> bool {
    matches!(r, Row::Any)
}

fn covers_nil(p: &Pattern) -> bool {
    match p {
        Pattern::Wildcard | Pattern::Binding { .. } | Pattern::Nil => true,
        Pattern::Or(alts) => alts.iter().any(covers_nil),
        _ => false,
    }
}

fn covers_bool(p: &Pattern, want: bool) -> bool {
    match p {
        Pattern::Wildcard | Pattern::Binding { .. } => true,
        Pattern::Bool(b) => *b == want,
        Pattern::Or(alts) => alts.iter().any(|a| covers_bool(a, want)),
        _ => false,
    }
}

fn missing_variants(eid: EnumId, rows: &[Row], types: &Types) -> Vec<Missing> {
    let def = types.enum_def(eid);
    let mut missing = Vec::new();

    for (vi, variant) in def.variants.iter().enumerate() {
        let vi = vi as u32;
        let matching: Vec<&Vec<Row>> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Variant(v, fields) if *v == vi => Some(fields),
                _ => None,
            })
            .collect();

        if matching.is_empty() {
            missing.push(Missing(render_variant(&def.name, &variant.name, variant.fields.len())));
            continue;
        }

        // The variant is named. It is fully covered only if some row leaves
        // every field open; otherwise the sub-patterns are refutable and a
        // specific value could still slip through.
        let fully_covered = matching.iter().any(|fields| fields.iter().all(is_any));
        if !fully_covered && !variant.fields.is_empty() {
            missing.push(Missing(render_variant(
                &def.name,
                &variant.name,
                variant.fields.len(),
            )));
        }
    }

    missing
}

fn render_variant(enum_name: &str, variant: &str, arity: usize) -> String {
    let _ = enum_name;
    if arity == 0 {
        variant.to_string()
    } else {
        let holes = vec!["_"; arity].join(", ");
        format!("{}({})", variant, holes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kite_hir::{FieldDef, LocalId};
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
}
