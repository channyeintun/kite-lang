//! Type checking, and lowering to HIR.
//!
//! Checking is bidirectional: inference propagates *down* from annotations and
//! *up* from literals, and never crosses a function boundary — signatures are
//! always fully annotated. That limit is deliberate. It keeps inference local,
//! makes the checker fast, and above all makes errors point at the actual
//! mismatch rather than at a unification failure three files away.
//!
//! Two rules from the specification do most of the work here:
//!
//! * **No implicit numeric conversion.** `int` and `float` never coerce.
//! * **No truthiness.** A condition must be exactly `bool`.
//!
//! Once an error is reported the offending expression becomes [`TyId::ERROR`],
//! which satisfies every expectation. One mistake therefore yields one
//! diagnostic instead of a cascade.

use kite_ast as ast;
use kite_diag::{codes, DiagBag, Diagnostic, Fix};
use kite_hir as hir;
use kite_hir::{Builtin, ExprKind, TyId, TyKind, Types};
use kite_resolve::{BuiltinFn, Res, ResolveMap};
use kite_span::{SourceMap, Span};

mod consts;
mod exclusive;
mod exhaustive;

pub use consts::{ConstTable, ConstValue};

pub fn check(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    sources: &SourceMap,
    diags: &mut DiagBag,
) -> hir::Program {
    check_with(file, resolved, sources, diags, false)
}

/// What the checker solved but the programmer never wrote.
///
/// A `let` with no annotation gets its type recorded here, and a call to a
/// generic function gets the type arguments the call worked out. This is what
/// an editor shows inline: Kite has no turbofish, so a call site is the one
/// place these facts have no written form at all.
///
/// Everything is rendered to text at the moment it is solved, because a
/// [`TyId`] is only meaningful next to the arena that interned it — and the
/// arena leaves with the checker.
#[derive(Default)]
pub struct Solved {
    /// The span of a bare `let`/`var` name, and the type it received.
    pub bindings: Vec<(Span, String)>,
    /// The span of a generic callee as written, and the arguments the call
    /// solved, comma-joined: `int, str`.
    pub calls: Vec<(Span, String)>,
    /// Every *use* of a local, with the name as written and its type.
    ///
    /// The resolver knows which slot a use names and not what is in it; the
    /// type is decided here. Without this an editor has nothing to say about
    /// a variable, which is most of what anybody hovers.
    pub locals: Vec<(Span, String, String)>,
    /// Every method call, with the receiver's type and the method's signature.
    ///
    /// A method is resolved *here* rather than in the resolver, because
    /// finding one needs the receiver's type — which is why methods reach the
    /// index this way and why rename, which reads the resolver's table, still
    /// refuses to touch one.
    pub methods: Vec<(Span, String)>,
}

/// Check, in a named build mode. `release` drops `assert`.
pub fn check_with(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    sources: &SourceMap,
    diags: &mut DiagBag,
    release: bool,
) -> hir::Program {
    // Solved-but-unwritten types are a language-server concern; a build has
    // nowhere to put them, so they are worked out and dropped.
    check_recording(file, resolved, sources, diags, release, &mut Solved::default())
}

/// Check, and keep what inference decided along the way.
///
/// The extra parameter is an out-parameter rather than a return value so that
/// [`check_with`] keeps its shape: everything that only wants a program calls
/// it exactly as before.
pub fn check_recording(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    sources: &SourceMap,
    diags: &mut DiagBag,
    release: bool,
    solved: &mut Solved,
) -> hir::Program {
    let mut types = Types::new();
    let mut fns = Vec::new();

    // Declare every nominal type before filling any of them in, so mutually
    // recursive definitions can refer to each other. Every Kite aggregate is a
    // GC reference, so recursion needs no annotation from the user.
    let mut type_ids: Vec<Option<TypeTarget>> = Vec::new();
    for decl in &resolved.types {
        let target = match decl.kind {
            kite_resolve::TypeKind::Struct => Some(TypeTarget::Struct(
                types.declare_struct(decl.name.clone(), true, decl.span),
            )),
            kite_resolve::TypeKind::Enum => Some(TypeTarget::Enum(
                types.declare_enum(decl.name.clone(), true, decl.span),
            )),
            kite_resolve::TypeKind::Trait => Some(TypeTarget::Trait(
                types.declare_trait(decl.name.clone(), true, decl.span),
            )),
            kite_resolve::TypeKind::Alias => None,
        };
        type_ids.push(target);
    }

    // An alias is interchangeable with the type it names (§3.4), so it is
    // expanded to that type here — before any field, variant or signature is
    // resolved, because all of those may name one. Arity comes first: an alias
    // may name a generic type, and the check for that reads a count which the
    // fill pass below would not have set yet.
    for (i, decl) in resolved.types.iter().enumerate() {
        let count = match &file.items[decl.decl_index] {
            ast::Item::Struct(s) => s.generics.len(),
            ast::Item::Enum(e) => e.generics.len(),
            _ => continue,
        };
        match type_ids[i] {
            Some(TypeTarget::Struct(sid)) => types.set_struct_generics(sid, count),
            Some(TypeTarget::Enum(eid)) => types.set_enum_generics(eid, count),
            _ => {}
        }
    }
    expand_aliases(file, resolved, &mut type_ids, &mut types, diags);

    // Now fill in fields and variants, resolving their types against the
    // arena, which already knows every name.
    let mut type_generics: Vec<Vec<GenericDef>> = vec![Vec::new(); resolved.types.len()];
    let mut trait_method_generics: std::collections::HashMap<(hir::TraitId, usize), Vec<GenericDef>> =
        std::collections::HashMap::new();
    for (i, decl) in resolved.types.iter().enumerate() {
        // Types named inside a module mean that module's, so its own name is
        // tried first everywhere a type is written.
        let module = resolved.module_of_item(decl.decl_index);
        // A declaration's own `<T, U>` is in scope for its fields.
        let own_generics = match &file.items[decl.decl_index] {
            ast::Item::Struct(s) => &s.generics,
            ast::Item::Enum(e) => &e.generics,
            ast::Item::Trait(tr) => &tr.generics,
            _ => &[][..],
        };
        let defs = declare_generics(own_generics, &[], resolved, module, &type_ids, &mut types, diags);
        let generics: &[(String, TyId)] =
            &defs.iter().map(|g| (g.name.clone(), g.ty)).collect::<Vec<_>>();
        // Kept, because a bound on a struct's or an enum's parameter holds at
        // every place one is built — not only where the declaration is read.
        type_generics[i] = defs.clone();
        match type_ids[i] {
            Some(TypeTarget::Struct(sid)) => types.set_struct_generics(sid, defs.len()),
            Some(TypeTarget::Enum(eid)) => types.set_enum_generics(eid, defs.len()),
            _ => {}
        }
        match (type_ids[i], &file.items[decl.decl_index]) {
            (Some(TypeTarget::Enum(eid)), ast::Item::Enum(e)) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| {
                        let (fields, named) = match &v.payload {
                            ast::VariantPayload::Unit => (Vec::new(), false),
                            ast::VariantPayload::Named(fs) => (
                                fs.iter()
                                    .map(|f| kite_hir::FieldDef {
                                        name: f.name.name.clone(),
                                        ty: resolve_named_ty(
                                            &f.ty, resolved, module, &type_ids, generics, &mut types, diags,
                                        ),
                                        mutable: false,
                                        is_pub: true,
                                        span: f.span,
                                    })
                                    .collect(),
                                true,
                            ),
                            ast::VariantPayload::Positional(tys) => (
                                tys.iter()
                                    .enumerate()
                                    .map(|(i, ty)| kite_hir::FieldDef {
                                        name: i.to_string(),
                                        ty: resolve_named_ty(
                                            ty, resolved, module, &type_ids, generics, &mut types, diags,
                                        ),
                                        mutable: false,
                                        is_pub: true,
                                        span: ty.span(),
                                    })
                                    .collect(),
                                false,
                            ),
                        };
                        kite_hir::VariantDef {
                            name: v.name.name.clone(),
                            fields,
                            named,
                            span: v.span,
                        }
                    })
                    .collect();
                types.set_enum_variants(eid, variants);
            }
            (Some(TypeTarget::Trait(tid)), ast::Item::Trait(tr)) => {
                // Inside a trait, `Self` is whichever type implements it, and
                // a method may have type parameters of its own after the
                // trait's.
                let self_ty = types.self_param();
                let mut methods = Vec::with_capacity(tr.methods.len());
                for (mi, m) in tr.methods.iter().enumerate() {
                    let own = declare_generics(
                        &m.generics, &defs, resolved, module, &type_ids, &mut types, diags,
                    );
                    let mut names: Vec<(String, TyId)> = generics.to_vec();
                    names.extend(own.iter().map(|g| (g.name.clone(), g.ty)));
                    names.push(("Self".to_string(), self_ty));
                    let params = m
                        .params
                        .iter()
                        .map(|p| {
                            resolve_named_ty(&p.ty, resolved, module, &type_ids, &names, &mut types, diags)
                        })
                        .collect();
                    let ret = match &m.ret {
                        None => TyId::UNIT,
                        Some(r) => resolve_named_ty(
                            r.value_type(), resolved, module, &type_ids, &names, &mut types, diags,
                        ),
                    };
                    methods.push(kite_hir::TraitMethodDef {
                        name: m.name.name.clone(),
                        params,
                        ret,
                        fallible: m.ret.as_ref().is_some_and(|r| r.is_fallible()),
                        takes_self: m.self_param.is_some(),
                        var_self: m.self_param.as_ref().is_some_and(|s| s.is_var),
                        has_default: m.body.is_some(),
                        generic_count: own.len(),
                        span: m.name.span,
                    });
                    trait_method_generics.insert((tid, mi), own);
                }
                types.set_trait_methods(tid, methods);
            }
            (Some(TypeTarget::Struct(sid)), ast::Item::Struct(s)) => {
                let fields = s
                    .fields
                    .iter()
                    .map(|f| kite_hir::FieldDef {
                        name: f.name.name.clone(),
                        ty: resolve_named_ty(&f.ty, resolved, module, &type_ids, generics, &mut types, diags),
                        mutable: f.is_var,
                        is_pub: f.is_pub,
                        span: f.span,
                    })
                    .collect();
                types.set_struct_fields(sid, fields);
            }
            _ => {}
        }
    }

    // A specialisation may have been asked for while its template was still
    // being filled in, so every one is recomputed now that they all are.
    types.refresh_instances();

    // Signatures next, so calls can be checked in either direction.
    let mut lifted: Vec<hir::Function> = Vec::new();
    let mut sigs = Vec::new();
    let mut externs: Vec<hir::ExternDef> = Vec::new();
    // Which declaration each signature belongs to. Matching them up by name
    // later would be matching a module-qualified name against a host-facing
    // one, which is how a stub ends up calling the wrong import.
    let mut extern_of_sig: Vec<Option<u32>> = Vec::new();
    // An `impl` block's `<T: Bound>` list belongs to every method in it, so
    // it is declared once per block — which is also what keeps a mistake in
    // it to one diagnostic rather than one per method.
    let mut impl_generics: std::collections::HashMap<usize, Vec<GenericDef>> =
        std::collections::HashMap::new();
    for sig in &resolved.fns {
        let module = resolved.module_of_item(sig.decl_index);
        // A function's own type parameters are in scope for its signature. A
        // method has its block's first, then its own: `fn map<U>` inside
        // `impl<T> Box<T>` is generic over `T` and `U`, in that order, which
        // is the order a call supplies them in — the receiver's arguments,
        // then whatever the call solves.
        let generic_defs = match sig.owner {
            None => match &file.items[sig.decl_index] {
                ast::Item::Fn(f) => {
                    declare_generics(&f.generics, &[], resolved, module, &type_ids, &mut types, diags)
                }
                _ => Vec::new(),
            },
            Some(owner) => {
                let mut defs = impl_generics
                    .entry(owner.impl_index)
                    .or_insert_with(|| match &file.items[owner.impl_index] {
                        ast::Item::Impl(i) => declare_generics(
                            &i.generics, &[], resolved, module, &type_ids, &mut types, diags,
                        ),
                        _ => Vec::new(),
                    })
                    .clone();
                let own: &[ast::GenericParam] = match &file.items[owner.impl_index] {
                    ast::Item::Impl(i) => &i.methods[owner.method_index].generics,
                    ast::Item::Trait(tr) => &tr.methods[owner.method_index].generics,
                    _ => &[],
                };
                let more = declare_generics(own, &defs, resolved, module, &type_ids, &mut types, diags);
                defs.extend(more);
                defs
            }
        };
        // `Self` in a method's signature is the type the method is on, at the
        // block's own parameters — `Box<T>` inside `impl<T> Box<T>`.
        let owner_ty = sig.owner.map(|owner| {
            let own: Vec<TyId> = impl_generics
                .get(&owner.impl_index)
                .map(|defs| defs.iter().map(|g| g.ty).collect())
                .unwrap_or_default();
            match type_ids[owner.type_index as usize] {
                Some(TypeTarget::Struct(s)) if !own.is_empty() => {
                    let id = types.instantiate_struct(s, &own);
                    types.struct_ty(id)
                }
                Some(TypeTarget::Enum(e)) if !own.is_empty() => {
                    let id = types.instantiate_enum(e, &own);
                    types.enum_ty(id)
                }
                other => named_ty(other, &mut types),
            }
        });
        let mut named: Vec<(String, TyId)> =
            generic_defs.iter().map(|g| (g.name.clone(), g.ty)).collect();
        if let Some(ty) = owner_ty {
            named.push(("Self".to_string(), ty));
        }
        let generics: &[(String, TyId)] = &named;
        if let ast::Item::Extern(e) = &file.items[sig.decl_index] {
            let params: Vec<TyId> = e
                .params
                .iter()
                .map(|p| resolve_named_ty(&p.ty, resolved, module, &type_ids, generics, &mut types, diags))
                .collect();
            let ret = match &e.ret {
                None => TyId::UNIT,
                Some(r) => resolve_named_ty(
                    r.value_type(), resolved, module, &type_ids, generics, &mut types, diags,
                ),
            };
            // Only numbers, booleans and strings cross the boundary. A struct
            // would need a representation both sides agreed on, and inventing
            // one silently is how an FFI becomes a source of corruption.
            for (p, ty) in e.params.iter().zip(&params) {
                check_host_type(*ty, p.ty.span(), &types, diags);
            }
            if let Some(r) = &e.ret {
                check_host_type(ret, r.span(), &types, diags);
            }
            extern_of_sig.push(Some(externs.len() as u32));
            externs.push(hir::ExternDef {
                host: e.host.clone(),
                // The host-facing name is the one written in the declaration.
                // A module qualifies the Kite name — `http.fetch_start` — but
                // the host knows nothing of Kite's modules, and an import
                // whose field name changed when a library was reorganised
                // would be a boundary that drifts for no reason.
                name: e
                    .name
                    .name
                    .rsplit('.')
                    .next()
                    .unwrap_or(&e.name.name)
                    .to_string(),
                params: params.clone(),
                ret,
                span: e.span,
            });
            sigs.push(Signature {
                params,
                ret,
                is_async: false,
                fallible: e.ret.as_ref().is_some_and(|r| r.is_fallible()),
                name_span: e.name.span,
                self_ty: None,
                generics: Vec::new(),
            });
            continue;
        }

        let (params, ret, fallible, name_span, self_ty) = match sig.owner {
            None => {
                let ast::Item::Fn(f) = &file.items[sig.decl_index] else {
                    unreachable!("a free-function signature points at a function")
                };
                let params = f
                    .params
                    .iter()
                    .map(|p| resolve_named_ty(&p.ty, resolved, module, &type_ids, generics, &mut types, diags))
                    .collect();
                let ret = match &f.ret {
                    None => TyId::UNIT,
                    Some(r) => {
                        resolve_named_ty(r.value_type(), resolved, module, &type_ids, generics, &mut types, diags)
                    }
                };
                let fallible = f.ret.as_ref().is_some_and(|r| r.is_fallible());
                let ret = if fallible { types.fallible_of(ret) } else { ret };
                (params, ret, fallible, f.name.span, None)
            }
            Some(owner) => {
                // A default method's body lives in the trait declaration, not
                // in the `impl` block that inherited it.
                let methods = match &file.items[owner.impl_index] {
                    ast::Item::Impl(imp) => &imp.methods,
                    ast::Item::Trait(tr) => &tr.methods,
                    _ => unreachable!("a method signature points at an impl or a trait"),
                };
                let m = &methods[owner.method_index];
                let params = m
                    .params
                    .iter()
                    .map(|p| resolve_named_ty(&p.ty, resolved, module, &type_ids, generics, &mut types, diags))
                    .collect();
                let ret = match &m.ret {
                    None => TyId::UNIT,
                    Some(r) => {
                        resolve_named_ty(r.value_type(), resolved, module, &type_ids, generics, &mut types, diags)
                    }
                };
                // On a generic type, `self` is the declaration at its own
                // parameters — `Box<T>`, not `Box`. Without that, the copy
                // made for `Box<int>` would still take a `Box` and the call
                // would not type-check where types are checked.
                let self_ty = if owner.takes_self { owner_ty } else { None };
                let fallible = m.ret.as_ref().is_some_and(|r| r.is_fallible());
                let ret = if fallible { types.fallible_of(ret) } else { ret };
                (params, ret, fallible, m.name.span, self_ty)
            }
        };
        let is_async = match sig.owner {
            None => matches!(&file.items[sig.decl_index], ast::Item::Fn(f) if f.is_async),
            Some(owner) => match &file.items[owner.impl_index] {
                ast::Item::Impl(imp) => imp.methods[owner.method_index].is_async,
                ast::Item::Trait(tr) => tr.methods[owner.method_index].is_async,
                _ => false,
            },
        };
        extern_of_sig.push(None);
        sigs.push(Signature {
            params,
            ret,
            is_async,
            fallible,
            name_span,
            self_ty,
            generics: generic_defs,
        });
    }

    // An `impl` whose methods are all inherited defaults declared none of its
    // own, so its `<T: Bound>` list was never reached above — and its bounds
    // are still part of whether its type implements the trait.
    for (i, item) in file.items.iter().enumerate() {
        if let ast::Item::Impl(imp) = item {
            if let std::collections::hash_map::Entry::Vacant(slot) = impl_generics.entry(i) {
                let module = resolved.module_of_item(i);
                slot.insert(declare_generics(
                    &imp.generics, &[], resolved, module, &type_ids, &mut types, diags,
                ));
            }
        }
    }
    let trait_impls = check_impls(file, resolved, &type_ids, &mut types, &sigs, &trait_method_generics, diags);

    // Constants are worked out before any body, because a body naming one gets
    // the value itself rather than a reference to it — by the time a use is
    // checked, there is nothing left to look up.
    let const_table = consts::evaluate(file, resolved, sources, diags);
    check_const_annotations(file, resolved, &const_table, &type_ids, &mut types, diags);

    let mut facts = GenericFacts {
        compared: sigs.iter().map(|s| vec![None; s.generics.len()]).collect(),
        calls: Vec::new(),
    };
    for (i, sig) in resolved.fns.iter().enumerate() {
        let mut checker = Checker {
            facts: &mut facts,
            type_generics: &type_generics,
            impl_generics: &impl_generics,
            trait_method_generics: &trait_method_generics,
            trait_impls: &trait_impls,
            consts: &const_table,
            resolved,
            module: resolved.module_of_item(sig.decl_index).to_string(),
            sigs: &sigs,
            lifted: Vec::new(),
            lifted_base: lifted.len(),
            closure_span: None,
            captures: Vec::new(),
            generic_defs: sigs[i].generics.clone(),
            generics: sigs[i].generics.iter().map(|g| (g.name.clone(), g.ty)).collect(),
            type_ids: &type_ids,
            types: &mut types,
            sources,
            diags,
            fn_index: i,
            locals: Vec::new(),
            init: Vec::new(),
            taint: Vec::new(),
            guards: std::collections::HashMap::new(),
            narrowed: std::collections::HashMap::new(),
            error_nonnil: std::collections::HashSet::new(),
            loops: Vec::new(),
            closure_sig: None,
            closure_ret_unknown: None,
            reported_unchecked: std::collections::HashSet::new(),
            defers: None,
            release,
            solved: &mut *solved,
        };
        if let ast::Item::Extern(e) = &file.items[sig.decl_index] {
            // A host function becomes an ordinary one whose whole body is the
            // call across the boundary. Nothing after this point needs to know
            // `extern` exists — and an unused one is pruned like any other.
            let Some(index) = extern_of_sig.get(i).copied().flatten() else {
                unreachable!("every extern declaration recorded its index")
            };
            let params: Vec<hir::Local> = e
                .params
                .iter()
                .zip(&sigs[i].params)
                .map(|(p, ty)| hir::Local {
                    name: p.name.name.clone(),
                    ty: *ty,
                    mutable: false,
                    span: p.span,
                    synthetic: false,
                })
                .collect();
            let args: Vec<hir::Expr> = params
                .iter()
                .enumerate()
                .map(|(j, p)| hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(j as u32)),
                    ty: p.ty,
                    span: p.span,
                })
                .collect();
            let call = hir::Expr {
                kind: ExprKind::CallExtern { index, args },
                ty: sigs[i].ret,
                span: e.span,
            };
            fns.push(hir::Function {
                name: e.name.name.clone(),
                is_free: true,
                generic_count: 0,
                is_pub: e.is_pub,
                is_async: false,
                param_count: params.len(),
                locals: params,
                ret: sigs[i].ret,
                body: hir::Block {
                    stmts: vec![hir::Stmt::Return { value: Some(call), span: e.span }],
                },
                span: e.span,
            });
            continue;
        }

        let func = match sig.owner {
            None => {
                let ast::Item::Fn(f) = &file.items[sig.decl_index] else {
                    unreachable!()
                };
                checker.check_body(
                    &f.name.name,
                    f.is_pub,
                    f.is_async,
                    &f.params,
                    Some(&f.body),
                    f.body.span,
                    f.span,
                    &sigs[i],
                    false,
                )
            }
            Some(owner) => {
                let methods = match &file.items[owner.impl_index] {
                    ast::Item::Impl(imp) => &imp.methods,
                    ast::Item::Trait(tr) => &tr.methods,
                    _ => unreachable!("a method signature points at an impl or a trait"),
                };
                let m = &methods[owner.method_index];
                let body_span = m.body.as_ref().map(|b| b.span).unwrap_or(m.span);
                checker.check_body(
                    &m.name.name,
                    m.is_pub,
                    m.is_async,
                    &m.params,
                    m.body.as_ref(),
                    body_span,
                    m.span,
                    &sigs[i],
                    owner.takes_self,
                )
            }
        };
        fns.push(func);
        // Functions lifted out of this one's closure literals. Their ids were
        // handed out assuming they land after every declared function, so they
        // are collected here and appended once all declarations are checked.
        lifted.append(&mut checker.lifted);
    }
    fns.append(&mut lifted);
    check_compared_params(&mut facts, resolved, &sigs, &types, diags);

    let vtables = build_vtables(resolved, &type_ids, &types);

    let program = hir::Program {
        types,
        externs,
        fns,
        entry: resolved.fn_by_name("main").map(hir::FnId),
        vtables,
    };

    // Exclusivity runs on the finished HIR rather than alongside inference: it
    // needs each callee's parameters, and a call may precede the declaration it
    // reaches. Skipped once anything else has failed, because a poisoned
    // argument list produces places that were never written.
    if !diags.has_errors() {
        exclusive::check(&program, diags);
    }

    program
}

/// Whether a type may cross the host boundary.
///
/// Numbers, booleans, strings — and a `JsValue`, which is the host's own object
/// going back to the host that made it. An aggregate of Kite's would need a
/// representation both sides agreed on, and inventing one silently is how an
/// FFI becomes a source of corruption: a host that wants structure is handed a
/// `str` of JSON, or a reference it made itself.
///
/// `JsValue` is the second of those, given a type. It was always the intended
/// answer — the specification described it before the compiler had it — and
/// until it existed the only way to spell "a thing the host made" was an `int`
/// indexing a table, which is the shape that leaks, cannot be collected, and
/// answers identity wrongly.
fn check_host_type(ty: TyId, span: Span, types: &Types, diags: &mut DiagBag) {
    let ok = matches!(
        types.kind(ty),
        TyKind::Int
            | TyKind::Float
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Error
            | TyKind::JsValue
    );
    if ok {
        return;
    }
    diags.push(
        Diagnostic::error(
            codes::E0204,
            format!("`{}` cannot cross the host boundary", types.name(ty)),
        )
        .with_primary(span, "not a host type")
        .with_note(
            "a host declaration takes and returns `int`, `float`, `bool`, `str` or \
             `JsValue`; Kite's own structures cross as text, or as a `JsValue` the \
             host made and understands",
        ),
    );
}

/// Collect, for every trait, the concrete types implementing it and the
/// function each supplies for each method. Doing this once here is what keeps
/// the bytecode VM and the Wasm backend dispatching over the same set.
fn build_vtables(
    resolved: &ResolveMap,
    type_ids: &[Option<TypeTarget>],
    types: &Types,
) -> Vec<hir::VTable> {
    let mut out = Vec::new();
    for (i, target) in type_ids.iter().enumerate() {
        let Some(TypeTarget::Trait(trait_id)) = target else { continue };
        let def = types.trait_def(*trait_id);
        let mut entries = Vec::new();
        for ti in resolved.impls_of(i as u32) {
            let tag = match type_ids.get(ti as usize).and_then(|t| *t) {
                Some(TypeTarget::Struct(s)) => hir::TypeTag::Struct(s),
                Some(TypeTarget::Enum(e)) => hir::TypeTag::Enum(e),
                // A trait implementing a trait is already rejected; skip rather
                // than emit a row nothing can dispatch to.
                _ => continue,
            };
            let methods: Vec<hir::FnId> = def
                .methods
                .iter()
                .map(|m| {
                    resolved
                        .trait_method(ti, i as u32, &m.name)
                        .map(hir::FnId)
                        // A missing method is already an error; point the row at
                        // itself rather than panic, so checking continues.
                        .unwrap_or(hir::FnId(0))
                })
                .collect();
            entries.push(hir::VTableEntry { tag, methods });
        }
        entries.sort_by_key(|e| e.tag);
        out.push(hir::VTable { trait_id: *trait_id, entries });
    }
    out
}

#[derive(Clone, Copy, Debug)]
enum TypeTarget {
    Struct(kite_hir::StructId),
    Enum(kite_hir::EnumId),
    Trait(kite_hir::TraitId),
    /// A `type` alias, already expanded to what it names. An alias is
    /// interchangeable with its underlying type rather than distinct from it,
    /// so what this carries *is* the expansion.
    Alias(TyId),
}

/// Where a slice or map that is about to change in place is kept. See
/// `Checker::container_place`, which builds one, and `Checker::change_at`,
/// which changes what it holds.
struct Place {
    root: PlaceRoot,
    /// The slices between the root and the value being changed, outermost
    /// first: each is the element at `.0` of the one before it, and `.1` is
    /// that element's type.
    steps: Vec<(hir::Expr, TyId)>,
}

enum PlaceRoot {
    /// A `var` binding holding the value.
    Local { id: u32, ty: TyId },
    /// A `var` binding holding it as an optional, proved present here.
    Narrowed { id: u32, declared: TyId, ty: TyId },
    /// A `var` field of the struct `holder` reads.
    Field { holder: hir::Expr, index: u32, ty: TyId },
}

impl Place {
    /// The binding to change where it lies, when the value is a plain
    /// binding's own — every change this compiler could make before it
    /// understood places.
    fn binding(&self) -> Option<u32> {
        match self.root {
            PlaceRoot::Local { id, .. } if self.steps.is_empty() => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Signature {
    params: Vec<TyId>,
    /// What the body returns. A call to an `async fn` yields `Task<ret>`; the
    /// body still returns the value itself, because the state machine is what
    /// puts it into the task.
    ret: TyId,
    is_async: bool,
    fallible: bool,
    name_span: Span,
    /// For a method, the type its `self` has.
    self_ty: Option<TyId>,
    /// Type parameters, in declaration order. Empty for most functions.
    generics: Vec<GenericDef>,
}

/// How many parameters a closure handed to the host through `js.func` may
/// take.
///
/// Four, the same ceiling `js.call0 … js.call4` has, and for the same reason:
/// the arities are spelled out one by one because a slice is a Kite aggregate
/// and does not cross the boundary, so there is no variadic form to fall back
/// on. Four covers what the platform actually hands a callback — an event, a
/// value and an index, an entry list and its observer, a comparator's two
/// sides — and every one costs a trampoline in the module.
pub const JS_FUNC_MAX_ARITY: usize = 4;

/// One declared type parameter.
#[derive(Clone, Debug)]
struct GenericDef {
    name: String,
    /// The `Param` type standing for it while the body is checked generically.
    ty: TyId,
    /// Traits it must implement. A bound is what makes a method call on a
    /// parameter legal — without one, nothing is known about the type.
    bounds: Vec<kite_hir::TraitId>,
    span: Span,
}

/// Whether the right number of type arguments were given.
fn arity_ok(
    p: &ast::TypePath,
    want: usize,
    got: usize,
    decl: Span,
    diags: &mut DiagBag,
) -> bool {
    if want == got {
        return true;
    }
    let message = if want == 0 {
        format!("`{}` takes no type arguments", p.name())
    } else {
        format!(
            "`{}` takes {} type argument{}, but {} {} given",
            p.name(),
            want,
            if want == 1 { "" } else { "s" },
            got,
            if got == 1 { "was" } else { "were" }
        )
    };
    diags.push(
        Diagnostic::error(codes::E0208, message)
            .with_primary(p.span, "wrong number of type arguments")
            .with_secondary(decl, "declared here"),
    );
    false
}

/// A generic declaration named with no arguments at all.
fn missing_args(p: &ast::TypePath, want: usize, diags: &mut DiagBag) -> TyId {
    diags.push(
        Diagnostic::error(
            codes::E0208,
            format!("`{}` needs {} type argument{}", p.name(), want, if want == 1 { "" } else { "s" }),
        )
        .with_primary(p.span, "this names a generic declaration, not a type")
        .with_note(format!("write `{}<...>` with the types it holds", p.name())),
    );
    TyId::ERROR
}

/// Turn a declaration's `<T: Bound, U>` list into parameter types.
///
/// `before` is the list this one continues: a method's own parameters follow
/// its `impl` block's, so they are numbered after them and may not reuse a
/// name the block already declared.
fn declare_generics(
    params: &[ast::GenericParam],
    before: &[GenericDef],
    resolved: &ResolveMap,
    module: &str,
    type_ids: &[Option<TypeTarget>],
    types: &mut Types,
    diags: &mut DiagBag,
) -> Vec<GenericDef> {
    let mut out: Vec<GenericDef> = Vec::new();
    for p in params.iter() {
        if let Some(prev) = before.iter().chain(out.iter()).find(|g| g.name == p.name.name) {
            diags.push(
                Diagnostic::error(
                    codes::E0208,
                    format!("type parameter `{}` is declared twice", p.name.name),
                )
                .with_primary(p.name.span, "declared again here")
                .with_secondary(prev.span, "first declared here"),
            );
            continue;
        }
        let ty = types.param_ty((before.len() + out.len()) as u32, &p.name.name);
        let mut bounds = Vec::new();
        for b in &p.bounds {
            let found = resolved.type_by_name_in(module, &b.text());
            if let Some(i) = found {
                check_type_visible(resolved, module, i, b.span, &b.text(), diags);
            }
            match found.and_then(|i| type_ids[i as usize]) {
                Some(TypeTarget::Trait(tr)) => bounds.push(tr),
                _ => diags.push(
                    Diagnostic::error(
                        codes::E0208,
                        format!("`{}` is not a trait", b.name()),
                    )
                    .with_primary(b.span, "a bound must name a trait")
                    .with_note("a bound says what a type parameter can do; only a trait says that"),
                ),
            }
        }
        out.push(GenericDef { name: p.name.name.clone(), ty, bounds, span: p.name.span });
    }
    out
}

struct Checker<'a> {
    resolved: &'a ResolveMap,
    /// Every module-level constant's value, already worked out. A use of one
    /// becomes the value here, so nothing past this point sees a constant.
    consts: &'a ConstTable,
    /// The module whose body is being checked. Names written unqualified mean
    /// this module's first.
    module: String,
    /// The local holding the body's `defer`red calls, when it has any.
    ///
    /// A registration is a run-time event, not a place in the text: a `defer`
    /// inside a loop registers once per iteration, one inside an `if` only
    /// when the branch runs, and a `return` above a `defer` in a loop body can
    /// follow one registered on an earlier iteration. So the calls are kept
    /// where the running program can see them — a slice of closures, pushed at
    /// each registration and run backwards at each exit — rather than decided
    /// here from what the text has shown so far.
    ///
    /// It is set before the body is checked, from a scan for `defer`, so that
    /// every exit knows whether there is anything to run — including one
    /// written above the first `defer`.
    defers: Option<u32>,
    /// Built for release. The only thing it changes is that `assert` is
    /// dropped; everything else about the program is the same, because a
    /// build mode that changed semantics would make testing meaningless.
    release: bool,
    /// Where inference writes down what it decided, for an editor. Recorded
    /// here rather than re-derived later, because a second derivation is a
    /// second checker waiting to disagree with this one.
    solved: &'a mut Solved,
    /// What generic bodies demand of their arguments beyond their bounds, and
    /// the calls to hold to it. See [`GenericFacts`].
    facts: &'a mut GenericFacts,
    /// Each declared type's own `<T: Bound>` list, parallel to
    /// `resolved.types`. A bound there holds wherever a value is built.
    type_generics: &'a [Vec<GenericDef>],
    /// Each `impl` block's `<T: Bound>` list, by item index. Its methods take
    /// these first, from the receiver's own type.
    impl_generics: &'a std::collections::HashMap<usize, Vec<GenericDef>>,
    /// A trait method's own type parameters, by trait and position.
    trait_method_generics: &'a std::collections::HashMap<(hir::TraitId, usize), Vec<GenericDef>>,
    /// Which `impl` block implements a trait for a type: `(type index, trait
    /// index)` to item index. Its bounds are what a generic type needs of
    /// its arguments to implement the trait at all.
    trait_impls: &'a std::collections::HashMap<(u32, u32), usize>,
    sigs: &'a [Signature],
    /// Functions lifted out of this function's closure literals.
    lifted: Vec<hir::Function>,
    /// How many were already lifted out of earlier declarations. Ids are handed
    /// out before the lists are joined, so each checker has to know where its
    /// own share starts — without this, two functions' first closures both
    /// claim the same id.
    lifted_base: usize,
    /// The source region of the closure being checked. A local declared
    /// outside it is a capture.
    ///
    /// This is a span test rather than an id threshold because the checker
    /// pre-populates every local a function has, closure parameters included —
    /// so "declared later" says nothing about "declared inside".
    closure_span: Option<Span>,
    /// Captures of the closure currently being checked, in the order they are
    /// first read — which is the order the lifted function takes them.
    captures: Vec<u32>,
    /// The enclosing function's type parameters. Empty for most functions,
    /// which is why every lookup here is a linear scan.
    generic_defs: Vec<GenericDef>,
    /// The same, as the name-to-type pairs type resolution wants.
    generics: Vec<(String, TyId)>,
    /// Arena handles for each entry in `resolved.types`, parallel by index.
    type_ids: &'a [Option<TypeTarget>],
    /// The interned type arena, built up as declarations are checked.
    types: &'a mut Types,
    /// Every file in the compilation. A span carries which one it came
    /// from, so a program built from more than one source — the prelude and
    /// the user's file — reads back correctly.
    sources: &'a SourceMap,
    diags: &'a mut DiagBag,
    fn_index: usize,
    locals: Vec<hir::Local>,
    /// Definite-assignment state, parallel to `locals`.
    init: Vec<Init>,
    /// Error-taint state, parallel to `locals`. See [`Taint`].
    taint: Vec<Taint>,
    /// Which value local each error local guards, so checking the error cleans
    /// the value.
    guards: std::collections::HashMap<u32, u32>,
    /// Locals currently narrowed out of their optional, and to what.
    ///
    /// A local keeps one declared type for its whole life — rewriting it would
    /// leave the IR describing a local as `T` that the backend allocated as
    /// `Option<T>`. The narrowed *use* carries an explicit `Unwrap` instead.
    narrowed: std::collections::HashMap<u32, TyId>,
    /// Error locals control flow has proved are not nil.
    ///
    /// `error` is nil-able, so `err.message()` has to reach a value that is
    /// there. The backends disagree about what a nil receiver does — the VM
    /// answers with an empty string, Wasm traps on the cast — and the language
    /// answers by not letting the call be written until the error is known to
    /// be present.
    error_nonnil: std::collections::HashSet<u32>,
    /// The loops enclosing the statement being checked, innermost last. A
    /// closure starts with none: a loop around the place it is written is
    /// not one its body runs in.
    loops: Vec<Span>,
    /// The signature of the closure being checked, which is what a `return`
    /// inside it answers to — not the function the closure is written in.
    closure_sig: Option<Signature>,
    /// Set inside a closure whose body is an expression and whose return
    /// type nothing states. A `return` there has nothing to be checked
    /// against until the body's type is known, which is after the `return`.
    closure_ret_unknown: Option<Span>,
    /// Error locals already reported by E0302, so that a local reported where
    /// it left scope on one path is not reported again where the function
    /// ends.
    reported_unchecked: std::collections::HashSet<u32>,
}

/// Whether a local certainly holds a value at this point.
///
/// The specification permits `let x: int` followed by assignment in branches,
/// "provided the compiler can prove exactly one assignment occurs on every path
/// before first use". This is that proof: a lattice merged at every branch
/// join, which is the same machinery as the error-taint analysis.
///
/// It takes three states because the rule has two halves. A read needs the
/// local assigned on every path; a write to an immutable one needs it
/// assigned on *none*. A local written on some paths only satisfies neither,
/// and with two states it was indistinguishable from one written on no path —
/// so `if c { x = 1 }` followed by `x = 2` wrote an immutable binding twice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Init {
    /// Declared without a value and not yet assigned on this path.
    Unassigned,
    /// Assigned on some paths to here and not on others.
    Maybe,
    Assigned,
}

impl Init {
    /// A local keeps a state across a join only when every incoming path
    /// agrees on it.
    fn merge(self, other: Init) -> Init {
        if self == other {
            self
        } else {
            Init::Maybe
        }
    }
}

/// Error-taint state for one local.
///
/// A function returning `(T, error)` returns a **correlated pair**: the value
/// is only meaningful when the error is nil. This two-element lattice is the
/// proof, and it is what fixes Go's single biggest flaw — in Go the value on a
/// failure path is the zero value and flows onward looking valid.
///
/// The lattice has height two and merges at branch joins, exactly like the
/// definite-assignment analysis it sits beside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Taint {
    /// An ordinary local. Nothing to prove.
    Clean,
    /// A value bound from a fallible call whose error is not yet known to be
    /// nil. Reading it is `E0301`.
    Tainted,
    /// An error binding that has not been inspected. Letting it fall out of
    /// scope is `E0302`.
    Unchecked,
}

impl Taint {
    /// A value is clean after a join only when it is clean on *every* incoming
    /// path. That merge rule is what makes the analysis sound.
    fn merge(self, other: Taint) -> Taint {
        if self == other {
            self
        } else if self == Taint::Tainted || other == Taint::Tainted {
            Taint::Tainted
        } else if self == Taint::Unchecked || other == Taint::Unchecked {
            Taint::Unchecked
        } else {
            Taint::Clean
        }
    }
}

/// Whether a block always leaves via `return`, `break`, or `continue`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Falls,
    Diverges,
}

/// What the flow analysis knows at one point in a body, for a branch to start
/// from and a join to merge.
#[derive(Clone)]
struct FlowState {
    init: Vec<Init>,
    taint: Vec<Taint>,
    narrowed: std::collections::HashMap<u32, TyId>,
    error_nonnil: std::collections::HashSet<u32>,
}

/// What a trial check may change, kept to be put back.
struct Trial {
    diags: DiagBag,
    flow: FlowState,
    guards: std::collections::HashMap<u32, u32>,
    captures: Vec<u32>,
    reported: std::collections::HashSet<u32>,
    locals: usize,
    lifted: usize,
    /// The lengths of what [`Solved`] had recorded.
    solved: [usize; 4],
}

/// The enclosing function's flow state, set aside while a closure's body is
/// checked and put back afterwards.
struct Enclosing {
    init: Vec<Init>,
    taint: Vec<Taint>,
    narrowed: std::collections::HashMap<u32, TyId>,
    error_nonnil: std::collections::HashSet<u32>,
    defers: Option<u32>,
    loops: Vec<Span>,
    sig: Option<Signature>,
    ret_unknown: Option<Span>,
}

/// Whether an `error` or `(T, error)` value put into a binding may be a
/// failure nobody has looked at yet.
///
/// Everything may, except `nil` — a deliberate absence, with nothing in it to
/// drop — and a read of another binding, which carries its own obligation and
/// had it discharged by being read. Asking the question the other way round,
/// "is this a call", let a failure through whenever the call was wrapped:
/// `await f()`, `if c { f() } else { g() }`.
fn produces_failure(kind: &ExprKind) -> bool {
    !matches!(kind, ExprKind::Nil | ExprKind::Local(_) | ExprKind::Error)
}

impl Flow {
    fn merge(self, other: Flow) -> Flow {
        if self == Flow::Diverges && other == Flow::Diverges {
            Flow::Diverges
        } else {
            Flow::Falls
        }
    }
}

impl<'a> Checker<'a> {
    // ---- functions --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn check_body(
        &mut self,
        name: &str,
        is_pub: bool,
        is_async: bool,
        params: &[ast::Param],
        body: Option<&ast::Block>,
        body_span: Span,
        span: Span,
        sig: &Signature,
        takes_self: bool,
    ) -> hir::Function {
        let infos = &self.resolved.locals[self.fn_index];

        // Every local gets a slot up front. A method's `self` is local 0, so
        // its declared parameters are offset by one.
        let offset = usize::from(takes_self);
        self.locals = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                let ty = if takes_self && i == 0 {
                    sig.self_ty.unwrap_or(TyId::ERROR)
                } else {
                    sig.params.get(i - offset).copied().unwrap_or(TyId::ERROR)
                };
                hir::Local {
                    name: info.name.clone(),
                    ty,
                    mutable: info.mutable,
                    span: info.span,
                    synthetic: info.synthetic,
                }
            })
            .collect();

        let param_count = params.len() + offset;

        // Parameters always hold a value; everything else starts unassigned.
        self.init = (0..self.locals.len())
            .map(|i| {
                if i < param_count {
                    Init::Assigned
                } else {
                    Init::Unassigned
                }
            })
            .collect();

        self.taint = vec![Taint::Clean; self.locals.len()];

        let (hir_body, flow) = match body {
            Some(b) => {
                self.defers = self.defer_stack_for(b.stmts.iter().any(stmt_defers), b.span);
                let (hir_body, flow) = self.block(b, sig);
                (self.with_defers(hir_body, flow, b.span), flow)
            }
            // A trait method with no default body. Nothing to check.
            None => (hir::Block::default(), Flow::Diverges),
        };
        self.defers = None;

        self.report_unchecked_errors(None);

        // A body the parser could not read to its end — its last statement
        // failed, or its `}` was never found — has had its syntax error
        // reported already, and whether the rest would have returned is not
        // something to guess at in a second diagnostic.
        let cut_short = matches!(body.and_then(|b| b.stmts.last()), Some(ast::Stmt::Error(_)));
        if body.is_some() && sig.ret != TyId::UNIT && flow == Flow::Falls && !cut_short {
            self.diags.push(
                Diagnostic::error(codes::E0203, "not every path returns a value")
                    .with_primary(
                        Span::empty_at(body_span.file, body_span.end.saturating_sub(1)),
                        "control reaches the end of the function here",
                    )
                    .with_secondary(
                        sig.name_span,
                        format!("`{}` declared here", self.types.name(sig.ret)),
                    ),
            );
        }

        hir::Function {
            generic_count: sig.generics.len(),
            name: name.to_string(),
            is_free: self.resolved.fns[self.fn_index].owner.is_none(),
            is_pub,
            is_async,
            param_count,
            locals: std::mem::take(&mut self.locals),
            ret: sig.ret,
            body: hir_body,
            span,
        }
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self, b: &ast::Block, sig: &Signature) -> (hir::Block, Flow) {
        let mut out = hir::Block::default();
        let mut flow = Flow::Falls;
        // A guard clause narrows for the rest of *this* block and no further,
        // so the set is restored on the way out — less whatever the block
        // disproved, by assigning to what was narrowed.
        let entry_narrowed = self.narrowed.clone();
        let entry_nonnil = self.error_nonnil.clone();

        for s in &b.stmts {
            if flow == Flow::Diverges {
                self.diags.push(
                    Diagnostic::warning(codes::E0116, "unreachable code")
                        .with_primary(s.span(), "this statement can never run")
                        .with_note("the preceding statement always leaves the block"),
                );
                break;
            }
            if let Some((stmt, f)) = self.stmt(s, sig) {
                out.stmts.push(stmt);
                flow = f;
            }
        }
        self.narrowed = entry_narrowed
            .into_iter()
            .filter(|(id, ty)| self.narrowed.get(id) == Some(ty))
            .collect();
        self.error_nonnil = entry_nonnil.intersection(&self.error_nonnil).copied().collect();
        (out, flow)
    }

    fn stmt(&mut self, s: &ast::Stmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        match s {
            ast::Stmt::Let(l) => self.let_stmt(l, sig),
            ast::Stmt::Var(v) => self.var_stmt(v, sig),
            ast::Stmt::Assign(a) => self.assign_stmt(a, sig),
            ast::Stmt::Return(r) => self.return_stmt(r, sig),
            ast::Stmt::If(i) => self.if_stmt(i, sig),
            ast::Stmt::For(f) => self.for_stmt(f, sig),
            ast::Stmt::Match(m) => {
                // A match every arm of which returns is how a multi-statement
                // arm is written, since a block in value position must be a
                // single expression. Reading divergence off the arms is what
                // lets the function containing it be seen to return.
                let (e, flow) = self.match_expr_with_flow(m, None);
                Some((hir::Stmt::Expr(e), flow))
            }

            ast::Stmt::Break { label, span } => Some((
                hir::Stmt::Break { label: label.as_ref().map(|l| l.name.clone()), span: *span },
                Flow::Diverges,
            )),
            ast::Stmt::Continue { label, span } => Some((
                hir::Stmt::Continue { label: label.as_ref().map(|l| l.name.clone()), span: *span },
                Flow::Diverges,
            )),

            ast::Stmt::Expr(e) => {
                self.inert_closure(e);
                let expr = self.expr(e, None);
                self.dropped_error(&expr);
                let flow = if expr.ty == TyId::NEVER { Flow::Diverges } else { Flow::Falls };
                Some((hir::Stmt::Expr(expr), flow))
            }

            // `_ = f()`. The value is evaluated and thrown away, which is the
            // same lowering an expression statement gets — the difference is
            // entirely that somebody wrote it down.
            ast::Stmt::Discard { value, .. } => {
                let expr = self.expr(value, None);
                let flow = if expr.ty == TyId::NEVER { Flow::Diverges } else { Flow::Falls };
                Some((hir::Stmt::Expr(expr), flow))
            }

            ast::Stmt::Check { expr, span } => self.check_stmt(expr, *span, sig),
            ast::Stmt::Defer { expr, span } => self.defer_stmt(expr, *span),
            ast::Stmt::Error(_) => None,
        }
    }

    fn let_stmt(&mut self, l: &ast::LetStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        if let ast::Binding::Tuple { elems, span } = &l.binding {
            return self.let_pair(l, elems, *span);
        }
        let ast::Binding::Name(name) = &l.binding else {
            unreachable!("a binding is a name or a tuple")
        };

        let local_id = self.resolved.lookup_binding(name.span)?;
        let annotated = l.ty.as_ref().map(|t| self.resolve_type(t));

        let init = l.init.as_ref().map(|e| {
            let v = self.expr(e, annotated);
            self.coerce(v, annotated)
        });

        let ty = match (annotated, &init) {
            (Some(a), Some(i)) => {
                self.expect_ty(i.ty, a, i.span, l.ty.as_ref().map(|t| t.span()));
                a
            }
            (Some(a), None) => a,
            (None, Some(i)) => {
                if i.ty == TyId::UNIT {
                    self.diags.push(
                        Diagnostic::error(codes::E0200, "cannot bind a value of type `()`")
                            .with_primary(i.span, "this expression produces no value")
                            .with_note("a function without a declared return type returns `()`"),
                    );
                    TyId::ERROR
                } else if i.ty == TyId::NEVER {
                    TyId::ERROR
                } else {
                    i.ty
                }
            }
            (None, None) => {
                self.diags.push(
                    Diagnostic::error(codes::E0204, "cannot infer a type for this binding")
                        .with_primary(name.span, "no type annotation and no initialiser")
                        .with_note("write `let x: int` or give it a value"),
                );
                TyId::ERROR
            }
        };

        self.locals[local_id as usize].ty = ty;
        // The binding's type was worked out rather than written, which is
        // exactly what an inlay hint exists to show.
        if l.ty.is_none() && init.is_some() && !self.types.is_poisoned(ty) {
            self.solved.bindings.push((name.span, self.types.name(ty)));
        }
        // A `let` with an initialiser holds a value from here on; one without
        // is unassigned until a branch writes it. Said explicitly rather than
        // left to the state every local starts in, because a loop body runs
        // this declaration afresh on every iteration.
        self.init[local_id as usize] =
            if init.is_some() { Init::Assigned } else { Init::Unassigned };
        // **A failure bound to a name is still a failure.** `let (v, err) = f()`
        // has always marked `err`, and a bare `f()` on its own line is caught
        // too — but a function returning `error` alone, bound and never looked
        // at, went through silently. That is the shape a caller writes by
        // habit against a library whose writes cannot fail usefully:
        // `let e = dom.set_text(node, body)` and nothing after it. Worse,
        // `--explain E0302` recommends exactly that spelling — "bind it and
        // test it" — for the half of the rule that was enforced.
        //
        // `let e: error = nil` is not marked: it is a deliberate absence and
        // there is nothing there to drop. Nor is a copy of another binding.
        // Reading the binding anywhere clears the mark, because reading an
        // error is inspecting it.
        //
        // A whole `(T, error)` bound to one name is the same hole one level
        // up: `let p = load()` never destructures, so R1 never marks an error
        // and the failure inside `p` went out of scope in silence. Taking it
        // apart later, or returning it, reads it and clears the mark.
        self.taint[local_id as usize] = match &init {
            Some(i) if self.may_hold_failure(ty) && produces_failure(&i.kind) => Taint::Unchecked,
            _ => Taint::Clean,
        };
        let _ = sig;
        Some((
            hir::Stmt::Let { local: hir::LocalId(local_id), init, span: l.span },
            Flow::Falls,
        ))
    }

    fn var_stmt(&mut self, v: &ast::VarStmt, _sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        let local_id = self.resolved.lookup_binding(v.name.span)?;
        let annotated = v.ty.as_ref().map(|t| self.resolve_type(t));
        let init = self.expr(&v.init, annotated);
        let init = self.coerce(init, annotated);

        let ty = match annotated {
            Some(a) => {
                self.expect_ty(init.ty, a, init.span, v.ty.as_ref().map(|t| t.span()));
                a
            }
            None if init.ty == TyId::UNIT || init.ty == TyId::NEVER => TyId::ERROR,
            None => init.ty,
        };
        self.locals[local_id as usize].ty = ty;
        self.init[local_id as usize] = Init::Assigned;
        if v.ty.is_none() && !self.types.is_poisoned(ty) {
            self.solved.bindings.push((v.name.span, self.types.name(ty)));
        }
        // The same obligation a `let` takes on: `var e = f()` holds a failure
        // just as surely, and being able to reassign it later does not mean
        // anybody looked at this one.
        self.taint[local_id as usize] =
            if self.may_hold_failure(ty) && produces_failure(&init.kind) {
                Taint::Unchecked
            } else {
                Taint::Clean
            };

        Some((
            hir::Stmt::Let { local: hir::LocalId(local_id), init: Some(init), span: v.span },
            Flow::Falls,
        ))
    }

    fn assign_stmt(&mut self, a: &ast::AssignStmt, _sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        if let ast::Expr::Field { base, name, span } = &a.target {
            return self.assign_field(base, name, *span, a);
        }
        if let ast::Expr::Index { base, index, span } = &a.target {
            return self.assign_index(base, index, *span, a);
        }
        let ast::Expr::Path(p) = &a.target else {
            self.diags.push(
                Diagnostic::error(codes::E0200, "cannot assign to this expression")
                    .with_primary(a.target.span(), "not something a value can be written to")
                    .with_note(
                        "a write names a binding, a field or an index — `x = …`, \
                         `p.x = …`, `xs[i] = …`. Anything else is a value, and a value \
                         has nowhere to put one",
                    ),
            );
            return None;
        };
        // A module-level constant is a name for a value, so there is nowhere
        // to write. Saying so here matters more than it looks: without it the
        // assignment resolved to something that is not a local, fell through,
        // and was silently dropped.
        if let Some(Res::Const(index)) = self.resolved.lookup_use(p.span) {
            let decl = self.resolved.consts[index as usize].span;
            self.diags.push(
                Diagnostic::error(
                    codes::E0114,
                    format!("cannot assign to the constant `{}`", p.text()),
                )
                .with_primary(p.span, "cannot assign")
                .with_secondary(decl, "declared as a constant here")
                .with_note(
                    "a constant is a name for a value, not a place to put one; for something \
                     that changes, put it in a struct and pass it to what changes it",
                ),
            );
            return None;
        }
        let Some(Res::Local(local_id)) = self.resolved.lookup_use(p.span) else {
            return None;
        };

        let slot = local_id as usize;
        let local_ty = self.locals[slot].ty;
        let mutable = self.locals[slot].mutable;
        let decl_span = self.locals[slot].span;
        let name = self.locals[slot].name.clone();

        // A closure's captures are copies taken when it was made, so a write
        // to one inside the closure would change the copy and nothing else —
        // and the lifted body has no slot for a local it never captured, so
        // the write used to land wherever that number pointed. A `var` is the
        // capture §4.5 already refuses; a `let` is refused for the same
        // reason, since even a first write would be invisible outside.
        if let Some(region) = self.closure_span {
            if !self.declared_inside(local_id, region) {
                if mutable {
                    self.note_capture(local_id, p.span);
                } else {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0211,
                            format!("a closure cannot assign to `{}`, which it captures", name),
                        )
                        .with_primary(p.span, "assigned inside the closure")
                        .with_secondary(decl_span, "declared outside it")
                        .with_note(
                            "a closure holds copies of what it captures, taken when it is \
                             made, so the write would change the copy and nothing else",
                        )
                        .with_note(
                            "assign it before making the closure, or have the closure return \
                             the value",
                        ),
                    );
                }
                let _ = self.expr(&a.value, Some(local_ty));
                return None;
            }
        }

        // The right-hand side is evaluated before the write, so it is checked
        // against the state before it: `let x: int` then `x = x + 1` reads an
        // `x` that has no value yet.
        let before = self.init[slot];
        let value = self.expr(&a.value, Some(local_ty));
        let (value, written) = match a.op.to_binary() {
            None => {
                let written = value.ty;
                // Subsumption applies to an assignment as to an initialiser:
                // `found = frame` where `found` is an `Option<Frame>` wraps.
                let value = self.coerce(value, Some(local_ty));
                self.expect_ty(value.ty, local_ty, value.span, Some(decl_span));
                (value, written)
            }
            Some(binop) => {
                // `n += 1` is checked as `n = n + 1`, so the operand rules and
                // their messages are shared — and so is the read: the old
                // value has to be there, and has to be one whose error was
                // checked, exactly as if `n` had been written on the right.
                let lhs = self.path_expr(p);
                let sum = self.binary(binop, lhs, value, a.span);
                let written = sum.ty;
                let sum = self.coerce(sum, Some(local_ty));
                self.expect_ty(sum.ty, local_ty, sum.span, Some(decl_span));
                (sum, written)
            }
        };

        // An immutable binding may be written exactly once, and only if it was
        // declared without an initialiser. That is what makes
        // `let z: int` followed by branch assignment legal — and why a write
        // after a branch that *may* have written it is refused too.
        // A compound assignment's read has already reported a binding that
        // may have no value, and that is the one mistake here.
        let compound = a.op.to_binary().is_some();
        if !mutable {
            match before {
                Init::Assigned => self.immutable_write(&name, decl_span, p.span, None),
                Init::Maybe if compound => {}
                Init::Maybe => self.immutable_write(
                    &name,
                    decl_span,
                    p.span,
                    Some("on some path to here it has been assigned already"),
                ),
                Init::Unassigned if compound => {}
                Init::Unassigned => {
                    // Inside a loop the assignment could run more than once,
                    // which would be a second write to an immutable binding —
                    // unless the binding is declared inside that same loop,
                    // when each iteration has a fresh one.
                    let in_loop = self
                        .loops
                        .last()
                        .is_some_and(|l| !self.declared_inside(local_id, *l));
                    if in_loop {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0114,
                                format!(
                                    "cannot assign to immutable binding `{}` inside a loop",
                                    name
                                ),
                            )
                            .with_primary(p.span, "this assignment may run more than once")
                            .with_secondary(decl_span, "declared immutable here")
                            .with_note("declare it `var` if it is meant to change"),
                        );
                    }
                }
            }
        }
        self.init[slot] = Init::Assigned;

        // A write replaces the value every earlier test was about. `x != nil`
        // said nothing about what `x = nil` put there, and a narrowing kept
        // past it unwrapped a nil. A value that is itself not optional keeps
        // an existing narrowing, since it cannot be nil either.
        let keeps_narrowing = matches!(
            *self.types.kind(local_ty),
            TyKind::Optional(inner) if inner == written && !self.types.is_poisoned(written)
        );
        if !keeps_narrowing {
            self.narrowed.remove(&local_id);
        }
        self.error_nonnil.remove(&local_id);

        Some((
            hir::Stmt::Assign { local: hir::LocalId(local_id), value, span: a.span },
            Flow::Falls,
        ))
    }

    /// Whether a binding of this type carries the obligation to be checked:
    /// an `error`, or a whole `(T, error)`.
    fn may_hold_failure(&self, ty: TyId) -> bool {
        ty == TyId::ERR || self.types.fallible_value(ty).is_some()
    }

    /// A second write to an immutable binding.
    fn immutable_write(&mut self, name: &str, decl: Span, at: Span, why: Option<&str>) {
        let mut d = Diagnostic::error(
            codes::E0114,
            format!("cannot assign to immutable binding `{}`", name),
        )
        .with_secondary(decl, "declared immutable here")
        .with_primary(at, "cannot assign");
        if let Some(why) = why {
            d = d.with_note(format!(
                "a `let` without an initialiser is assigned exactly once; {}",
                why
            ));
        }
        if let Some(kw) = self.let_keyword_span(decl) {
            d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
        }
        self.diags.push(d);
    }

    /// `defer file.close()` — run this when the function returns, by any path.
    ///
    /// The call is checked here, where its arguments still mean what they did:
    /// deferring evaluates the receiver and arguments now and the call later,
    /// which is what makes `defer file.close()` close *that* file.
    ///
    /// Unlike Go's, a deferred call cannot modify the return value. It is for
    /// releasing what was taken, which is the only use that survives scrutiny.
    fn defer_stmt(&mut self, expr: &ast::Expr, span: Span) -> Option<(hir::Stmt, Flow)> {
        if !matches!(expr, ast::Expr::Call { .. }) {
            self.diags.push(
                Diagnostic::error(codes::E0200, "`defer` takes a call")
                    .with_primary(expr.span(), "this is not a call")
                    .with_note(
                        "`defer` runs something when the function returns; an expression \
                         that is not a call has nothing to run",
                    ),
            );
            return None;
        }
        let mut call = self.expr(expr, None);
        if matches!(call.kind, ExprKind::Error) {
            return None;
        }
        // Set before the body was checked, by the same scan that found this
        // statement; absent only if the two disagree about what a `defer` is.
        let stack = self.defers?;
        // What reaches here has to be a call the exit can make later. A few
        // things written as calls are operations on a local — `xs.push(v)`,
        // `m.remove(k)` — and those change the binding in place, which is
        // meaningless once the function has left and wrong to do to a copy.
        if !matches!(
            call.kind,
            ExprKind::Call { .. }
                | ExprKind::CallVirtual { .. }
                | ExprKind::CallBuiltin { .. }
                | ExprKind::CallExtern { .. }
                | ExprKind::StrOp { .. }
                | ExprKind::CallClosure { .. }
        ) {
            self.diags.push(
                Diagnostic::error(codes::E0200, "`defer` takes a call to a function")
                    .with_primary(expr.span(), "this is not something an exit can call")
                    .with_note(
                        "a deferred call runs after the body has finished, so it has to be \
                         a function or a method; an operation on a local — `push`, `remove` \
                         — would change a binding nothing can read any more",
                    ),
            );
            return None;
        }
        // Evaluate the receiver and arguments *now*, into hidden locals the
        // deferred call reads at exit. That is what makes `defer f.close()`
        // close the file `f` names here rather than whatever `f` names by the
        // time the function returns — and in a loop, what makes each
        // iteration's registration keep that iteration's values.
        let mut stmts = self.hoist_deferred_operands(&mut call, span);
        let captures: Vec<u32> = stmts
            .iter()
            .filter_map(|s| match s {
                hir::Stmt::Let { local, .. } => Some(local.0),
                _ => None,
            })
            .collect();
        // The call becomes a closure over those values, pushed onto the
        // body's stack; the exits run the stack backwards.
        let func = self.lift(
            &captures,
            &[],
            hir::Block { stmts: vec![hir::Stmt::Expr(call)] },
            TyId::UNIT,
            span,
        );
        let ty = self.types.fn_of(Vec::new(), TyId::UNIT);
        let captured = captures
            .iter()
            .map(|id| hir::Expr {
                kind: ExprKind::Local(hir::LocalId(*id)),
                ty: self.locals[*id as usize].ty,
                span,
            })
            .collect();
        let targs = self.generic_defs.iter().map(|g| g.ty).collect();
        stmts.push(hir::Stmt::SlicePush {
            local: hir::LocalId(stack),
            value: hir::Expr {
                kind: ExprKind::ClosureNew { func, captures: captured, targs },
                ty,
                span,
            },
            span,
        });
        Some((hir::Stmt::Block(hir::Block { stmts }), Flow::Falls))
    }

    /// The local a body's `defer`s are pushed onto, if it has any.
    ///
    /// `span` is the body's own, which is what puts a closure's stack inside
    /// the closure — so it moves with the closure when the closure is lifted.
    fn defer_stack_for(&mut self, has_defer: bool, span: Span) -> Option<u32> {
        if !has_defer {
            return None;
        }
        let call = self.types.fn_of(Vec::new(), TyId::UNIT);
        let ty = self.types.slice_of(call);
        Some(self.synthetic_local("deferred", ty, span))
    }

    /// A finished body, with its `defer` stack created on entry and run where
    /// control falls off the end. A body that always returns has run it at
    /// every `return` already.
    fn with_defers(&mut self, mut body: hir::Block, flow: Flow, span: Span) -> hir::Block {
        let Some(stack) = self.defers else { return body };
        if flow == Flow::Falls {
            let run = self.run_deferred(span);
            body.stmts.extend(run);
        }
        let ty = self.locals[stack as usize].ty;
        body.stmts.insert(
            0,
            hir::Stmt::Let {
                local: hir::LocalId(stack),
                init: Some(hir::Expr { kind: ExprKind::SliceNew { elems: Vec::new() }, ty, span }),
                span,
            },
        );
        body
    }

    /// Bind a deferred call's operands to hidden locals, leaving the call
    /// reading those locals instead of the original expressions.
    ///
    /// A literal is left alone: it cannot change, so a temporary for it would
    /// be a register the backends carry for nothing.
    fn hoist_deferred_operands(&mut self, call: &mut hir::Expr, span: Span) -> Vec<hir::Stmt> {
        let mut stmts = Vec::new();
        let mut args = match &mut call.kind {
            ExprKind::Call { args, .. }
            | ExprKind::CallVirtual { args, .. }
            | ExprKind::CallBuiltin { args, .. }
            | ExprKind::CallExtern { args, .. }
            | ExprKind::StrOp { args, .. }
            | ExprKind::CallClosure { args, .. } => std::mem::take(args),
            _ => return stmts,
        };
        if let ExprKind::CallClosure { callee, .. } = &mut call.kind {
            // The function value itself is an operand like any other: which
            // closure runs is decided here, not at the exit.
            self.hoist_one(callee, &mut stmts, span);
        }
        for a in &mut args {
            self.hoist_one(a, &mut stmts, span);
        }
        match &mut call.kind {
            ExprKind::Call { args: slot, .. }
            | ExprKind::CallVirtual { args: slot, .. }
            | ExprKind::CallBuiltin { args: slot, .. }
            | ExprKind::CallExtern { args: slot, .. }
            | ExprKind::StrOp { args: slot, .. }
            | ExprKind::CallClosure { args: slot, .. } => *slot = args,
            _ => {}
        }
        stmts
    }

    fn hoist_one(&mut self, e: &mut hir::Expr, out: &mut Vec<hir::Stmt>, span: Span) {
        if matches!(
            e.kind,
            ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::Nil
        ) {
            return;
        }
        let (ty, at) = (e.ty, e.span);
        let id = self.synthetic_local("deferred_arg", ty, span);
        let value = std::mem::replace(
            e,
            hir::Expr { kind: ExprKind::Local(hir::LocalId(id)), ty, span: at },
        );
        out.push(hir::Stmt::Let { local: hir::LocalId(id), init: Some(value), span });
    }

    /// A `return`, with whatever was deferred running first.
    ///
    /// The returned value is evaluated *before* the deferred calls — it was
    /// written before them — so it is bound to a hidden local here and that
    /// local is what the return hands back. Without the binding the expression
    /// would be evaluated after the deferred stack had run, and a deferred
    /// mutation could change the answer.
    fn returning(&mut self, ret: hir::Stmt, span: Span) -> hir::Stmt {
        if self.defers.is_none() {
            return ret;
        }
        let mut stmts = Vec::new();
        let ret = match ret {
            hir::Stmt::Return { value: Some(v), span: rs } => {
                let (ty, at) = (v.ty, v.span);
                let id = self.synthetic_local("returned", ty, span);
                stmts.push(hir::Stmt::Let { local: hir::LocalId(id), init: Some(v), span });
                hir::Stmt::Return {
                    value: Some(hir::Expr {
                        kind: ExprKind::Local(hir::LocalId(id)),
                        ty,
                        span: at,
                    }),
                    span: rs,
                }
            }
            other => other,
        };
        let run = self.run_deferred(span);
        stmts.extend(run);
        stmts.push(ret);
        hir::Stmt::Block(hir::Block { stmts })
    }

    /// Every call registered so far on this run, newest first. Emitted before
    /// every `return` and at the end of the body.
    ///
    /// `for k in 0..n { stack[n - 1 - k]() }`, written out in the forms the
    /// backends already lower, so a `defer` needs nothing of its own below
    /// this pass. `span` must lie inside the body, so the counter moves with
    /// a closure's body when it is lifted.
    fn run_deferred(&mut self, span: Span) -> Vec<hir::Stmt> {
        let Some(stack) = self.defers else { return Vec::new() };
        let stack_ty = self.locals[stack as usize].ty;
        let call_ty = self.types.slice_elem(stack_ty).unwrap_or(TyId::ERROR);
        let k = self.synthetic_local("deferred_index", TyId::INT, span);
        let int = |kind: ExprKind| hir::Expr { kind, ty: TyId::INT, span };
        let len = || {
            int(ExprKind::SliceLen {
                base: Box::new(hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(stack)),
                    ty: stack_ty,
                    span,
                }),
            })
        };
        // Neither subtraction can overflow — `k < n` — so the checked form is
        // as good as the wrapping one in either build.
        let last = int(ExprKind::Binary {
            op: hir::BinOp::SubInt,
            lhs: Box::new(len()),
            rhs: Box::new(int(ExprKind::Int(1))),
        });
        let index = int(ExprKind::Binary {
            op: hir::BinOp::SubInt,
            lhs: Box::new(last),
            rhs: Box::new(int(ExprKind::Local(hir::LocalId(k)))),
        });
        let callee = hir::Expr {
            kind: ExprKind::Index {
                base: Box::new(hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(stack)),
                    ty: stack_ty,
                    span,
                }),
                index: Box::new(index),
            },
            ty: call_ty,
            span,
        };
        let call = hir::Expr {
            kind: ExprKind::CallClosure { callee: Box::new(callee), args: Vec::new() },
            ty: TyId::UNIT,
            span,
        };
        vec![hir::Stmt::ForRange {
            var: hir::LocalId(k),
            start: int(ExprKind::Int(0)),
            end: len(),
            inclusive: false,
            body: hir::Block { stmts: vec![hir::Stmt::Expr(call)] },
            label: None,
            span,
        }]
    }

    fn return_stmt(&mut self, r: &ast::ReturnStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        if let Some(closure) = self.closure_ret_unknown {
            self.diags.push(
                Diagnostic::error(
                    codes::E0211,
                    "this `return` needs the closure's return type written",
                )
                .with_primary(r.span, "returns from a closure whose type is not known yet")
                .with_secondary(closure, "the closure's body is an expression, and nothing says what it returns")
                .with_note(
                    "a closure's result type comes from its body, which is only known once \
                     the whole body is checked — write it, as in `|x: int| -> int …`",
                ),
            );
            // Still a way out, so the arm it ends is not also reported as
            // producing `()`.
            match &r.value {
                Some(ast::ReturnValue::Single(e)) | Some(ast::ReturnValue::Fail { error: e, .. }) => {
                    let _ = self.expr(e, None);
                }
                Some(ast::ReturnValue::Pair { value, error, .. }) => {
                    let _ = self.expr(value, None);
                    let _ = self.expr(error, None);
                }
                None => {}
            }
            return Some((hir::Stmt::Return { value: None, span: r.span }, Flow::Diverges));
        }
        match &r.value {
            None => {
                if sig.ret != TyId::UNIT {
                    self.diags.push(
                        Diagnostic::error(codes::E0203, "missing return value")
                            .with_primary(r.span, format!("expected a `{}`", self.types.name(sig.ret)))
                            .with_secondary(sig.name_span, "declared here"),
                    );
                }
                Some((self.returning(hir::Stmt::Return { value: None, span: r.span }, r.span), Flow::Diverges))
            }
            Some(ast::ReturnValue::Single(e)) if sig.fallible => {
                let inner = self.types.fallible_value(sig.ret).unwrap_or(TyId::ERROR);
                let value = self.expr(e, Some(sig.ret));
                // `return parse_user(raw)` — passing a fallible call's result
                // straight out. The pair travels as a pair: its value is still
                // only valid when its error is nil, and the caller's taint
                // analysis is what enforces that, so nothing is lost by not
                // taking it apart here.
                if self.types.fallible_value(value.ty) == Some(inner) {
                    let ret = hir::Stmt::Return { value: Some(value), span: r.span };
                    return Some((self.returning(ret, r.span), Flow::Diverges));
                }
                self.expect_ty(value.ty, inner, value.span, Some(sig.name_span));
                self.diags.push(
                    Diagnostic::error(codes::E0203, "a fallible function returns two values")
                        .with_primary(r.span, "only one value returned")
                        .with_secondary(sig.name_span, "declared `(T, error)` here")
                        .with_note(
                            "write `return value, nil` on the success path, or return a call \
                             that is itself fallible",
                        ),
                );
                None
            }

            Some(ast::ReturnValue::Single(e)) => {
                let value = self.expr(e, Some(sig.ret));
                let value = self.coerce(value, Some(sig.ret));
                if sig.ret == TyId::UNIT {
                    self.diags.push(
                        Diagnostic::error(codes::E0200, "returning a value from a `()` function")
                            .with_primary(value.span, format!("this is {}", self.types.with_article(value.ty)))
                            .with_secondary(sig.name_span, "no return type declared here"),
                    );
                } else {
                    self.expect_ty(value.ty, sig.ret, value.span, Some(sig.name_span));
                }
                let ret = hir::Stmt::Return { value: Some(value), span: r.span };
                Some((self.returning(ret, r.span), Flow::Diverges))
            }
            Some(ast::ReturnValue::Pair { value, error, span }) => {
                if !sig.fallible {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            "returning a pair from a function that is not fallible",
                        )
                        .with_primary(*span, "two values returned here")
                        .with_secondary(sig.name_span, "declare `-> (T, error)` to return a pair"),
                    );
                    return None;
                }
                let inner = self.types.fallible_value(sig.ret).unwrap_or(TyId::ERROR);
                let v = self.expr(value, Some(inner));
                // The value side needs the same conversion the error side gets
                // below, and for the same reason. `expect_ty` waves a `T`
                // through where an `Option<T>` is wanted — that much is
                // correct — but without the `Wrap` the backend puts a bare `T`
                // into a slot typed `Option<T>`, and the module it writes does
                // not validate: `kitec check` passed and `kitec build` failed
                // with E0900, on `return value, nil` in a function returning
                // `(Option<Json>, error)`. Both applications carry a comment
                // telling the next person to bind through a `let` first.
                let v = self.coerce(v, Some(inner));
                self.expect_ty(v.ty, inner, v.span, Some(sig.name_span));
                let e = self.expr(error, Some(TyId::ERR));
                // A type implementing `Error` becomes one here. `expect_ty`
                // deliberately does *not* wave it through on its own: a site
                // that accepted the value without converting it would hand a
                // raw struct to `err.message()` and trap at run time, which is
                // exactly what happened the first time this was written.
                let e = self.coerce(e, Some(TyId::ERR));
                self.expect_ty(e.ty, TyId::ERR, e.span, None);
                let ret = hir::Stmt::Return {
                    value: Some(hir::Expr {
                        kind: ExprKind::PairNew { value: Box::new(v), error: Box::new(e) },
                        ty: sig.ret,
                        span: *span,
                    }),
                    span: r.span,
                };
                Some((self.returning(ret, r.span), Flow::Diverges))
            }

            // `return _, err` — the failure arm. There is deliberately no
            // value on this path, which is what stops Go's zero-value leak.
            Some(ast::ReturnValue::Fail { error, span }) => {
                if !sig.fallible {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            "`return _, err` needs a fallible function",
                        )
                        .with_primary(*span, "no error slot to return through")
                        .with_secondary(sig.name_span, "declare `-> (T, error)` here"),
                    );
                    return None;
                }
                let e = self.expr(error, Some(TyId::ERR));
                let e = self.coerce(e, Some(TyId::ERR));
                self.expect_ty(e.ty, TyId::ERR, e.span, None);
                self.mark_checked(error);
                let ret = hir::Stmt::Return {
                    value: Some(hir::Expr {
                        kind: ExprKind::PairNew {
                            value: Box::new(hir::Expr {
                                kind: ExprKind::Nil,
                                ty: TyId::ERROR,
                                span: *span,
                            }),
                            error: Box::new(e),
                        },
                        ty: sig.ret,
                        span: *span,
                    }),
                    span: r.span,
                };
                Some((self.returning(ret, r.span), Flow::Diverges))
            }
        }
    }

    fn if_stmt(&mut self, i: &ast::IfStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        // `if err != nil { … }` is the explicit form of a check. Recognising it
        // here is what lets a hand-written test clean the value it guards.
        let tested = self.error_tested_by(&i.cond);
        // `if x == nil { … } else { … }` narrows `x` in whichever branch it
        // cannot be nil. Kite has no `?` sigil of any kind: an inline `if`
        // does the same work, in the open.
        let narrowing = self.nil_test(&i.cond);
        let cond = self.condition(&i.cond);
        if let Some((id, _)) = tested {
            if self.taint[id as usize] == Taint::Unchecked {
                self.taint[id as usize] = Taint::Clean;
            }
        }

        // Each branch is checked from the same entry state, and the states are
        // merged at the join. A branch that diverges contributes nothing to the
        // join, because control never arrives from it.
        let entry = self.flow_state();
        self.enter_branch(narrowing, tested, true);
        let (then, then_flow) = self.block(&i.then, sig);
        if then_flow == Flow::Diverges {
            self.report_unchecked_errors(Some(i.then.span));
        }
        let then_exit = self.flow_state();
        self.set_flow_state(entry);

        // The else path exists whether or not it is written: without an
        // `else`, control arrives at the join having skipped the `then`, and
        // it arrives knowing the condition was false. An `else if` starts from
        // that knowledge too, so `if x == nil { … } else if c { … }` has `x`
        // narrowed in the second test's branches.
        self.enter_branch(narrowing, tested, false);
        let (else_, else_flow) = match i.else_.as_deref() {
            None => (None, Flow::Falls),
            Some(ast::ElseBranch::Block(b)) => {
                let (blk, f) = self.block(b, sig);
                if f == Flow::Diverges {
                    self.report_unchecked_errors(Some(b.span));
                }
                (Some(blk), f)
            }
            Some(ast::ElseBranch::If(nested)) => {
                let (stmt, f) = self.if_stmt(nested, sig)?;
                (Some(hir::Block { stmts: vec![stmt] }), f)
            }
        };
        let else_exit = self.flow_state();

        // `if err != nil { return … }` leaves only the path where the error
        // was nil, so the value it guards is clean after it; `if x == nil {
        // return }` leaves only the path where `x` is present, so the
        // narrowing holds for the rest of the block. That guard clause is the
        // shape people actually write — test the bad case, leave, and get on
        // with it — and it falls out of the join: the branch that left is not
        // one of the ways in.
        //
        // Both leaving means nothing follows, but the end of the function
        // still asks what was left unchecked on the way out — on either way.
        let merged = match (then_flow, else_flow) {
            (Flow::Diverges, Flow::Falls) => else_exit,
            (Flow::Falls, Flow::Diverges) => then_exit,
            _ => self.join_flow(&then_exit, &else_exit),
        };
        self.set_flow_state(merged);

        // Without an `else`, control can always fall through.
        let flow = if i.else_.is_none() {
            Flow::Falls
        } else {
            then_flow.merge(else_flow)
        };

        Some((hir::Stmt::If { cond, then, else_, span: i.span }, flow))
    }

    fn for_stmt(&mut self, f: &ast::ForStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        self.loops.push(f.span);

        // A loop body may run zero times, so nothing it assigns can be assumed
        // assigned afterwards, and it may run more than once, so a write in it
        // may already have happened when control returns to its top.
        let entry_init = self.init.clone();
        // Taint needs the same treatment for the same reason: a `check` in a
        // body that never runs proves nothing, so the state after the loop is
        // the entry state joined with whatever the body left. Without this, a
        // `for false { check err }` makes the value it guards readable.
        let entry_taint = self.taint.clone();
        // The body is checked once, but runs again from its own end: a local
        // it assigns may hold, on the second pass, whatever the first pass
        // put there. So nothing proved about such a local before the loop is
        // true inside it — `if x == nil { return }` above a loop that ends
        // with `x = nil` said nothing about the loop's second iteration.
        let mut assigned = Vec::new();
        for s in &f.body.stmts {
            visit_stmts(s, true, &mut |s| {
                if let ast::Stmt::Assign(a) = s {
                    if let ast::Expr::Path(p) = &a.target {
                        if let Some(Res::Local(id)) = self.resolved.lookup_use(p.span) {
                            assigned.push(id);
                        }
                    }
                }
            });
        }
        for id in &assigned {
            self.narrowed.remove(id);
            self.error_nonnil.remove(id);
        }
        let entry_narrowed = self.narrowed.clone();
        let entry_nonnil = self.error_nonnil.clone();

        let (result, runs) = self.loop_parts(f, sig);

        self.loops.pop();
        let merged: Vec<Init> = self
            .init
            .iter()
            .enumerate()
            .map(|(i, now)| entry_init.get(i).map_or(*now, |before| before.merge(*now)))
            .collect();
        self.restore_init(merged);
        // A body that certainly runs, and can only leave through its end or a
        // `return`, hands on exactly the state it ends in: there is no path
        // around it. That is the shape of a bounded walk — `for i in 0..64`
        // testing what it walks on every turn — and without it the path that
        // runs the body zero times, which does not exist, made the walk's
        // subject look unread.
        if !runs {
            self.merge_loop_taint(&entry_taint);
        }
        // What the body proved is not known after it, which may be after zero
        // runs of it; what it disproved was removed before it started.
        self.narrowed = entry_narrowed;
        self.error_nonnil = entry_nonnil;

        let result = result?;
        // `for { … }` with nothing that breaks out of it never falls through.
        // That is worth knowing here rather than leaving to MIR, because a
        // function whose every exit is a `return` inside such a loop is
        // otherwise reported as missing a return — and writing an unreachable
        // one to satisfy the compiler is exactly the sort of dead code a
        // reader has to puzzle over.
        let diverges = matches!(f.header, ast::ForHeader::Loop)
            && !breaks_out(&f.body, f.label.as_ref().map(|l| l.name.as_str()));
        Some((result, if diverges { Flow::Diverges } else { Flow::Falls }))
    }

    /// A loop's header and body, and whether the body certainly runs at least
    /// once and can leave only by finishing or returning.
    fn loop_parts(&mut self, f: &ast::ForStmt, sig: &Signature) -> (Option<hir::Stmt>, bool) {
        let label = f.label.as_ref().map(|l| l.name.clone());
        let mut jumps = false;
        for s in &f.body.stmts {
            visit_stmts(s, false, &mut |s| {
                jumps |= matches!(s, ast::Stmt::Break { .. } | ast::Stmt::Continue { .. })
            });
        }
        match &f.header {
            ast::ForHeader::In { binding, iter } => {
                let ast::Binding::Name(name) = binding else {
                    // `for (k, v) in m` — the only tuple binding a loop takes,
                    // because a map is the only thing that yields pairs.
                    return (self.for_map(f, binding, iter, sig).map(|(s, _)| s), false);
                };
                let ast::Expr::Range { start, end, inclusive, .. } = iter else {
                    // Iterating a slice. The `Iterate` trait generalises this
                    // later; slices are the case that matters now.
                    let seq = self.expr(iter, None);
                    let Some(elem) = self.types.slice_elem(seq.ty) else {
                        if !self.types.is_poisoned(seq.ty) {
                            let found = self.types.with_article(seq.ty);
                            self.diags.push(
                                Diagnostic::error(
                                    codes::E0200,
                                    format!("cannot iterate {}", found),
                                )
                                .with_primary(seq.span, "not iterable")
                                .with_note(
                                    "`for x in …` takes a range or a slice; the `Iterate` \
                                     trait generalises this in a later phase",
                                ),
                            );
                        }
                        return (None, false);
                    };
                    let Some(local_id) = self.resolved.lookup_binding(name.span) else {
                        return (None, false);
                    };
                    self.locals[local_id as usize].ty = elem;
                    self.init[local_id as usize] = Init::Assigned;

                    let (body, _) = self.block(&f.body, sig);
                    let stmt = hir::Stmt::ForSlice {
                        var: hir::LocalId(local_id),
                        slice: seq,
                        body,
                        label,
                        span: f.span,
                    };
                    return (Some(stmt), false);
                };

                let start_e = self.expr(start, Some(TyId::INT));
                let end_e = self.expr(end, Some(TyId::INT));
                self.expect_ty(start_e.ty, TyId::INT, start_e.span, None);
                self.expect_ty(end_e.ty, TyId::INT, end_e.span, None);
                // Two literal ends, the first before the second: the body runs.
                let runs = match (&start_e.kind, &end_e.kind) {
                    (ExprKind::Int(a), ExprKind::Int(b)) => a < b || (*inclusive && a == b),
                    _ => false,
                };

                let Some(local_id) = self.resolved.lookup_binding(name.span) else {
                    return (None, false);
                };
                self.locals[local_id as usize].ty = TyId::INT;
                // The loop itself supplies the counter's value.
                self.init[local_id as usize] = Init::Assigned;

                let (body, _) = self.block(&f.body, sig);
                let stmt = hir::Stmt::ForRange {
                    var: hir::LocalId(local_id),
                    start: start_e,
                    end: end_e,
                    inclusive: *inclusive,
                    body,
                    label,
                    span: f.span,
                };
                (Some(stmt), runs && !jumps)
            }
            ast::ForHeader::While(c) => {
                let cond = self.condition(c);
                let (body, _) = self.block(&f.body, sig);
                (Some(hir::Stmt::While { cond, body, label, span: f.span }), false)
            }
            ast::ForHeader::Loop => {
                let (body, _) = self.block(&f.body, sig);
                (Some(hir::Stmt::Loop { body, label, span: f.span }), false)
            }
        }
    }

    /// `for (key, value) in m`.
    ///
    /// Lowered to a walk over `m.keys()` and `m.values()` side by side, in
    /// insertion order, which the specification guarantees.
    fn for_map(
        &mut self,
        f: &ast::ForStmt,
        binding: &ast::Binding,
        iter: &ast::Expr,
        sig: &Signature,
    ) -> Option<(hir::Stmt, Flow)> {
        let ast::Binding::Tuple { elems, span } = binding else {
            return None;
        };
        let map = self.expr(iter, None);

        // A slice of two-element tuples destructures the same way a map does,
        // and it has to: `zip` and `enumerate` both answer with one, and a
        // pair the language can build but not take apart in the loop that
        // consumes it would be a hole with no reason behind it.
        if let Some(elem) = self.types.slice_elem(map.ty) {
            if let TyKind::Tuple(parts) = self.types.kind(elem).clone() {
                if parts.len() == 2 {
                    return self.for_tuple_slice(f, elems, *span, map, elem, &parts, sig);
                }
            }
        }

        let TyKind::Map(key_ty, value_ty) = *self.types.kind(map.ty) else {
            if !self.types.is_poisoned(map.ty) {
                let found = self.types.with_article(map.ty);
                self.diags.push(
                    Diagnostic::error(codes::E0200, format!("cannot iterate {} in pairs", found))
                        .with_primary(map.span, "not a map")
                        .with_note(
                            "`for (a, b) in …` iterates a map, or a slice whose element is a \
                             two-element tuple; any other slice yields one value",
                        ),
                );
            }
            return None;
        };
        if elems.len() != 2 {
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("expected 2 bindings, found {}", elems.len()),
                )
                .with_primary(*span, "a map yields a key and a value")
                .with_note("write `for (key, value) in m`"),
            );
            return None;
        }

        // The map is read once, into its keys and its values — both in
        // insertion order, so element `i` of one belongs with element `i` of
        // the other — and the loop walks the two side by side. Looking each
        // value up by its key instead was a second search per entry, and it
        // was wrong for a key that is not equal to itself: a NaN key found no
        // entry, and the unwrap of that missing value is what each backend
        // then disagreed about.
        let keys_ty = self.types.slice_of(key_ty);
        let values_ty = self.types.slice_of(value_ty);
        let holder = self.synthetic_local("map", map.ty, *span);
        let keys = self.synthetic_local("keys", keys_ty, *span);
        let values = self.synthetic_local("values", values_ty, *span);
        let index = self.synthetic_local("entry", TyId::INT, *span);
        let local = |id: u32, ty: TyId| hir::Expr {
            kind: ExprKind::Local(hir::LocalId(id)),
            ty,
            span: *span,
        };
        let element = |slice: u32, slice_ty: TyId, ty: TyId| hir::Expr {
            kind: ExprKind::Index {
                base: Box::new(local(slice, slice_ty)),
                index: Box::new(local(index, TyId::INT)),
            },
            ty,
            span: *span,
        };

        let mut body_stmts = Vec::new();
        for (elem, (from, from_ty, ty)) in
            elems.iter().zip([(keys, keys_ty, key_ty), (values, values_ty, value_ty)])
        {
            let ast::BindElem::Name(n) = elem else { continue };
            let Some(bound) = self.resolved.lookup_binding(n.span) else { continue };
            self.locals[bound as usize].ty = ty;
            self.init[bound as usize] = Init::Assigned;
            body_stmts.push(hir::Stmt::Let {
                local: hir::LocalId(bound),
                init: Some(element(from, from_ty, ty)),
                span: n.span,
            });
        }
        let (body, _) = self.block(&f.body, sig);
        body_stmts.extend(body.stmts);

        let whole = |kind| hir::Expr { kind, ty: TyId::INT, span: *span };
        let loop_stmt = hir::Stmt::ForRange {
            var: hir::LocalId(index),
            start: whole(ExprKind::Int(0)),
            end: whole(ExprKind::SliceLen { base: Box::new(local(keys, keys_ty)) }),
            inclusive: false,
            body: hir::Block { stmts: body_stmts },
            label: f.label.as_ref().map(|l| l.name.clone()),
            span: f.span,
        };
        let read = |kind, ty| hir::Expr { kind, ty, span: *span };
        Some((
            hir::Stmt::Block(hir::Block {
                stmts: vec![
                    hir::Stmt::Let { local: hir::LocalId(holder), init: Some(map), span: *span },
                    hir::Stmt::Let {
                        local: hir::LocalId(keys),
                        init: Some(read(
                            ExprKind::MapKeys { base: Box::new(local(holder, self.locals[holder as usize].ty)) },
                            keys_ty,
                        )),
                        span: *span,
                    },
                    hir::Stmt::Let {
                        local: hir::LocalId(values),
                        init: Some(read(
                            ExprKind::MapValues { base: Box::new(local(holder, self.locals[holder as usize].ty)) },
                            values_ty,
                        )),
                        span: *span,
                    },
                    loop_stmt,
                ],
            }),
            Flow::Falls,
        ))
    }

    /// `for (a, b) in xs`, where `xs` is a slice of two-element tuples.
    ///
    /// Lowered to an ordinary slice loop over a synthetic local holding the
    /// tuple, with the two names read out of it by position — the same shape
    /// `let (a, b) = pair` produces, so nothing new reaches a backend.
    #[allow(clippy::too_many_arguments)]
    fn for_tuple_slice(
        &mut self,
        f: &ast::ForStmt,
        elems: &[ast::BindElem],
        span: Span,
        seq: hir::Expr,
        elem_ty: TyId,
        parts: &[TyId],
        sig: &Signature,
    ) -> Option<(hir::Stmt, Flow)> {
        if elems.len() != 2 {
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("expected 2 bindings, found {}", elems.len()),
                )
                .with_primary(span, "this slice yields a pair")
                .with_note("write `for (a, b) in xs`"),
            );
            return None;
        }

        let holder = self.synthetic_local("pair", elem_ty, span);
        self.locals[holder as usize].ty = elem_ty;
        self.init[holder as usize] = Init::Assigned;

        let mut body_stmts = Vec::new();
        for (i, e) in elems.iter().enumerate() {
            let ast::BindElem::Name(name) = e else { continue };
            let Some(local) = self.resolved.lookup_binding(name.span) else { continue };
            self.locals[local as usize].ty = parts[i];
            self.init[local as usize] = Init::Assigned;
            self.taint[local as usize] = Taint::Clean;
            body_stmts.push(hir::Stmt::Let {
                local: hir::LocalId(local),
                init: Some(hir::Expr {
                    kind: ExprKind::FieldGet {
                        base: Box::new(hir::Expr {
                            kind: ExprKind::Local(hir::LocalId(holder)),
                            ty: elem_ty,
                            span,
                        }),
                        index: i as u32,
                    },
                    ty: parts[i],
                    span: name.span,
                }),
                span: name.span,
            });
        }

        let (body, _) = self.block(&f.body, sig);
        body_stmts.extend(body.stmts);

        Some((
            hir::Stmt::ForSlice {
                var: hir::LocalId(holder),
                slice: seq,
                body: hir::Block { stmts: body_stmts },
                label: f.label.as_ref().map(|l| l.name.clone()),
                span: f.span,
            },
            Flow::Falls,
        ))
    }

    /// Put back a saved definite-assignment state.
    ///
    /// A snapshot is taken before a branch or a loop body and put back after,
    /// because neither is guaranteed to run. Locals created *while* checking
    /// the body — the temporaries a desugaring needs — are not in the snapshot
    /// and must not vanish with it, so the state is padded back out to the
    /// table it indexes.
    fn restore_init(&mut self, saved: Vec<Init>) {
        self.init = saved;
        self.init.resize(self.locals.len(), Init::Assigned);
    }

    /// The same for taint. A temporary the checker made holds no correlated
    /// result, so it is clean.
    fn restore_taint(&mut self, saved: Vec<Taint>) {
        self.taint = saved;
        self.taint.resize(self.locals.len(), Taint::Clean);
    }

    /// Leave a loop: join what the body left with the state it was entered in,
    /// because control also reaches here having run the body zero times.
    fn merge_loop_taint(&mut self, entry: &[Taint]) {
        let merged = self.join_taint(entry, &self.taint);
        self.restore_taint(merged);
    }

    /// Join two taint states at a point where control arrives from either.
    ///
    /// Sides can be different lengths, because a branch declares locals the
    /// other never saw; a local the incoming path had not reached yet is Clean
    /// there, which is the identity the merge rule wants.
    fn join_taint(&self, a: &[Taint], b: &[Taint]) -> Vec<Taint> {
        let n = a.len().max(b.len()).max(self.locals.len());
        (0..n)
            .map(|i| {
                let x = a.get(i).copied().unwrap_or(Taint::Clean);
                let y = b.get(i).copied().unwrap_or(Taint::Clean);
                x.merge(y)
            })
            .collect()
    }

    /// A local compared against `nil`, and whether the comparison was `==`.
    ///
    /// Returns `None` unless the local's type is optional, so an `error` test
    /// (handled separately by the taint analysis) does not narrow anything.
    fn nil_test(&self, cond: &ast::Expr) -> Option<(u32, bool)> {
        let ast::Expr::Binary { op, lhs, rhs, .. } = cond else {
            return None;
        };
        let is_eq = match op {
            ast::BinaryOp::Eq => true,
            ast::BinaryOp::Ne => false,
            _ => return None,
        };
        let path = match (lhs.as_ref(), rhs.as_ref()) {
            (ast::Expr::Path(p), ast::Expr::Nil(_)) => p,
            (ast::Expr::Nil(_), ast::Expr::Path(p)) => p,
            _ => return None,
        };
        let Some(Res::Local(id)) = self.resolved.lookup_use(path.span) else {
            return None;
        };
        matches!(self.types.kind(self.locals[id as usize].ty), TyKind::Optional(_))
            .then_some((id, is_eq))
    }

    /// Start a branch of a test: narrow what the test proved present on this
    /// side of it, and clean the value an error guards where the error is
    /// known nil.
    ///
    /// `x == nil` narrows in the *else*, `x != nil` in the *then*; `err !=
    /// nil` proves the error present in the *then*, `err == nil` in the
    /// *else*. The branch's state is thrown away or joined at the end, so
    /// nothing here has to be undone.
    fn enter_branch(
        &mut self,
        narrowing: Option<(u32, bool)>,
        tested: Option<(u32, bool)>,
        in_then: bool,
    ) {
        if let Some((id, is_eq)) = narrowing {
            if is_eq != in_then {
                if let TyKind::Optional(inner) = *self.types.kind(self.locals[id as usize].ty) {
                    self.narrowed.insert(id, inner);
                }
            }
        }
        if let Some((id, is_eq)) = tested {
            if is_eq != in_then {
                self.error_nonnil.insert(id);
            }
        }
        let mut taint = std::mem::take(&mut self.taint);
        self.clean_guarded(&mut taint, tested, in_then);
        self.taint = taint;
    }

    /// Start checking something only to learn its type.
    ///
    /// Inference sometimes checks an expression twice: once to see what type
    /// it has, and again for real against what that settled. Everything the
    /// first check does is thrown away, not only its diagnostics — a read
    /// that reports a tainted value marks it clean, so that one mistake is one
    /// diagnostic, and with the report discarded that mark let the value
    /// through unreported on the real check. So a trial works on the checker
    /// as it is, and [`Self::end_trial`] puts back everything it changed.
    fn begin_trial(&mut self) -> Trial {
        Trial {
            diags: std::mem::replace(self.diags, DiagBag::new()),
            flow: self.flow_state(),
            guards: self.guards.clone(),
            captures: self.captures.clone(),
            reported: self.reported_unchecked.clone(),
            locals: self.locals.len(),
            lifted: self.lifted.len(),
            solved: [
                self.solved.bindings.len(),
                self.solved.calls.len(),
                self.solved.locals.len(),
                self.solved.methods.len(),
            ],
        }
    }

    fn end_trial(&mut self, t: Trial) {
        *self.diags = t.diags;
        // A temporary the trial made is referred to only by what the trial
        // built, which is being thrown away; so is a closure it lifted, whose
        // number the real check hands out again.
        self.locals.truncate(t.locals);
        self.lifted.truncate(t.lifted);
        self.set_flow_state(t.flow);
        self.guards = t.guards;
        self.captures = t.captures;
        self.reported_unchecked = t.reported;
        self.solved.bindings.truncate(t.solved[0]);
        self.solved.calls.truncate(t.solved[1]);
        self.solved.locals.truncate(t.solved[2]);
        self.solved.methods.truncate(t.solved[3]);
    }

    /// Everything the flow analysis knows at this point.
    fn flow_state(&self) -> FlowState {
        FlowState {
            init: self.init.clone(),
            taint: self.taint.clone(),
            narrowed: self.narrowed.clone(),
            error_nonnil: self.error_nonnil.clone(),
        }
    }

    fn set_flow_state(&mut self, state: FlowState) {
        self.restore_init(state.init);
        self.restore_taint(state.taint);
        self.narrowed = state.narrowed;
        self.error_nonnil = state.error_nonnil;
    }

    /// The state at a point control reaches from either of two others: what
    /// both agree on, and nothing either alone proved.
    fn join_flow(&self, a: &FlowState, b: &FlowState) -> FlowState {
        let n = a.init.len().max(b.init.len());
        let init = (0..n)
            .map(|i| match (a.init.get(i), b.init.get(i)) {
                (Some(x), Some(y)) => x.merge(*y),
                (Some(x), None) | (None, Some(x)) => *x,
                (None, None) => Init::Assigned,
            })
            .collect();
        FlowState {
            init,
            taint: self.join_taint(&a.taint, &b.taint),
            narrowed: a
                .narrowed
                .iter()
                .filter(|(id, ty)| b.narrowed.get(id) == Some(ty))
                .map(|(id, ty)| (*id, *ty))
                .collect(),
            error_nonnil: a.error_nonnil.intersection(&b.error_nonnil).copied().collect(),
        }
    }

    /// The error local a condition inspects, and whether the test was `==`.
    ///
    /// `if err != nil { … } else { … }` proves the error is nil in the *else*;
    /// `if err == nil` proves it in the *then*. Cleaning the guarded value on
    /// exactly that branch is what lets a hand-written test do the same work as
    /// `check`.
    fn error_tested_by(&self, cond: &ast::Expr) -> Option<(u32, bool)> {
        let ast::Expr::Binary { op, lhs, rhs, .. } = cond else {
            return None;
        };
        let is_eq = match op {
            ast::BinaryOp::Eq => true,
            ast::BinaryOp::Ne => false,
            _ => return None,
        };
        let path = match (lhs.as_ref(), rhs.as_ref()) {
            (ast::Expr::Path(p), ast::Expr::Nil(_)) => p,
            (ast::Expr::Nil(_), ast::Expr::Path(p)) => p,
            _ => return None,
        };
        match self.resolved.lookup_use(path.span) {
            Some(Res::Local(id)) if self.locals[id as usize].ty == TyId::ERR => {
                Some((id, is_eq))
            }
            _ => None,
        }
    }

    /// Clean the value an error guards, on the branch where the error is nil.
    fn clean_guarded(&self, taint: &mut [Taint], tested: Option<(u32, bool)>, in_then: bool) {
        let Some((id, is_eq)) = tested else { return };
        // `err == nil` proves it in the then-branch; `err != nil` in the else.
        if is_eq != in_then {
            return;
        }
        if let Some(&guarded) = self.guards.get(&id) {
            taint[guarded as usize] = Taint::Clean;
        }
    }

    /// A condition must be exactly `bool`. Kite has no truthiness.
    fn condition(&mut self, e: &ast::Expr) -> hir::Expr {
        let c = self.expr(e, Some(TyId::BOOL));
        if !self.types.satisfies(c.ty, TyId::BOOL) && !self.types.is_poisoned(c.ty) {
            let mut d = Diagnostic::error(codes::E0202, "condition must be `bool`")
                .with_primary(c.span, format!("this is {}", self.types.with_article(c.ty)))
                .with_note("Kite has no truthiness: compare explicitly");
            if c.ty == TyId::INT {
                d = d.with_note("for example, write `n != 0`");
            }
            self.diags.push(d);
        }
        c
    }

    // ---- expressions ------------------------------------------------------

    /// `expected` is a hint, not a constraint — it steers literal typing and
    /// improves messages. The caller still checks the result.
    fn expr(&mut self, e: &ast::Expr, expected: Option<TyId>) -> hir::Expr {
        match e {
            ast::Expr::Int(span) => {
                let text = self.text(*span);
                match parse_int(text) {
                    Some(v) => self.lit(ExprKind::Int(v), TyId::INT, *span),
                    None => {
                        self.diags.push(
                            Diagnostic::error(codes::E0004, "integer literal is out of range")
                                .with_primary(*span, "does not fit in `int`")
                                .with_note("`int` is 64-bit signed"),
                        );
                        self.lit(ExprKind::Error, TyId::ERROR, *span)
                    }
                }
            }
            ast::Expr::ImpliedInt { value, span } => self.lit(ExprKind::Int(*value), TyId::INT, *span),
            ast::Expr::Float(span) => {
                let text = self.text(*span);
                match parse_float(text) {
                    Some(v) => self.lit(ExprKind::Float(v), TyId::FLOAT, *span),
                    None => {
                        self.diags.push(
                            Diagnostic::error(codes::E0004, "invalid float literal")
                                .with_primary(*span, "cannot be parsed"),
                        );
                        self.lit(ExprKind::Error, TyId::ERROR, *span)
                    }
                }
            }
            ast::Expr::Str(span) => {
                let value = self.string_value(*span);
                self.lit(ExprKind::Str(value), TyId::STR, *span)
            }
            ast::Expr::Bool { value, span } => self.lit(ExprKind::Bool(*value), TyId::BOOL, *span),
            ast::Expr::Interpolated { parts, span } => self.interpolated(parts, *span),

            // A unit variant of a generic enum says nothing about its
            // arguments, so the type the context expects is what settles them:
            // `let m: Maybe<str> = None`.
            ast::Expr::Path(p) => match self.resolved.lookup_use(p.span) {
                Some(Res::Variant(ti, vi)) => {
                    self.variant_value(ti, vi, &[], &[], p.span, p.span, expected)
                }
                _ => self.path_expr(p),
            },
            ast::Expr::Paren { inner, .. } => self.expr(inner, expected),

            ast::Expr::Unary { op, operand, span } => {
                // `-9223372036854775808` is `int`'s minimum, but the literal
                // after the `-` is one past its maximum and would be refused
                // on its own. Written together they are one constant.
                if let (ast::UnaryOp::Neg, ast::Expr::Int(digits)) = (op, operand.as_ref()) {
                    if is_int_min_magnitude(self.text(*digits)) {
                        return self.lit(ExprKind::Int(i64::MIN), TyId::INT, *span);
                    }
                }
                let val = self.expr(operand, expected);
                self.unary(*op, val, *span)
            }

            // `x == nil` is a nil test, not a structural comparison. Saying so
            // here keeps it one instruction and keeps `nil` out of the
            // equality rules entirely.
            ast::Expr::Binary { op, lhs, rhs, span }
                if matches!(op, ast::BinaryOp::Eq | ast::BinaryOp::Ne)
                    && (matches!(lhs.as_ref(), ast::Expr::Nil(_))
                        || matches!(rhs.as_ref(), ast::Expr::Nil(_))) =>
            {
                let subject = if matches!(lhs.as_ref(), ast::Expr::Nil(_)) { rhs } else { lhs };
                let value = self.expr(subject, None);
                if !matches!(self.types.kind(value.ty), TyKind::Optional(_))
                    && value.ty != TyId::ERR
                    && !self.types.is_poisoned(value.ty)
                {
                    let found = self.types.with_article(value.ty);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0201,
                            format!("{} can never be nil", found),
                        )
                        .with_primary(value.span, "this comparison is always the same")
                        .with_note("only an optional or an `error` is ever nil"),
                    );
                }
                let test = hir::Expr {
                    kind: ExprKind::IsNil { value: Box::new(value) },
                    ty: TyId::BOOL,
                    span: *span,
                };
                if *op == ast::BinaryOp::Eq {
                    test
                } else {
                    hir::Expr {
                        kind: ExprKind::Unary { op: hir::UnOp::Not, operand: Box::new(test) },
                        ty: TyId::BOOL,
                        span: *span,
                    }
                }
            }

            ast::Expr::Binary { op, lhs, rhs, span } => {
                if let Some(hop) = short_circuit(*op) {
                    let l = self.expr(lhs, Some(TyId::BOOL));
                    let r = self.expr(rhs, Some(TyId::BOOL));
                    for side in [&l, &r] {
                        if !self.types.satisfies(side.ty, TyId::BOOL) && !self.types.is_poisoned(side.ty) {
                            self.diags.push(
                                Diagnostic::error(
                                    codes::E0201,
                                    format!("`{}` needs `bool` operands", op.text()),
                                )
                                .with_primary(side.span, format!("this is {}", self.types.with_article(side.ty)))
                                .with_note("Kite has no truthiness"),
                            );
                        }
                    }
                    return hir::Expr {
                        kind: ExprKind::Binary { op: hop, lhs: Box::new(l), rhs: Box::new(r) },
                        ty: TyId::BOOL,
                        span: *span,
                    };
                }

                // Steer literal typing on one side by the other, so
                // `x + 1` works when `x` is a float.
                let l = self.expr(lhs, None);
                let hint = if self.types.is_poisoned(l.ty) { expected } else { Some(l.ty) };
                let r = self.expr(rhs, hint);
                self.binary(*op, l, r, *span)
            }

            ast::Expr::Call { callee, args, arg_names, span } => {
                self.call(callee, args, arg_names, *span, expected)
            }

            ast::Expr::If { cond, then, else_, span } => {
                self.if_expr(cond, then, else_, *span, expected)
            }

            ast::Expr::Range { span, .. } => {
                self.diags.push(
                    Diagnostic::error(codes::E0200, "a range is not a value here")
                        .with_primary(*span, "a range cannot be held")
                        .with_note(
                            "`a..b` is syntax rather than a type: it says how to walk \
                             a `for` header and what window to take out of a slice or a \
                             `str`. There is no `Range` to bind, pass or return",
                        )
                        .with_note("to carry one, pass the two ends"),
                );
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }

            ast::Expr::Char(span) => {
                self.diags.push(
                    Diagnostic::error(codes::E0200, "there is no `char` type")
                        .with_primary(*span, "not a type in this language")
                        .with_note(
                            "§3.1 has one integer type and no `char`: a character is \
                             an `int` code point, which `s.code_at(i)` answers with",
                        ),
                );
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            // `nil` has no type of its own; it takes one from context. Kite
            // has no null, so the only place it fits is a `?T`.
            ast::Expr::Nil(span) => match expected {
                // `nil` is the no-error value, which is why `return v, nil`
                // reads the way it does.
                Some(TyId::ERR) => hir::Expr { kind: ExprKind::Nil, ty: TyId::ERR, span: *span },
                Some(want) if matches!(self.types.kind(want), TyKind::Optional(_)) => {
                    hir::Expr { kind: ExprKind::Nil, ty: want, span: *span }
                }
                Some(want) if !self.types.is_poisoned(want) => {
                    let name = self.types.name(want);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("expected `{}`, found `nil`", name),
                        )
                        .with_primary(*span, "`nil` is only a value of an optional type")
                        .with_note(format!(
                            "Kite has no null: write `Option<{}>` if this may be absent",
                            name
                        )),
                    );
                    self.lit(ExprKind::Error, TyId::ERROR, *span)
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error(codes::E0204, "cannot infer a type for `nil`")
                            .with_primary(*span, "no expected type here")
                            .with_note("annotate the binding, as in `let x: Option<int> = nil`"),
                    );
                    self.lit(ExprKind::Error, TyId::ERROR, *span)
                }
            },
            // A method's receiver is local 0, which is what makes a method
            // call and a plain call the same thing after checking.
            ast::Expr::SelfExpr(span) => match self.sigs[self.fn_index].self_ty {
                Some(ty) => {
                    // Inside a closure the receiver is a capture like any
                    // other. Not noting it left the lifted body reading its
                    // own first slot, which holds whatever it captured first.
                    self.note_capture(0, *span);
                    hir::Expr { kind: ExprKind::Local(hir::LocalId(0)), ty, span: *span }
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(codes::E0111, "`self` outside a method")
                            .with_primary(*span, "no receiver here")
                            .with_note(
                                "only a method declared with `self` as its first parameter has \
                                 a receiver",
                            ),
                    );
                    self.lit(ExprKind::Error, TyId::ERROR, *span)
                }
            },
            ast::Expr::Field { base, name, span } => {
                // A dotted static path in value position is not a field read.
                match self.resolved.lookup_use(*span) {
                    Some(Res::Fn(id)) => self.fn_value(id, *span),
                    Some(Res::Const(index)) => self.const_expr(index, *span),
                    Some(Res::Builtin(_)) => {
                        self.builtin_not_a_value(*span);
                        self.lit(ExprKind::Error, TyId::ERROR, *span)
                    }
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_value(ti, vi, &[], &[], *span, *span, expected)
                    }
                    Some(Res::Type(ti)) => self.type_member_value(ti, base, name, *span),
                    _ => self.field_access(base, name, *span),
                }
            }

            ast::Expr::StructLit(lit) => self.struct_literal_with(lit, expected),

            ast::Expr::Match(m) => self.match_expr(m, expected),

            ast::Expr::Map { entries, span } => {
                let hint = expected.and_then(|e| match self.types.kind(e) {
                    TyKind::Map(k, v) => Some((*k, *v)),
                    _ => None,
                });
                if entries.is_empty() {
                    let Some((k, v)) = hint else {
                        self.diags.push(
                            Diagnostic::error(codes::E0204, "cannot infer the map's types")
                                .with_primary(*span, "an empty map has no entries to infer from")
                                .with_note("write the type, as in `let m: {str: int} = {}`"),
                        );
                        return self.lit(ExprKind::Error, TyId::ERROR, *span);
                    };
                    let ty = self.types.map_of(k, v);
                    return hir::Expr {
                        kind: ExprKind::MapNew { entries: Vec::new() },
                        ty,
                        span: *span,
                    };
                }

                let mut flat = Vec::with_capacity(entries.len() * 2);
                let mut key_ty = hint.map(|(k, _)| k);
                let mut val_ty = hint.map(|(_, v)| v);
                for e in entries {
                    let k = self.expr(&e.key, key_ty);
                    let k = match key_ty {
                        None => {
                            if !self.types.is_poisoned(k.ty) {
                                key_ty = Some(k.ty);
                            }
                            k
                        }
                        Some(want) => self.accept(k, want, None),
                    };
                    let v = self.expr(&e.value, val_ty);
                    let v = match val_ty {
                        None => {
                            if !self.types.is_poisoned(v.ty) {
                                val_ty = Some(v.ty);
                            }
                            v
                        }
                        Some(want) => self.accept(v, want, None),
                    };
                    flat.push(k);
                    flat.push(v);
                }

                let k = key_ty.unwrap_or(TyId::ERROR);
                let v = val_ty.unwrap_or(TyId::ERROR);
                if check_map_key(self.types, k, *span, self.diags) {
                    self.note_compared(k, *span);
                }
                let ty = self.types.map_of(k, v);
                hir::Expr { kind: ExprKind::MapNew { entries: flat }, ty, span: *span }
            }
            ast::Expr::Index { base, index, span } => self.index_expr(base, index, *span),
            ast::Expr::Cast { expr, ty, span } => self.cast(expr, ty, *span),
            ast::Expr::Await { expr, span } => self.await_expr(expr, *span),
            ast::Expr::Tuple { elems, span } => {
                if elems.is_empty() {
                    return self.lit(ExprKind::Error, TyId::UNIT, *span);
                }
                // The expected type steers each element, which is what lets a
                // tuple literal supply a `(int, str)` without annotation.
                let hint: Option<Vec<TyId>> = expected.and_then(|e| {
                    match self.types.kind(e) {
                        TyKind::Tuple(ts) if ts.len() == elems.len() => Some(ts.clone()),
                        _ => None,
                    }
                });
                let mut out = Vec::with_capacity(elems.len());
                let mut tys = Vec::with_capacity(elems.len());
                for (i, e) in elems.iter().enumerate() {
                    let want = hint.as_ref().map(|h| h[i]);
                    let v = self.expr(e, want);
                    let v = self.coerce(v, want);
                    if let Some(w) = want {
                        self.expect_ty(v.ty, w, v.span, None);
                    }
                    tys.push(v.ty);
                    out.push(v);
                }
                let ty = self.types.tuple_of(tys);
                hir::Expr { kind: ExprKind::TupleNew { elems: out }, ty, span: *span }
            }
            ast::Expr::Slice { elems, span } => self.slice_literal(elems, expected, *span),
            ast::Expr::Closure { params, ret, body, span } => {
                self.closure(params, ret.as_deref(), body, expected, *span)
            }
            ast::Expr::Error(span) => self.lit(ExprKind::Error, TyId::ERROR, *span),
        }
    }

    /// A closure literal.
    ///
    /// The body is lifted into a function of its own, whose leading parameters
    /// are the values it captured. Lifting here rather than in a later pass
    /// means MIR, both backends and monomorphisation see only ordinary
    /// functions: nothing downstream has to know closures exist.
    ///
    /// Captures are by value, taken when the closure is made. A closure that
    /// could see later writes to a `var` would be action at a distance, which
    /// is exactly what the rest of the language refuses — so capturing a `var`
    /// is rejected rather than quietly given one meaning or the other.
    fn closure(
        &mut self,
        params: &[ast::ClosureParam],
        ret_ty: Option<&ast::Type>,
        body: &ast::ClosureBody,
        expected: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        // What the context wants, when it wants anything. An unannotated
        // parameter is only knowable from here.
        let wanted: Option<(Vec<TyId>, TyId)> = expected.and_then(|e| {
            match self.types.kind(e) {
                TyKind::Fn { params, ret } => Some((params.clone(), *ret)),
                _ => None,
            }
        });

        let mut param_ids = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let annotated = p.ty.as_ref().map(|t| self.resolve_type(t));
            let from_context = wanted.as_ref().and_then(|(ps, _)| ps.get(i).copied());
            let ty = match (annotated, from_context) {
                (Some(a), Some(c)) => {
                    self.expect_ty(a, c, p.name.span, None);
                    a
                }
                (Some(a), None) => a,
                (None, Some(c)) => c,
                (None, None) => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0211,
                            format!("cannot infer the type of `{}`", p.name.name),
                        )
                        .with_primary(p.name.span, "this parameter has no type")
                        .with_note(
                            "a closure's parameter types come from the place it is used. \
                             Where that is not known, annotate them: `|x: int| ...`",
                        ),
                    );
                    TyId::ERROR
                }
            };
            let Some(id) = self.resolved.lookup_binding(p.name.span) else {
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            };
            self.set_local_ty(id, ty);
            param_ids.push(id);
        }

        // Check the body. Captures accumulate as outer locals are read.
        // A closure inside a closure sees the inner boundary, so its own
        // captures are the ones it reads from anywhere further out.
        let outer_captures = std::mem::take(&mut self.captures);
        let outer_region = self.closure_span.replace(span);
        // An annotation wins over the context, and is checked against it.
        let annotated_ret = ret_ty.map(|t| self.resolve_type(t));
        let expected_ret = match (annotated_ret, wanted.as_ref().map(|(_, r)| *r)) {
            (Some(a), Some(c)) => {
                self.expect_ty(a, c, ret_ty.map(|t| t.span()).unwrap_or(span), None);
                Some(a)
            }
            (Some(a), None) => Some(a),
            (None, c) => c,
        };
        // `-> (T, error)` on a closure is the fallible form, exactly as it is
        // on a named function, and is kept as one: without this the closure
        // returned a plain tuple, `return v, nil` was refused as a pair from
        // a function that is not fallible, and `let (v, err) = f(x)` could
        // not take its result apart.
        let expected_ret = expected_ret.map(|r| fallible_form(r, self.types));
        let sig = Signature {
            params: Vec::new(),
            // A block body's `return` statements are checked against the
            // signature the context asked for; with no context there is
            // nothing to check them against, so the closure returns unit.
            ret: expected_ret.unwrap_or(match body {
                ast::ClosureBody::Expr(_) => TyId::ERROR,
                ast::ClosureBody::Block(_) => TyId::UNIT,
            }),
            // A closure cannot be `async`: it has no signature of its own to
            // declare it on, and a suspension point inside one would suspend a
            // function that never said it could.
            is_async: false,
            fallible: expected_ret.is_some_and(|r| self.types.fallible_value(r).is_some()),
            name_span: span,
            self_ty: None,
            generics: Vec::new(),
        };

        // The body is a function of its own, and is checked as one. It starts
        // from what is true where the closure is made — its captures are
        // copies taken there, so a narrowing or a checked error there holds
        // for them — but nothing it proves or does reaches back out. It has
        // no loop around it to leave, its own `defer`s run at its own exits,
        // and its `return` answers to its own signature. Sharing any of that
        // with the enclosing function let `if err != nil { return }` inside a
        // closure clean a value outside it, a `return` inside one run the
        // enclosing function's deferred calls, and a `break` inside one leave
        // a loop that had long finished.
        let outer = Enclosing {
            init: self.init.clone(),
            taint: self.taint.clone(),
            narrowed: self.narrowed.clone(),
            error_nonnil: self.error_nonnil.clone(),
            defers: self.defers.take(),
            loops: std::mem::take(&mut self.loops),
            sig: self.closure_sig.replace(sig.clone()),
            ret_unknown: std::mem::replace(
                &mut self.closure_ret_unknown,
                match body {
                    ast::ClosureBody::Expr(_) if expected_ret.is_none() => Some(span),
                    _ => None,
                },
            ),
        };

        let (hir_body, ret) = match body {
            ast::ClosureBody::Expr(e) => {
                self.defers = self.defer_stack_for(expr_defers(e), e.span());
                let v = self.expr(e, expected_ret);
                let v = self.coerce(v, expected_ret);
                let ty = v.ty;
                let ret = self.returning(hir::Stmt::Return { value: Some(v), span }, e.span());
                let block = self.with_defers(hir::Block { stmts: vec![ret] }, Flow::Diverges, e.span());
                (block, ty)
            }
            ast::ClosureBody::Block(b) => {
                let ret = sig.ret;
                self.defers = self.defer_stack_for(b.stmts.iter().any(stmt_defers), b.span);
                let (block, flow) = self.block(b, &sig);
                let block = self.with_defers(block, flow, b.span);
                // Falling off the end of a closure that promised a value is
                // the same mistake as in a named function.
                if ret != TyId::UNIT && flow != Flow::Diverges {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0211,
                            "this closure can finish without returning a value",
                        )
                        .with_primary(span, format!("it must return `{}`", self.types.name(ret)))
                        .with_note("every path out of the body needs a `return`"),
                    );
                }
                (block, ret)
            }
        };
        // What the body declared goes out of scope here, so an error in it
        // nobody looked at is reported here: the state that knew about it is
        // about to be put back.
        self.report_unchecked_errors(Some(span));
        let captures = std::mem::replace(&mut self.captures, outer_captures);
        self.closure_span = outer_region;
        self.restore_init(outer.init);
        // Reading an error is inspecting it, and a closure that reads one has
        // been handed it. The closure may run later or not at all, but that
        // is the same as passing it to a function: somebody is responsible.
        let mut taint = outer.taint;
        for id in &captures {
            if taint.get(*id as usize) == Some(&Taint::Unchecked) {
                taint[*id as usize] = Taint::Clean;
            }
        }
        self.restore_taint(taint);
        self.narrowed = outer.narrowed;
        self.error_nonnil = outer.error_nonnil;
        self.defers = outer.defers;
        self.loops = outer.loops;
        self.closure_sig = outer.sig;
        self.closure_ret_unknown = outer.ret_unknown;
        // A capture of a capture: what an inner closure took from outside the
        // enclosing one is something the enclosing one must take too, so that
        // it has a value to hand on.
        if let Some(outer) = outer_region {
            for id in &captures {
                if !self.declared_inside(*id, outer) && !self.captures.contains(id) {
                    self.captures.push(*id);
                }
            }
        }

        let ty = self.types.fn_of(
            param_ids.iter().map(|id| self.locals[*id as usize].ty).collect(),
            ret,
        );
        let func = self.lift(&captures, &param_ids, hir_body, ret, span);
        let capture_exprs = captures
            .iter()
            .map(|id| hir::Expr {
                kind: ExprKind::Local(hir::LocalId(*id)),
                ty: self.locals[*id as usize].ty,
                span,
            })
            .collect();
        // A closure inside a generic function is specialised with it.
        let targs = self.generic_defs.iter().map(|g| g.ty).collect();
        hir::Expr {
            kind: ExprKind::ClosureNew { func, captures: capture_exprs, targs },
            ty,
            span,
        }
    }

    /// Move a closure body into a function of its own.
    ///
    /// The new function's locals are the captures, then the parameters, then
    /// everything the body declared — so a lifted body's local numbering is
    /// remapped exactly once, here.
    fn lift(
        &mut self,
        captures: &[u32],
        params: &[u32],
        mut body: hir::Block,
        ret: TyId,
        span: Span,
    ) -> hir::FnId {
        let mut map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut locals: Vec<hir::Local> = Vec::new();
        for id in captures.iter().chain(params.iter()) {
            map.insert(*id, locals.len() as u32);
            locals.push(self.locals[*id as usize].clone());
        }
        // Everything the body itself introduced, which is every local whose
        // declaration lies inside the closure. A nested closure's locals are
        // in there too; they are dead here, having moved with its body, and
        // cost an unused slot rather than a wrong one.
        for id in 0..self.locals.len() as u32 {
            if map.contains_key(&id) || !self.declared_inside(id, span) {
                continue;
            }
            map.insert(id, locals.len() as u32);
            locals.push(self.locals[id as usize].clone());
        }
        kite_hir::mono::renumber_locals(&mut body, &map);

        let index = self.lifted.len();
        self.lifted.push(hir::Function {
            // A lifted body has a generated name and no caller outside the
            // closure that made it.
            is_free: false,
            // Named per enclosing function, so two closures in different
            // functions do not both come out as `closure#0` in a dump.
            name: format!("{}#closure{}", self.resolved.fns[self.fn_index].name, index),
            // A closure written inside `f<T>` mentions `T`, so its lifted body
            // is a template exactly as `f` is.
            generic_count: self.generic_defs.len(),
            is_pub: false,
            is_async: false,
            param_count: captures.len() + params.len(),
            locals,
            ret,
            body,
            span,
        });
        // Lifted functions are appended after every declared one, so their ids
        // start where the declared ones end — offset by everything lifted out
        // of the declarations checked before this one.
        hir::FnId(self.resolved.fns.len() as u32 + (self.lifted_base + index) as u32)
    }

    fn set_local_ty(&mut self, id: u32, ty: TyId) {
        if let Some(l) = self.locals.get_mut(id as usize) {
            l.ty = ty;
        }
        while self.init.len() <= id as usize {
            self.init.push(Init::Assigned);
        }
        while self.taint.len() <= id as usize {
            self.taint.push(Taint::Clean);
        }
        self.init[id as usize] = Init::Assigned;
        self.taint[id as usize] = Taint::Clean;
    }

    /// Whether a local's declaration lies inside a source region.
    fn declared_inside(&self, id: u32, region: Span) -> bool {
        let s = self.locals[id as usize].span;
        s.start >= region.start && s.end <= region.end
    }

    /// Record a read of an enclosing function's local as a capture.
    fn note_capture(&mut self, id: u32, span: Span) {
        let Some(region) = self.closure_span else { return };
        if self.declared_inside(id, region) || self.captures.contains(&id) {
            return;
        }
        // `var self` is the one mutable binding a capture cannot go stale on:
        // nothing can assign to `self`, only to its fields, and a struct is a
        // reference — so the copy and the original are one value.
        let receiver = id == 0 && self.locals[0].name == "self";
        if self.locals[id as usize].mutable && !receiver {
            let name = self.locals[id as usize].name.clone();
            let decl = self.locals[id as usize].span;
            self.diags.push(
                Diagnostic::error(
                    codes::E0211,
                    format!("a closure cannot capture `{}`, which is a `var`", name),
                )
                .with_primary(span, "captured here")
                .with_secondary(decl, "declared with `var`")
                .with_note(
                    "captures are by value, taken when the closure is made — so later \
                     writes would not be seen, and reading one as if they were is a bug \
                     waiting to happen",
                )
                .with_note("copy it into a `let` first, or pass it as a parameter"),
            );
        }
        // Recorded even when refused, so a closure that uses the `var` twice
        // is told once.
        self.captures.push(id);
    }

    /// `x as float`.
    ///
    /// Only between `int` and `float`, and only when written. Kite performs no
    /// implicit numeric conversion — an `int` reaching a `float` context is an
    /// error rather than a widening — because a silent conversion is how
    /// precision goes missing without anyone having decided it should.
    fn cast(&mut self, expr: &ast::Expr, ty: &ast::Type, span: Span) -> hir::Expr {
        let value = self.expr(expr, None);
        let to = self.resolve_type(ty);
        if self.types.is_poisoned(value.ty) || self.types.is_poisoned(to) {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        if !self.types.is_numeric(value.ty) || !self.types.is_numeric(to) {
            let (a, b) = (self.types.name(value.ty), self.types.name(to));
            self.diags.push(
                Diagnostic::error(codes::E0212, format!("cannot cast `{}` to `{}`", a, b))
                    .with_primary(
                        span,
                        "`as` converts between `int` and `float`, and nothing else",
                    )
                    .with_note(
                        "there is no conversion between other types: a `str` is not a \
                         number and a number is not a `str`",
                    ),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        if value.ty == TyId::FLOAT && to == TyId::INT {
            self.diags.push(
                Diagnostic::warning(codes::E0212, "this cast discards the fractional part")
                    .with_primary(span, "truncated towards zero")
                    .with_note(
                        "a value too large for an `int` saturates rather than trapping, and \
                         a NaN becomes zero",
                    ),
            );
        }
        hir::Expr { kind: ExprKind::Cast { value: Box::new(value), to }, ty: to, span }
    }

    /// A constant's value as a literal, standing where the name was written.
    ///
    /// The span stays the use's, not the declaration's, so a later diagnostic
    /// about the value points at the line the reader is looking at.
    fn const_expr(&mut self, index: u32, span: Span) -> hir::Expr {
        match self.consts.get(index) {
            Some(ConstValue::Bool(b)) => self.lit(ExprKind::Bool(*b), TyId::BOOL, span),
            Some(ConstValue::Int(i)) => self.lit(ExprKind::Int(*i), TyId::INT, span),
            Some(ConstValue::Float(f)) => self.lit(ExprKind::Float(*f), TyId::FLOAT, span),
            Some(ConstValue::Str(s)) => {
                let s = s.clone();
                self.lit(ExprKind::Str(s), TyId::STR, span)
            }
            // Its own declaration already reported why it has no value.
            None => self.lit(ExprKind::Error, TyId::ERROR, span),
        }
    }

    fn path_expr(&mut self, p: &ast::Path) -> hir::Expr {
        match self.resolved.lookup_use(p.span) {
            Some(Res::Const(index)) => self.const_expr(index, p.span),

            Some(Res::Local(id)) => {
                self.note_capture(id, p.span);
                if self.taint[id as usize] == Taint::Tainted {
                    let local = &self.locals[id as usize];
                    let (name, decl) = (local.name.clone(), local.span);
                    // The error this value is paired with, for the secondary
                    // span that explains *why*.
                    let err_name = self
                        .guards
                        .iter()
                        .find(|(_, v)| **v == id)
                        .map(|(e, _)| self.locals[*e as usize].name.clone());
                    let mut d = Diagnostic::error(
                        codes::E0301,
                        format!("`{}` is used before its error is checked", name),
                    )
                    .with_secondary(decl, "this value is only valid when the error is nil")
                    .with_primary(p.span, "used here while still tainted");
                    if let Some(e) = err_name {
                        d = d.with_note(format!(
                            "check it first: write `check {}`, or test `{} != nil`",
                            e, e
                        ));
                    }
                    d = d.with_note(
                        "in Go the value on a failure path is the zero value and flows onward \
                         looking valid; in Kite there is no value on that path at all",
                    );
                    self.diags.push(d);
                    // One mistake, one diagnostic.
                    self.taint[id as usize] = Taint::Clean;
                }
                // Reading an error *is* inspecting it. Handing it to
                // something that will deal with it — `errors.wrap(err, …)`,
                // `log(err)`, `test.failed(err, …)` — is a way of handling a
                // failure, and E0302 exists to catch the error nobody looked
                // at, not the error somebody passed on.
                if self.taint[id as usize] == Taint::Unchecked {
                    self.taint[id as usize] = Taint::Clean;
                }
                if self.init[id as usize] != Init::Assigned {
                    let local = &self.locals[id as usize];
                    let (name, decl) = (local.name.clone(), local.span);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0110,
                            format!("`{}` may not have a value here", name),
                        )
                        .with_primary(p.span, "used before being assigned")
                        .with_secondary(decl, "declared without a value here")
                        .with_note(
                            "a `let` without an initialiser must be assigned on every path \
                             before it is read",
                        ),
                    );
                    // Mark it assigned so one omission yields one diagnostic.
                    self.init[id as usize] = Init::Assigned;
                }
                let declared = self.locals[id as usize].ty;
                let read = hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(id)),
                    ty: declared,
                    span: p.span,
                };
                let narrowed = self.narrowed.get(&id).copied();
                // What an editor says when this is hovered. The *narrowed*
                // type where there is one: inside `if x != nil`, `x` is a
                // `User` and saying `Option<User>` there would be telling the
                // reader the opposite of what the compiler just proved.
                let shown = narrowed.unwrap_or(declared);
                let name = self.locals[id as usize].name.clone();
                let text = self.types.name(shown);
                self.solved.locals.push((p.span, name, text));
                match narrowed {
                    Some(inner) => hir::Expr {
                        kind: ExprKind::Unwrap { value: Box::new(read) },
                        ty: inner,
                        span: p.span,
                    },
                    None => read,
                }
            }
            Some(Res::Fn(id)) => self.fn_value(id, p.span),
            Some(Res::Builtin(_)) => {
                // A builtin is not a function value: it has no body of its
                // own, and several of them choose what to do from the type of
                // their argument.
                self.builtin_not_a_value(p.span);
                self.lit(ExprKind::Error, TyId::ERROR, p.span)
            }
            Some(Res::Type(ti)) => {
                let name = self.resolved.type_decl(ti).name.clone();
                self.diags.push(
                    Diagnostic::error(codes::E0200, format!("`{}` is a type, not a value", name))
                        .with_primary(p.span, "a type name cannot stand alone here")
                        .with_note(format!(
                            "to build one, write a struct literal such as `{}{{ … }}`",
                            name
                        )),
                );
                self.lit(ExprKind::Error, TyId::ERROR, p.span)
            }
            // A unit variant used as a value: `Status.Active`.
            Some(Res::Variant(ti, vi)) => self.variant_value(ti, vi, &[], &[], p.span, p.span, None),
            // Resolution already reported this.
            None => self.lit(ExprKind::Error, TyId::ERROR, p.span),
        }
    }

    /// A named function used as a value.
    ///
    /// It becomes a closure that captured nothing, which is what a function
    /// reference *is* once the representation is settled: code plus an empty
    /// environment. Nothing downstream needs a second form for it.
    ///
    /// A generic function has no single body to point at — which instantiation
    /// would it be? — so it is refused with that as the reason.
    fn fn_value(&mut self, id: u32, span: Span) -> hir::Expr {
        let sig = &self.sigs[id as usize];
        if !sig.generics.is_empty() {
            let name = self.resolved.fns[id as usize].name.clone();
            self.diags.push(
                Diagnostic::error(
                    codes::E0209,
                    format!("`{}` is generic, so it has no single value", name),
                )
                .with_primary(span, "a generic function is a template, not a function")
                .with_note("wrap the call in a closure, which fixes the type arguments"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        let (params, ret) = (sig.params.clone(), sig.ret);
        let ty = self.types.fn_of(params, ret);
        hir::Expr {
            kind: ExprKind::ClosureNew {
                func: hir::FnId(id),
                captures: Vec::new(),
                targs: Vec::new(),
            },
            ty,
            span,
        }
    }

    /// `await t` — the value of a task, once it has one.
    ///
    /// Only inside an `async fn`, because awaiting is suspending and a
    /// function that can suspend has to say so in its signature. That is the
    /// whole of the "function colouring" rule, and it is what keeps the
    /// scheduler out of the middle of an ordinary call.
    fn await_expr(&mut self, inner: &ast::Expr, span: Span) -> hir::Expr {
        let value = self.expr(inner, None);
        // A closure is a function of its own, and never an async one — so an
        // `await` in it is outside an async function wherever the closure is
        // written. Letting the enclosing `async fn` answer for it put a
        // suspension into a body the state-machine transform never sees.
        if let Some(closure) = self.closure_span {
            self.diags.push(
                Diagnostic::error(codes::E0521, "`await` inside a closure")
                    .with_primary(span, "this would suspend the closure")
                    .with_secondary(closure, "a closure cannot be `async`")
                    .with_note(
                        "await the task before making the closure, or make the closure \
                         return the task and await it where it is called",
                    ),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        if !self.sigs[self.fn_index].is_async {
            self.diags.push(
                Diagnostic::error(codes::E0521, "`await` outside an async function")
                    .with_primary(span, "this suspends the enclosing function")
                    .with_secondary(self.sigs[self.fn_index].name_span, "declared here")
                    .with_note("write `async fn` to allow it to suspend"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        let Some(payload) = self.types.task_payload(value.ty) else {
            if !self.types.is_poisoned(value.ty) {
                let found = self.types.with_article(value.ty);
                self.diags.push(
                    Diagnostic::error(codes::E0200, "only a task can be awaited")
                        .with_primary(value.span, format!("this is {}", found))
                        .with_note(
                            "calling an `async fn` without `await` yields its `Task<T>`; \
                             `await` is how the value comes out",
                        ),
                );
            }
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };
        hir::Expr {
            kind: ExprKind::Await { value: Box::new(value) },
            ty: payload,
            span,
        }
    }

    /// A call to a named function, reached unqualified or through its module.
    ///
    /// `callee_span` is the callee exactly as written, which is where an inlay
    /// hint for the solved type arguments belongs — after the name, where a
    /// language with a turbofish would have let them be spelled.
    fn fn_call(
        &mut self,
        id: u32,
        text: &str,
        callee_span: Span,
        args: &[ast::Expr],
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        let sig_params = self.sigs[id as usize].params.clone();
        let ret = self.sigs[id as usize].ret;
        let decl_span = self.sigs[id as usize].name_span;
        let generics = self.sigs[id as usize].generics.clone();

        if args.len() != sig_params.len() {
            self.arity_error(text, args.len(), sig_params.len(), span, Some(decl_span));
        }

        // A generic call works its type arguments out from the values around
        // it. There is no turbofish, so a parameter nothing mentions cannot be
        // supplied at all — which is reported rather than silently defaulted.
        //
        // The arguments are the usual source, and the type the result is
        // *used* as is the other one. It is consulted first, so that what it
        // settles is known while the arguments are checked: `ui.box_of(name,
        // style, [])` returning a `Node<Msg>` needs `Msg` before the empty
        // slice can be given an element type, and no argument will ever supply
        // it. Seeding only ever fills a parameter that was unknown, so a call
        // that compiles without a context compiles the same way with one.
        let mut subst: Vec<Option<TyId>> = vec![None; generics.len()];
        if !generics.is_empty() {
            if let Some(want) = expected {
                self.unify(ret, want, &generics, &mut subst, span);
            }
        }
        let hargs = self.check_args(&sig_params, &generics, &mut subst, args, Some(decl_span));

        let targs = self.finish_subst(&generics, &subst, span);
        self.check_bounds(&generics, &targs, span);
        self.note_generic_call(id, &targs, span);
        // What the call inferred, once it inferred all of it. A partial answer
        // would be a hint that lies, so an unsolved parameter records nothing.
        if !targs.is_empty() && !targs.iter().any(|t| self.types.is_poisoned(*t)) {
            let names: Vec<String> = targs.iter().map(|t| self.types.name(*t)).collect();
            self.solved.calls.push((callee_span, names.join(", ")));
        }
        let ret = self.apply_subst(ret, &subst);
        // Calling an `async fn` starts it and yields the task. That is how
        // concurrency is expressed: two calls then one `await` of each runs
        // both at once, and there is no second keyword for it.
        let ret = if self.sigs[id as usize].is_async {
            let task = self.types.task_of(ret, span);
            self.types.struct_ty(task)
        } else {
            ret
        };

        hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(id), args: hargs, targs },
            ty: ret,
            span,
        }
    }

    /// Check a call's arguments against a signature, solving its type
    /// parameters as they go.
    ///
    /// Shared by every call that can be generic — a function, a method, an
    /// associated function, a trait's method reached through a bound — so
    /// they infer, convert and report alike. `subst` arrives holding whatever
    /// is already known, such as the arguments a receiver's own type fixes.
    fn check_args(
        &mut self,
        sig_params: &[TyId],
        generics: &[GenericDef],
        subst: &mut Vec<Option<TyId>>,
        args: &[ast::Expr],
        decl_span: Option<Span>,
    ) -> Vec<hir::Expr> {
        // A callee that demands `Share` of its type arguments is one that
        // hands values to another task — and a function it is given goes with
        // them, along with everything that function captured.
        let crosses = generics
            .iter()
            .any(|g| g.bounds.iter().any(|b| self.is_share_bound(*b)));
        let mut hargs = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let declared = sig_params.get(i).copied();
            // An argument is checked against the parameter type only once that
            // type is fully known; until then it is checked on its own and used
            // to fill parameters in.
            let want = declared.and_then(|d| self.apply_subst_opt(d, subst));
            let e = self.expr(a, want);
            let e = self.coerce(e, want);
            let e = match declared {
                Some(d) => {
                    // A conflict is its own diagnostic, naming both sides;
                    // a mismatch reported on top of it would say the same
                    // thing again, less clearly.
                    let before = self.diags.error_count();
                    self.unify(d, e.ty, generics, subst, e.span);
                    if self.diags.error_count() != before {
                        e
                    } else {
                        // Converted again now the parameter is known: `get(7,
                        // 3)` against `x: Option<T>` only learns that the
                        // `7` must be wrapped once `T` is `int`, which is
                        // after it was checked.
                        let expected = self.apply_subst(d, subst);
                        self.accept(e, expected, decl_span)
                    }
                }
                None => e,
            };
            if crosses && declared.is_some_and(|d| matches!(self.types.kind(d), TyKind::Fn { .. })) {
                self.require_share_captures(&e);
            }
            hargs.push(e);
        }
        hargs
    }

    /// A function handed to another task takes its captures with it, so each
    /// must be `Share` exactly as an argument would (§4.5, §12.3). A closure
    /// capturing a `Counter` with a `var` field, or a `JsValue`, would
    /// otherwise carry one across the boundary that the arguments are checked
    /// at.
    fn require_share_captures(&mut self, e: &hir::Expr) {
        match &e.kind {
            ExprKind::ClosureNew { captures, .. } => {
                for c in captures {
                    if self.types.is_poisoned(c.ty) || self.share_given_params(c.ty) {
                        continue;
                    }
                    let ExprKind::Local(id) = c.kind else { continue };
                    let (name, decl) = {
                        let l = &self.locals[id.0 as usize];
                        (l.name.clone(), l.span)
                    };
                    let what = self.types.with_article(c.ty);
                    let mut d = Diagnostic::error(
                        codes::E0520,
                        "this closure cannot be moved to another task",
                    )
                    .with_primary(e.span, format!("it captures `{}`, which is {}", name, what))
                    .with_secondary(decl, format!("`{}` is declared here", name));
                    d = if self.types.mentions_host_value(c.ty) {
                        d.with_note(
                            "a `JsValue` belongs to the isolate that created it, and the \
                             closure would carry it to one where it names nothing",
                        )
                    } else {
                        d.with_note(format!(
                            "{} is not `Share`, and a closure takes what it captures along \
                             with it: two tasks would hold one mutable value",
                            what
                        ))
                    };
                    self.diags.push(d.with_note(
                        "pass what the work needs as the items instead, or capture a value \
                         that is `Share`",
                    ));
                }
            }
            // A function value whose closure is out of sight here — held in a
            // binding, returned from a call — could be carrying anything.
            ExprKind::Local(_)
            | ExprKind::Call { .. }
            | ExprKind::CallClosure { .. }
            | ExprKind::FieldGet { .. }
            | ExprKind::Index { .. }
            | ExprKind::If { .. }
            | ExprKind::Match { .. } => {
                if self.types.is_poisoned(e.ty) {
                    return;
                }
                self.diags.push(
                    Diagnostic::error(
                        codes::E0520,
                        "a function handed to another task must be written where it is handed",
                    )
                    .with_primary(e.span, "what this captured cannot be seen here")
                    .with_note(
                        "a closure takes what it captures to the other task, and each capture \
                         must be `Share`; write the closure literal here, or name a function, \
                         so the captures can be checked",
                    ),
                );
            }
            _ => {}
        }
    }

    fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        arg_names: &[Option<ast::Ident>],
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        // Named arguments exist only for named-payload variant construction.
        // Everywhere else, a function needing many optional inputs takes a
        // struct, which is the specification's answer.
        let named_variant = matches!(
            self.resolved.lookup_use(match callee {
                ast::Expr::Path(p) => p.span,
                ast::Expr::Field { span, .. } => *span,
                _ => span,
            }),
            Some(Res::Variant(..))
        );
        if !named_variant {
            if let Some(n) = arg_names.iter().flatten().next() {
                self.diags.push(
                    Diagnostic::error(codes::E0113, "functions do not take named arguments")
                        .with_primary(n.span, "named argument here")
                        .with_note(
                            "Kite has no named arguments; a function needing many optional \
                             inputs takes a struct, whose literal names every field anyway",
                        ),
                );
            }
        }

        // `a.b(…)` is a method call unless the resolver already decided the
        // dotted name is static — a builtin, a variant, or an associated
        // function on a type.
        if let ast::Expr::Field { base, name, span: fspan } = callee {
            {
                return match self.resolved.lookup_use(*fspan) {
                    Some(Res::Builtin(b)) => self.builtin_call(b, args, span),
                    // `task.all(…)` — a function reached through its module.
                    Some(Res::Fn(id)) => self.fn_call(
                        id,
                        &format!("{}.{}", expr_text(base), name.name),
                        *fspan,
                        args,
                        span,
                        expected,
                    ),
                    Some(Res::Type(ti)) => {
                        self.associated_call_named(ti, &name.name, *fspan, args, span, expected)
                    }
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_value(ti, vi, args, arg_names, *fspan, span, expected)
                    }
                    _ => self.method_call(base, name, args, span, expected),
                };
            }
        }

        // Anything else in callee position — `handlers[0](e)`, `pick()(3)` —
        // is a value, and calling it is well-formed exactly when its type is a
        // function. This used to be refused as unimplemented, which put a hole
        // in the language at the point where closures stop being held in a
        // plain binding and start being held in the data structure a program
        // actually keeps them in.
        let ast::Expr::Path(p) = callee else {
            let value = self.expr(callee, None);
            if self.types.is_poisoned(value.ty) {
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            }
            let TyKind::Fn { params: sig_params, ret } = self.types.kind(value.ty).clone() else {
                self.diags.push(
                    Diagnostic::error(codes::E0205, "this is not a function")
                        .with_primary(
                            value.span,
                            format!("this is {}", self.types.with_article(value.ty)),
                        )
                        .with_note("only a value whose type is a `fn(…)` can be called"),
                );
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            };
            if args.len() != sig_params.len() {
                self.arity_error("this function", args.len(), sig_params.len(), span, None);
            }
            let mut hargs = Vec::with_capacity(args.len());
            for (i, a) in args.iter().enumerate() {
                let want = sig_params.get(i).copied();
                let e = self.expr(a, want);
                let e = self.coerce(e, want);
                if let Some(w) = want {
                    self.expect_ty(e.ty, w, e.span, None);
                }
                hargs.push(e);
            }
            return hir::Expr {
                kind: ExprKind::CallClosure { callee: Box::new(value), args: hargs },
                ty: ret,
                span,
            };
        };

        match self.resolved.lookup_use(p.span) {
            Some(Res::Fn(id)) => self.fn_call(id, &p.text(), p.span, args, span, expected),

            Some(Res::Builtin(b)) => self.builtin_call(b, args, span),

            // `LIMIT()` where `LIMIT` is a constant. Saying "not a function"
            // and naming what it is instead is the whole of the help needed:
            // the fix is to delete two characters.
            Some(Res::Const(index)) => {
                let value = self.const_expr(index, p.span);
                let decl = self.resolved.consts[index as usize].span;
                self.diags.push(
                    Diagnostic::error(codes::E0205, format!("`{}` is not a function", p.text()))
                        .with_primary(span, format!("this is {}", self.types.with_article(value.ty)))
                        .with_secondary(decl, "declared as a constant here")
                        .with_note("a constant is already a value; drop the `()`"),
                );
                value
            }

            Some(Res::Local(id)) => {
                let ty = self.locals[id as usize].ty;
                let decl = self.locals[id as usize].span;
                // A local whose type *is* a function is a different failure
                // from one that is not: the call is well-formed and it is the
                // compiler that cannot do it yet. Saying "not a function" of a
                // `fn(int) -> int` reads as a contradiction, because it is one.
                if let TyKind::Fn { params: sig_params, ret } = self.types.kind(ty).clone() {
                    self.note_capture(id, p.span);
                    let callee = hir::Expr {
                        kind: ExprKind::Local(hir::LocalId(id)),
                        ty,
                        span: p.span,
                    };
                    if args.len() != sig_params.len() {
                        self.arity_error(&p.text(), args.len(), sig_params.len(), span, Some(decl));
                    }
                    let mut hargs = Vec::with_capacity(args.len());
                    for (i, a) in args.iter().enumerate() {
                        let want = sig_params.get(i).copied();
                        let e = self.expr(a, want);
                        let e = self.coerce(e, want);
                        if let Some(w) = want {
                            self.expect_ty(e.ty, w, e.span, Some(decl));
                        }
                        hargs.push(e);
                    }
                    return hir::Expr {
                        kind: ExprKind::CallClosure { callee: Box::new(callee), args: hargs },
                        ty: ret,
                        span,
                    };
                }
                // A local whose initialiser was already reported has no type
                // to be wrong about: `let f = Rect.square` then `f(2)` is one
                // mistake, and it is on the first line.
                if !self.types.is_poisoned(ty) {
                    self.diags.push(
                        Diagnostic::error(codes::E0205, format!("`{}` is not a function", p.text()))
                            .with_primary(
                                p.span,
                                format!("this is {}", self.types.with_article(ty)),
                            )
                            .with_secondary(decl, "declared here"),
                    );
                }
                self.lit(ExprKind::Error, TyId::ERROR, span)
            }

            Some(Res::Type(ti)) => self.associated_call(ti, p, args, span, expected),

            Some(Res::Variant(ti, vi)) => {
                self.variant_value(ti, vi, args, arg_names, p.span, span, expected)
            }

            None => self.lit(ExprKind::Error, TyId::ERROR, span),
        }
    }

    /// `draw.rect(x, y, w, h, colour)` and `draw.text(x, y, body, colour)`.
    ///
    /// Coordinates are floats because a layout produces floats; a colour is an
    /// `int` holding `0xRRGGBB`, because a struct crossing the host boundary
    /// would need a representation both renderers agreed on and neither needs.
    fn draw_call(
        &mut self,
        b: BuiltinFn,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let text = b == BuiltinFn::DrawText;
        let rounded = b == BuiltinFn::DrawRRect;
        let wanted: [TyId; 6] = if text {
            [TyId::FLOAT, TyId::FLOAT, TyId::STR, TyId::INT, TyId::INT, TyId::INT]
        } else if rounded {
            [TyId::FLOAT, TyId::FLOAT, TyId::FLOAT, TyId::FLOAT, TyId::FLOAT, TyId::INT]
        } else {
            [TyId::FLOAT, TyId::FLOAT, TyId::FLOAT, TyId::FLOAT, TyId::INT, TyId::INT]
        };
        let arity = if text {
            4
        } else if rounded {
            6
        } else {
            5
        };
        if args.len() != arity {
            self.arity_error(b.path(), args.len(), arity, span, None);
        }
        let mut hargs = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let want = wanted.get(i).copied();
            let e = self.expr(a, want);
            if let Some(w) = want {
                self.expect_ty(e.ty, w, e.span, None);
            }
            hargs.push(e);
        }
        let builtin = if text {
            Builtin::DrawText
        } else if rounded {
            Builtin::DrawRRect
        } else {
            Builtin::DrawRect
        };
        hir::Expr { kind: ExprKind::CallBuiltin { builtin, args: hargs }, ty: TyId::UNIT, span }
    }

    fn builtin_call(&mut self, b: BuiltinFn, args: &[ast::Expr], span: Span) -> hir::Expr {
        match b {
            // Handled above, but named here so adding a builtin fails to
            // compile rather than falling through to something else.
            BuiltinFn::DrawRect | BuiltinFn::DrawRRect | BuiltinFn::DrawText => {
                self.draw_call(b, args, span)
            }

            // `draw.font(size, weight)`. A weight is an `int` on the CSS scale
            // — 400 is regular, 500 medium, 700 bold — because that is the
            // scale both renderers already speak and inventing a second one
            // would only need translating back.
            // `draw.drrect(x, y, w, h, radius, width, colour)`.
            BuiltinFn::DrawDRRect => {
                if args.len() != 7 {
                    self.arity_error("draw.drrect", args.len(), 7, span, None);
                }
                let wanted = [
                    TyId::FLOAT,
                    TyId::FLOAT,
                    TyId::FLOAT,
                    TyId::FLOAT,
                    TyId::FLOAT,
                    TyId::FLOAT,
                    TyId::INT,
                ];
                let mut hargs = Vec::with_capacity(7);
                for (i, a) in args.iter().enumerate() {
                    let want = wanted.get(i).copied();
                    let e = self.expr(a, want);
                    if let Some(w) = want {
                        self.expect_ty(e.ty, w, e.span, None);
                    }
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawDRRect, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `draw.alpha(a)`, with `a` in 0..1.
            BuiltinFn::DrawAlpha => {
                if args.len() != 1 {
                    self.arity_error("draw.alpha", args.len(), 1, span, None);
                }
                let mut hargs = Vec::with_capacity(1);
                for a in args {
                    let e = self.expr(a, Some(TyId::FLOAT));
                    self.expect_ty(e.ty, TyId::FLOAT, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawAlpha, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            BuiltinFn::DrawFont => {
                if args.len() != 2 {
                    self.arity_error("draw.font", args.len(), 2, span, None);
                }
                let wanted = [TyId::FLOAT, TyId::INT];
                let mut hargs = Vec::with_capacity(2);
                for (i, a) in args.iter().enumerate() {
                    let want = wanted.get(i).copied();
                    let e = self.expr(a, want);
                    if let Some(w) = want {
                        self.expect_ty(e.ty, w, e.span, None);
                    }
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawFont, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            BuiltinFn::DrawClip => {
                if args.len() != 4 {
                    self.arity_error("draw.clip", args.len(), 4, span, None);
                }
                let mut hargs = Vec::with_capacity(4);
                for a in args {
                    let e = self.expr(a, Some(TyId::FLOAT));
                    self.expect_ty(e.ty, TyId::FLOAT, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawClip, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            BuiltinFn::DrawUnclip => {
                if !args.is_empty() {
                    self.arity_error("draw.unclip", args.len(), 0, span, None);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: Builtin::DrawUnclip,
                        args: Vec::new(),
                    },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `task.yield()` — the suspension primitive. It is a statement,
            // not a value: what comes back is the same nothing that went in.
            BuiltinFn::TaskYield => {
                if !args.is_empty() {
                    self.arity_error("task.yield", args.len(), 0, span, None);
                }
                if !self.sigs[self.fn_index].is_async {
                    self.diags.push(
                        Diagnostic::error(codes::E0521, "`task.yield` outside an async function")
                            .with_primary(span, "this suspends the enclosing function")
                            .with_secondary(self.sigs[self.fn_index].name_span, "declared here")
                            .with_note("write `async fn` to allow it to suspend"),
                    );
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                hir::Expr { kind: ExprKind::Yield, ty: TyId::UNIT, span }
            }

            BuiltinFn::TaskWakeAt => {
                if args.len() != 1 {
                    self.arity_error("task.wake_at", args.len(), 1, span, None);
                }
                let mut hargs = Vec::with_capacity(1);
                for a in args {
                    let e = self.expr(a, Some(TyId::INT));
                    self.expect_ty(e.ty, TyId::INT, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::TaskWakeAt, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `assert(cond, "…")` and `require(cond, "…")`.
            //
            // Both take the message eagerly rather than lazily: a message is a
            // string, building one is cheap, and a closure that might run
            // would be a second kind of control flow to explain.
            BuiltinFn::Assert | BuiltinFn::Require => {
                let name = b.path();
                if args.len() != 2 {
                    self.arity_error(name, args.len(), 2, span, None);
                    return self.lit(ExprKind::Error, TyId::UNIT, span);
                }
                let cond = self.expr(&args[0], Some(TyId::BOOL));
                if !self.types.satisfies(cond.ty, TyId::BOOL) && !self.types.is_poisoned(cond.ty) {
                    let found = self.types.with_article(cond.ty);
                    self.diags.push(
                        Diagnostic::error(codes::E0202, format!("`{}` takes a `bool`", name))
                            .with_primary(cond.span, format!("this is {}", found))
                            .with_note("Kite has no truthiness; compare explicitly"),
                    );
                }
                let message = self.expr(&args[1], Some(TyId::STR));
                self.expect_ty(message.ty, TyId::STR, message.span, None);
                // An assertion is a claim about the program, not about its
                // input, so a release build drops it. A `require` is a claim
                // about what a caller passed, and stays.
                if b == BuiltinFn::Assert && self.release {
                    return self.lit(ExprKind::Error, TyId::UNIT, span);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: Builtin::Require,
                        args: vec![cond, message],
                    },
                    ty: TyId::UNIT,
                    span,
                }
            }

            BuiltinFn::PtrSame => {
                if args.len() != 2 {
                    self.arity_error("ptr.same", args.len(), 2, span, None);
                    return self.lit(ExprKind::Error, TyId::BOOL, span);
                }
                let left = self.expr(&args[0], None);
                // The second argument is checked against the first, so
                // `ptr.same(model, "model")` names the mismatch rather than
                // reporting two unrelated complaints.
                let right = self.expr(&args[1], Some(left.ty));
                self.expect_ty(right.ty, left.ty, right.span, None);

                if !self.types.has_identity(left.ty) && !self.types.is_poisoned(left.ty) {
                    let article = self.types.with_article(left.ty);
                    let mut d = Diagnostic::error(
                        codes::E0213,
                        format!("`ptr.same` cannot compare {}", article),
                    )
                    .with_primary(left.span, format!("this is {}", article));
                    // A slice is the near miss worth naming: it *is* a heap
                    // allocation, so being told it has no identity is
                    // surprising without the reason.
                    d = match self.types.kind(left.ty) {
                        TyKind::Slice(_) => d.with_note(
                            "slices are copy-on-write values: whether two share a buffer \
                             is an allocator detail, and a write to either ends it",
                        ),
                        _ => d.with_note(
                            "only structs, enums and maps are one cell two names can share",
                        ),
                    };
                    self.diags.push(d);
                }

                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: Builtin::PtrSame,
                        args: vec![left, right],
                    },
                    ty: TyId::BOOL,
                    span,
                }
            }

            BuiltinFn::TaskPark | BuiltinFn::TaskWaitHost => {
                if !args.is_empty() {
                    self.arity_error(b.path(), args.len(), 0, span, None);
                }
                let builtin = if b == BuiltinFn::TaskPark {
                    Builtin::TaskPark
                } else {
                    Builtin::TaskWaitHost
                };
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin, args: Vec::new() },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `js.func(handler) -> JsValue`.
            //
            // The expected type is supplied to the argument rather than
            // inferred from it, so `js.func(|e| …)` needs no annotation on `e`
            // — which matters because every listener a program writes goes
            // through here and annotating each one would be noise.
            BuiltinFn::JsFunc => {
                if args.len() != 1 {
                    self.arity_error("js.func", args.len(), 1, span, None);
                    return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
                }

                // The expected type is supplied to the argument rather than
                // inferred from it, so `js.func(|e| …)` needs no annotation on
                // `e` — which matters because every listener a program writes
                // goes through here and annotating each one would be noise.
                //
                // Which shape to expect is read off the closure literal, so
                // that a two-argument observer callback and a comparator that
                // answers with a value are inferred just as a listener is. A
                // handler that is not written inline — a named function, or one
                // out of a variable — is checked without an expectation and
                // validated after.
                let wanted = match &args[0] {
                    ast::Expr::Closure { params, ret, .. } => {
                        let n = params.len();
                        let returns = ret.is_some();
                        (n <= JS_FUNC_MAX_ARITY).then(|| {
                            self.types.fn_of(
                                vec![TyId::JS_VALUE; n],
                                if returns { TyId::JS_VALUE } else { TyId::UNIT },
                            )
                        })
                    }
                    _ => None,
                };
                let handler = self.expr(&args[0], wanted);
                if !self.types.is_poisoned(handler.ty) && !self.is_js_handler(handler.ty) {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!(
                                "`js.func` cannot hand {} to the host",
                                self.types.with_article(handler.ty)
                            ),
                        )
                        .with_primary(handler.span, "not a handler")
                        .with_note(
                            "a handler takes up to four `JsValue` parameters — whatever the \
                             thing calling it passes — and answers with a `JsValue` or with \
                             nothing",
                        )
                        .with_note(
                            "a Kite aggregate does not cross the boundary, so a parameter or \
                             a result that is a struct, slice, map or tuple has no host form \
                             to take",
                        ),
                    );
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::JsFunc, args: vec![handler] },
                    ty: TyId::JS_VALUE,
                    span,
                }
            }

            BuiltinFn::TimeNow => {
                if !args.is_empty() {
                    self.arity_error("time.now", args.len(), 0, span, None);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::TimeNow, args: Vec::new() },
                    ty: TyId::INT,
                    span,
                }
            }

            // Reading a task without suspending. `finished` is a field read
            // and `get` is another; a combinator that must not block on one
            // particular task is what they exist for.
            BuiltinFn::TaskFinished | BuiltinFn::TaskGet => {
                let name = b.path();
                if args.len() != 1 {
                    self.arity_error(name, args.len(), 1, span, None);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                let task = self.expr(&args[0], None);
                let Some(payload) = self.types.task_payload(task.ty) else {
                    if !self.types.is_poisoned(task.ty) {
                        let found = self.types.with_article(task.ty);
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("`{}` needs a task", name),
                            )
                            .with_primary(task.span, format!("this is {}", found)),
                        );
                    }
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                };
                let index = u32::from(b == BuiltinFn::TaskGet);
                let ty = if index == 0 { TyId::BOOL } else { payload };
                hir::Expr {
                    kind: ExprKind::FieldGet { base: Box::new(task), index },
                    ty,
                    span,
                }
            }

            BuiltinFn::TextHeight => {
                if !args.is_empty() {
                    self.arity_error("text.height", args.len(), 0, span, None);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: Builtin::TextHeight,
                        args: Vec::new(),
                    },
                    ty: TyId::FLOAT,
                    span,
                }
            }

            // `draw.field(x, y, w, h, value, hint, colour, id, multiline)` — a
            // region, its text, its ink, who it is and whether it takes more
            // than one line.
            BuiltinFn::DrawField => {
                if args.len() != 9 {
                    self.arity_error("draw.field", args.len(), 9, span, None);
                }
                let mut hargs = Vec::with_capacity(9);
                for (i, a) in args.iter().enumerate() {
                    let want = match i {
                        4 | 5 | 7 => TyId::STR,
                        6 => TyId::INT,
                        8 => TyId::BOOL,
                        _ => TyId::FLOAT,
                    };
                    let e = self.expr(a, Some(want));
                    self.expect_ty(e.ty, want, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawField, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `draw.image(x, y, w, h, src)` — a box, and where the picture
            // comes from.
            BuiltinFn::DrawImage => {
                if args.len() != 5 {
                    self.arity_error("draw.image", args.len(), 5, span, None);
                }
                let mut hargs = Vec::with_capacity(5);
                for (i, a) in args.iter().enumerate() {
                    let want = if i == 4 { TyId::STR } else { TyId::FLOAT };
                    let e = self.expr(a, Some(want));
                    self.expect_ty(e.ty, want, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawImage, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `draw.semantics(x, y, w, h, role, label, flags, id)` — a region,
            // and what it is.
            BuiltinFn::DrawSemantics => {
                if args.len() != 8 {
                    self.arity_error("draw.semantics", args.len(), 8, span, None);
                }
                let mut hargs = Vec::with_capacity(8);
                for (i, a) in args.iter().enumerate() {
                    let want = match i {
                        4 | 6 => TyId::INT,
                        5 | 7 => TyId::STR,
                        _ => TyId::FLOAT,
                    };
                    let e = self.expr(a, Some(want));
                    self.expect_ty(e.ty, want, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::DrawSemantics, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }

            // `text.from_code(code)` — a code point in, one character out.
            BuiltinFn::TextFromCode => {
                if args.len() != 1 {
                    self.arity_error("text.from_code", args.len(), 1, span, None);
                }
                let mut hargs = Vec::with_capacity(1);
                for a in args.iter() {
                    let e = self.expr(a, Some(TyId::INT));
                    self.expect_ty(e.ty, TyId::INT, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::TextFromCode, args: hargs },
                    ty: TyId::STR,
                    span,
                }
            }

            BuiltinFn::TextWidth => {
                if args.len() != 1 {
                    self.arity_error("text.width", args.len(), 1, span, None);
                }
                let mut hargs = Vec::with_capacity(1);
                for a in args {
                    let e = self.expr(a, Some(TyId::STR));
                    self.expect_ty(e.ty, TyId::STR, e.span, None);
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::TextWidth, args: hargs },
                    ty: TyId::FLOAT,
                    span,
                }
            }
            BuiltinFn::ErrorsNew => {
                if args.len() != 1 {
                    self.arity_error("errors.new", args.len(), 1, span, None);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                let m = self.expr(&args[0], Some(TyId::STR));
                self.expect_ty(m.ty, TyId::STR, m.span, None);
                let span_of = m.span;
                hir::Expr {
                    kind: ExprKind::ErrorNew {
                        message: Box::new(m),
                        value: Box::new(Self::nothing(span_of)),
                        tag: Box::new(hir::Expr {
                            kind: ExprKind::Int(0),
                            ty: TyId::INT,
                            span: span_of,
                        }),
                        cause: Box::new(Self::nothing(span_of)),
                    },
                    ty: TyId::ERR,
                    span,
                }
            }

            // `errors.because(message, cause)` — the constructor `wrap` is
            // written over. A builtin rather than Kite for the same reason
            // `errors.new` is: building an error is the one thing about them
            // that cannot be written in the language.
            BuiltinFn::ErrorsBecause => {
                if args.len() != 2 {
                    self.arity_error("errors.because", args.len(), 2, span, None);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                let m = self.expr(&args[0], Some(TyId::STR));
                self.expect_ty(m.ty, TyId::STR, m.span, None);
                let c = self.expr(&args[1], Some(TyId::ERR));
                let c = self.accept(c, TyId::ERR, None);
                let span_of = m.span;
                hir::Expr {
                    kind: ExprKind::ErrorNew {
                        message: Box::new(m),
                        value: Box::new(Self::nothing(span_of)),
                        tag: Box::new(hir::Expr {
                            kind: ExprKind::Int(0),
                            ty: TyId::INT,
                            span: span_of,
                        }),
                        cause: Box::new(c),
                    },
                    ty: TyId::ERR,
                    span,
                }
            }

            BuiltinFn::IoReadLine => {
                if !args.is_empty() {
                    self.arity_error("io.read_line", args.len(), 0, span, None);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: Builtin::IoReadLine,
                        args: Vec::new(),
                    },
                    ty: TyId::STR,
                    span,
                }
            }

            // `io.error` is `io.print` to the other stream, and shares its
            // whole rule: the same types print, and a `Display` type renders
            // through the same call, so the two cannot drift.
            BuiltinFn::IoPrint | BuiltinFn::IoError => {
                let name = if matches!(b, BuiltinFn::IoError) { "io.error" } else { "io.print" };
                if args.len() != 1 {
                    self.arity_error(name, args.len(), 1, span, None);
                }
                let mut hargs = Vec::new();
                for a in args {
                    let e = self.expr(a, None);
                    // A type with `Display` prints through it, so `io.print(p)`
                    // and `"\(p)"` agree by construction rather than by
                    // everyone remembering to keep them the same.
                    if !self.types.is_printable(e.ty) && !self.types.is_poisoned(e.ty) {
                        if let Some(shown) = self.show_through_display(e.clone()) {
                            hargs.push(shown);
                            continue;
                        }
                    }
                    if !self.types.is_printable(e.ty) && !self.types.is_poisoned(e.ty) {
                        let article = self.types.with_article(e.ty);
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!(
                                    "`{}` cannot print a `{}`",
                                    name,
                                    self.types.name(e.ty)
                                ),
                            )
                            .with_primary(e.span, format!("this is {}", article))
                            .with_note(
                                format!(
                                    "`{}` takes `int`, `float`, `bool` and `str` directly, \
                                     and anything that implements `Display`",
                                    name
                                ),
                            )
                            .with_note(format!(
                                "write `impl Display for {} {{ fn show(self) -> str {{ … }} }}`",
                                self.types.name(e.ty)
                            )),
                        );
                    }
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin {
                        builtin: if matches!(b, BuiltinFn::IoError) {
                            Builtin::IoError
                        } else {
                            Builtin::IoPrint
                        },
                        args: hargs,
                    },
                    ty: TyId::UNIT,
                    span,
                }
            }
        }
    }

    /// A method called on a trait object. Which body runs is decided at run
    /// time from the receiver's concrete type; the signature comes from the
    /// trait, so checking is entirely static.
    fn virtual_call(
        &mut self,
        base: &ast::Expr,
        receiver: hir::Expr,
        tr: hir::TraitId,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let def = self.types.trait_def(tr);
        let Some((index, method)) = def.method(&name.name) else {
            let trait_name = def.name.clone();
            let available: Vec<String> = def.methods.iter().map(|m| m.name.clone()).collect();
            let mut d = Diagnostic::error(
                codes::E0205,
                format!("`dyn {}` has no method `{}`", trait_name, name.name),
            )
            .with_primary(name.span, "no such method")
            .with_note(
                "a trait object exposes only its trait's methods; the concrete type is \
                 not known here",
            );
            if !available.is_empty() {
                d = d.with_note(format!("`{}` has: {}", trait_name, available.join(", ")));
            }
            self.diags.push(d);
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };
        let (params, ret, var_self) = (method.params.clone(), method.ret, method.var_self);
        let method = index as u32;
        // The trait's receiver is the one every implementation agreed to, so
        // `var self` asks the same of a call through a `dyn` or a bound as it
        // does of a direct call.
        if var_self {
            self.require_mutable_receiver(base, &name.name);
        }

        // `Self` is the receiver's type. Through a bound that is the
        // parameter, and monomorphisation makes it the concrete type; through
        // a `dyn` no method mentioning it is callable at all, which is what
        // object safety is (§10.3).
        let self_param = self.types.self_param();
        let map = [(self_param, receiver.ty)];
        let params: Vec<TyId> = params.iter().map(|p| replace_types(self.types, *p, &map)).collect();
        let ret = replace_types(self.types, ret, &map);

        // A method generic in its own right solves its parameters from its
        // arguments, as a generic function does. They are numbered after the
        // trait's own, so the substitution has a slot for those too.
        let mut generics: Vec<GenericDef> = self
            .trait_index_of(tr)
            .and_then(|ti| self.type_generics.get(ti as usize).cloned())
            .unwrap_or_default();
        let before = generics.len();
        generics.extend(self.trait_method_generics.get(&(tr, index)).cloned().unwrap_or_default());
        let mut subst: Vec<Option<TyId>> = vec![None; generics.len()];

        if args.len() != params.len() {
            self.arity_error(&name.name, args.len(), params.len(), span, None);
        }
        let mut lowered = vec![receiver];
        lowered.extend(self.check_args(&params, &generics, &mut subst, args, None));
        let own = generics[before..].to_vec();
        let targs = self.finish_subst(&own, &subst[before..], span);
        self.check_bounds(&own, &targs, span);
        let ret = self.apply_subst(ret, &subst);
        hir::Expr {
            kind: ExprKind::CallVirtual { trait_id: tr, method, args: lowered, targs },
            ty: ret,
            span,
        }
    }

    /// A method on a `str`.
    ///
    /// The set is small on purpose. Each of these is a host call, and a host
    /// call is a boundary two runtimes have to agree about — `split`,
    /// `starts_with` and the rest are writable in Kite on top of them, and
    /// belong in the standard library where they can be read.
    fn str_method(
        &mut self,
        receiver: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        use kite_hir::StrKind;
        // Operation, then the types of its arguments past the receiver, then
        // what it returns.
        let (op, params, ret): (StrKind, &[TyId], TyId) = match name.name.as_str() {
            "len" => (StrKind::Len, &[], TyId::INT),
            "trim" => (StrKind::Trim, &[], TyId::STR),
            "index_of" => (StrKind::IndexOf, &[TyId::STR], TyId::INT),
            "slice" => (StrKind::Slice, &[TyId::INT, TyId::INT], TyId::STR),
            "code_at" => (StrKind::CodeAt, &[TyId::INT], TyId::INT),
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`str` has no method `{}`", name.name),
                    )
                    .with_primary(name.span, "no such method")
                    .with_note("`str` has: len, slice, index_of, trim, code_at")
                    .with_note(
                        "anything else is writable on top of those, and belongs in the \
                         standard library rather than in the compiler",
                    ),
                );
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            }
        };

        if args.len() != params.len() {
            self.arity_error(&name.name, args.len(), params.len(), span, None);
        }
        let mut hargs = vec![receiver];
        for (i, a) in args.iter().enumerate() {
            let want = params.get(i).copied();
            let e = self.expr(a, want);
            if let Some(w) = want {
                self.expect_ty(e.ty, w, e.span, None);
            }
            hargs.push(e);
        }
        hir::Expr { kind: ExprKind::StrOp { op, args: hargs }, ty: ret, span }
    }

    /// `receiver.method(args)`.
    fn method_call(
        &mut self,
        base: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        let receiver = self.expr(base, None);
        if self.types.is_poisoned(receiver.ty) {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        if receiver.ty == TyId::ERR {
            // `errors.new(…)` and its kin build an error on the spot, and a
            // built error is never nil.
            let built = matches!(receiver.kind, ExprKind::ErrorNew { .. });
            let (kind, ty) = match name.name.as_str() {
                "message" => (
                    ExprKind::ErrorMessage { base: Box::new(receiver) },
                    TyId::STR,
                ),
                // An `error`, not an `Option<error>`: `error` is already the
                // nil-able type — §7.2 — so wrapping it in an optional would
                // be two ways to say absent, and callers would have to open
                // both.
                "cause" => (ExprKind::ErrorCause { base: Box::new(receiver) }, TyId::ERR),
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0205,
                            format!("`error` has no method `{}`", name.name),
                        )
                        .with_primary(name.span, "no such method")
                        .with_note("`error` has: message, cause")
                        .with_note(
                            "to ask which failure it was, name the type: \
                             `NotFound.is(err)` and `NotFound.as(err)`",
                        ),
                    );
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
            };
            if !args.is_empty() {
                self.arity_error(&name.name, args.len(), 0, span, None);
            }
            if !built {
                self.require_error_present(base, name.span);
            }
            return hir::Expr { kind, ty, span };
        }

        if receiver.ty == TyId::STR {
            return self.str_method(receiver, name, args, span);
        }

        if let TyKind::Map(k, v) = *self.types.kind(receiver.ty) {
            // `remove` mutates, so it is a statement dressed as a unit
            // expression — the shape `push` already has — and it is handled
            // before the table below because that table ends by checking the
            // argument count against zero.
            if name.name == "remove" {
                if args.len() != 1 {
                    self.arity_error("remove", args.len(), 1, span, None);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                let mut stmts = Vec::new();
                let Some(place) =
                    self.container_place(base, receiver, "removed from", None, &mut stmts, span)
                else {
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                };
                let key = self.expr(&args[0], Some(k));
                let key = self.accept(key, k, None);
                self.note_compared(k, key.span);
                let remove = match place.binding() {
                    Some(local) => hir::Stmt::MapRemove { local: hir::LocalId(local), key, span },
                    None => {
                        let key = self.hoist(key, &mut stmts, span);
                        self.change_at(place, stmts, span, |_, local, _| hir::Stmt::MapRemove {
                            local: hir::LocalId(local),
                            key,
                            span,
                        })
                    }
                };
                return self.as_statement(remove, span);
            }
            let (kind, ty) = match name.name.as_str() {
                "len" => (ExprKind::MapLen { base: Box::new(receiver) }, TyId::INT),
                // Insertion order, which the specification guarantees — so
                // `keys` and `values` line up element for element.
                "keys" => {
                    let ty = self.types.slice_of(k);
                    (ExprKind::MapKeys { base: Box::new(receiver) }, ty)
                }
                "values" => {
                    let ty = self.types.slice_of(v);
                    (ExprKind::MapValues { base: Box::new(receiver) }, ty)
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0205,
                            format!(
                                "`{}` has no method `{}`",
                                self.types.name(receiver.ty),
                                name.name
                            ),
                        )
                        .with_primary(name.span, "no such method")
                        .with_note(
                            "a map has: len, keys, values, remove; read with \
                             `m[key]`, which yields an optional, and write with \
                             `m[key] = value`",
                        ),
                    );
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
            };
            if !args.is_empty() {
                self.arity_error(&name.name, args.len(), 0, span, None);
            }
            return hir::Expr { kind, ty, span };
        }

        if self.types.slice_elem(receiver.ty).is_some() {
            return self
                .slice_method(base, receiver, name, args, span)
                .unwrap_or(hir::Expr {
                    kind: ExprKind::Error,
                    ty: TyId::ERROR,
                    span,
                });
        }

        if let TyKind::Dyn(tr) = *self.types.kind(receiver.ty) {
            return self.virtual_call(base, receiver, tr, name, args, span);
        }

        // A method on a type parameter. Only its bounds say what it can do —
        // that is the whole job of a bound.
        if let TyKind::Param { index, .. } = *self.types.kind(receiver.ty) {
            let def = self.generic_defs.get(index as usize).cloned();
            let Some(def) = def else {
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            };
            let found = def
                .bounds
                .iter()
                .find(|tr| self.types.trait_def(**tr).method(&name.name).is_some())
                .copied();
            match found {
                Some(tr) => return self.virtual_call(base, receiver, tr, name, args, span),
                None => {
                    let mut d = Diagnostic::error(
                        codes::E0205,
                        format!("`{}` has no method `{}`", def.name, name.name),
                    )
                    .with_primary(name.span, "no such method")
                    .with_secondary(def.span, "this parameter is declared here");
                    d = if def.bounds.is_empty() {
                        d.with_note(format!(
                            "`{}` has no bounds, so nothing is known about it; write `{}: Trait`",
                            def.name, def.name
                        ))
                    } else {
                        let names: Vec<String> = def
                            .bounds
                            .iter()
                            .map(|b| self.types.trait_def(*b).name.clone())
                            .collect();
                        d.with_note(format!(
                            "`{}` is bounded by {}, and none of those declares `{}`",
                            def.name,
                            names.join(", "),
                            name.name
                        ))
                    };
                    self.diags.push(d);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
            }
        }

        let Some(ti) = self.type_index_of(receiver.ty) else {
            let found = self.types.with_article(receiver.ty);
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` has no methods", self.types.name(receiver.ty)),
                )
                .with_primary(receiver.span, format!("this is {}", found))
                .with_secondary(name.span, "no method can be called here"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        // A field holding a function is callable through its name. Kite has no
        // method/field distinction to preserve here — a field of function type
        // *is* what a handler or a callback looks like, and `(r.handle)(x)`
        // would be punctuation standing in for nothing.
        if self.resolved.method_on(ti, &name.name).is_none() {
            if let TyKind::Struct(sid) = *self.types.kind(receiver.ty) {
                let field = self
                    .types
                    .struct_def(sid)
                    .field(&name.name)
                    .map(|(i, f)| (i as u32, f.ty));
                if let Some((index, field_ty)) = field {
                    if let TyKind::Fn { params, ret } = self.types.kind(field_ty).clone() {
                        self.check_field_visible(sid, index as usize, name.span);
                        let callee = hir::Expr {
                            kind: ExprKind::FieldGet { base: Box::new(receiver), index },
                            ty: field_ty,
                            span: name.span,
                        };
                        if args.len() != params.len() {
                            self.arity_error(&name.name, args.len(), params.len(), span, None);
                        }
                        let mut hargs = Vec::with_capacity(args.len());
                        for (i, a) in args.iter().enumerate() {
                            let want = params.get(i).copied();
                            let e = self.expr(a, want);
                            let e = self.coerce(e, want);
                            if let Some(w) = want {
                                self.expect_ty(e.ty, w, e.span, None);
                            }
                            hargs.push(e);
                        }
                        return hir::Expr {
                            kind: ExprKind::CallClosure { callee: Box::new(callee), args: hargs },
                            ty: ret,
                            span,
                        };
                    }
                }
            }
        }

        let Some(fn_index) = self.resolved.method_on(ti, &name.name) else {
            let type_name = self.resolved.type_decl(ti).name.clone();
            let mut d = Diagnostic::error(
                codes::E0205,
                format!("`{}` has no method `{}`", type_name, name.name),
            )
            .with_primary(name.span, "no such method");

            // A field of the same name is the likely intent.
            if let TyKind::Struct(sid) = *self.types.kind(receiver.ty) {
                if self.types.struct_def(sid).field(&name.name).is_some() {
                    d = d.with_note(format!(
                        "`{}` is a field, not a method; write it without `()`",
                        name.name
                    ));
                }
            }
            let methods: Vec<String> = self
                .resolved
                .methods_of(ti)
                .iter()
                .map(|i| self.resolved.fns[*i as usize].name.clone())
                .collect();
            if !methods.is_empty() {
                d = d.with_note(format!("`{}` has: {}", type_name, methods.join(", ")));
            }
            self.diags.push(d);
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        let owner = self.resolved.fns[fn_index as usize]
            .owner
            .expect("a method has an owner");
        if !owner.takes_self {
            let type_name = self.resolved.type_decl(ti).name.clone();
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` is an associated function, not a method", name.name),
                )
                .with_primary(name.span, "takes no `self`")
                .with_note(format!("call it as `{}.{}(…)`", type_name, name.name)),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        // The other half of the `var self` contract. A method that may modify
        // its receiver can only be reached through a binding that may change,
        // so the modification is predictable from the call site: `c.bump()`
        // changing `c` is visible in `var c`, and impossible in `let c`.
        if owner.var_self {
            self.require_mutable_receiver(base, &name.name);
        }

        self.check_member_visible(
            self.resolved.module_of_fn(fn_index).to_string(),
            self.resolved.fns[fn_index as usize].is_pub || owner.trait_index.is_some(),
            "method",
            &name.name,
            name.span,
            self.sigs[fn_index as usize].name_span,
        );

        // A method on a generic type is written once against the parameters
        // and specialised per receiver: its block's arguments come off the
        // receiver's own type. Its own parameters, if it has any, are solved
        // from its arguments exactly as a generic function's are.
        let generics = self.sigs[fn_index as usize].generics.clone();
        let block = self.block_generic_count(owner.impl_index);
        let mut subst: Vec<Option<TyId>> = vec![None; generics.len()];
        for (slot, t) in subst.iter_mut().zip(self.receiver_args(receiver.ty)).take(block) {
            *slot = Some(t);
        }
        let raw_params = self.sigs[fn_index as usize].params.clone();
        let raw_ret = self.sigs[fn_index as usize].ret;
        let decl_span = self.sigs[fn_index as usize].name_span;
        self.seed_from_expected(raw_ret, expected, &generics, &mut subst, span);

        if args.len() != raw_params.len() {
            self.arity_error(&name.name, args.len(), raw_params.len(), span, Some(decl_span));
        }

        // The receiver becomes the first argument, which is exactly how `self`
        // is stored: local 0.
        let receiver_ty = receiver.ty;
        let mut hargs = vec![receiver];
        hargs.extend(self.check_args(&raw_params, &generics, &mut subst, args, Some(decl_span)));
        let targs = self.finish_subst(&generics, &subst, span);
        // A bound on the block holds for the receiver as for anything else.
        // One the declaration itself carries was checked where the value was
        // built, so it is not reported a second time here.
        let owned = self.block_bounds_to_check(ti, &generics, block);
        self.check_bounds(&owned, &targs, span);
        self.note_generic_call(fn_index, &targs, span);
        let ret = self.apply_subst(raw_ret, &subst);
        let sig_params: Vec<TyId> =
            raw_params.iter().map(|p| self.apply_subst(*p, &subst)).collect();

        // What an editor says when the method name is hovered. Recorded here
        // because this is the only place that knows: finding a method needs
        // the receiver's type, so the resolver never had one to record.
        //
        // The *specialised* signature, so a `Cache<str, int>` says `str` where
        // the source says `K` — which is what the receiver in hand actually
        // has.
        let shown_params: Vec<String> =
            sig_params.iter().map(|p| self.types.name(*p)).collect();
        let shown = if ret == TyId::UNIT {
            format!(
                "fn {}.{}({})",
                self.types.name(receiver_ty),
                name.name,
                shown_params.join(", ")
            )
        } else {
            format!(
                "fn {}.{}({}) -> {}",
                self.types.name(receiver_ty),
                name.name,
                shown_params.join(", "),
                self.types.name(ret)
            )
        };
        self.solved.methods.push((name.span, shown));

        hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(fn_index), args: hargs, targs },
            ty: ret,
            span,
        }
    }

    /// The module declaring a struct, which is where its private fields may
    /// be reached. `None` for one the compiler declared, such as a task.
    fn module_of_struct(&self, sid: kite_hir::StructId) -> Option<String> {
        let template = self.types.struct_template_of(sid).unwrap_or(sid);
        let i = self
            .type_ids
            .iter()
            .position(|t| matches!(t, Some(TypeTarget::Struct(s)) if *s == template))?;
        Some(self.resolved.module_of_item(self.resolved.types[i].decl_index).to_string())
    }

    /// A field reached from outside its struct's module must be `pub`
    /// (§4.3). Reported at the name; false when it was not visible.
    fn check_field_visible(&mut self, sid: kite_hir::StructId, index: usize, span: Span) -> bool {
        let Some(module) = self.module_of_struct(sid) else { return true };
        let f = &self.types.struct_def(sid).fields[index];
        let (is_pub, name, decl) = (f.is_pub, f.name.clone(), f.span);
        self.check_member_visible(module, is_pub, "field", &name, span, decl)
    }

    /// A struct literal outside the struct's module needs every field to be
    /// `pub`: a private one cannot be named, and a literal cannot leave one
    /// out.
    fn check_constructible(&mut self, sid: kite_hir::StructId, span: Span) {
        let Some(module) = self.module_of_struct(sid) else { return };
        if module == self.module {
            return;
        }
        let private: Vec<(String, Span)> = self
            .types
            .struct_def(sid)
            .fields
            .iter()
            .filter(|f| !f.is_pub)
            .map(|f| (f.name.clone(), f.span))
            .collect();
        let Some((_, first)) = private.first().cloned() else { return };
        let name = self.types.struct_def(sid).name.clone();
        let fields: Vec<String> = private.iter().map(|(n, _)| format!("`{}`", n)).collect();
        self.diags.push(
            Diagnostic::error(
                codes::E0401,
                format!("`{}` cannot be built outside module `{}`", name, module),
            )
            .with_primary(span, "not visible here")
            .with_secondary(first, "this field is not marked `pub`")
            .with_note(format!(
                "{} {} private, and a literal — even one starting from `..` — has to \
                 give every field",
                fields.join(", "),
                if fields.len() == 1 { "is" } else { "are" }
            ))
            .with_note(format!(
                "build one with a function module `{}` exports, which is what a private \
                 field is asking for",
                module
            )),
        );
    }

    /// The bounds a generic struct or enum declares, held against the type
    /// arguments of a value being built.
    fn check_type_bounds(&mut self, ti: u32, ty: TyId, span: Span) {
        let Some(defs) = self.type_generics.get(ti as usize).cloned() else { return };
        if defs.iter().all(|g| g.bounds.is_empty()) {
            return;
        }
        let args = self.receiver_args(ty);
        self.check_bounds(&defs, &args, span);
    }

    /// How many of a method's type parameters its `impl` block declares —
    /// the ones a receiver's own type supplies. The rest are the method's.
    fn block_generic_count(&self, impl_index: usize) -> usize {
        self.impl_generics.get(&impl_index).map_or(0, |g| g.len())
    }

    /// A method's parameters with the bounds still to be checked at a call:
    /// all of the method's own, and the block's except those the type's
    /// declaration already demands of whatever builds a value of it.
    fn block_bounds_to_check(&self, ti: u32, generics: &[GenericDef], block: usize) -> Vec<GenericDef> {
        let declared = self.type_generics.get(ti as usize);
        generics
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let mut g = g.clone();
                if i < block {
                    if let Some(d) = declared.and_then(|d| d.get(i)) {
                        g.bounds.retain(|b| !d.bounds.contains(b));
                    }
                }
                g
            })
            .collect()
    }

    /// Fill in type parameters from the type a call's result is used as,
    /// where it says more than the arguments will.
    ///
    /// A trial: any disagreement is reported by the argument or the use that
    /// has it, where it can be explained, rather than here.
    fn seed_from_expected(
        &mut self,
        ret: TyId,
        expected: Option<TyId>,
        generics: &[GenericDef],
        subst: &mut Vec<Option<TyId>>,
        span: Span,
    ) {
        let Some(want) = expected else { return };
        if subst.iter().all(|s| s.is_some()) {
            return;
        }
        let mut scratch = DiagBag::new();
        std::mem::swap(self.diags, &mut scratch);
        self.unify(ret, want, generics, subst, span);
        // A `T` is acceptable where an `Option<T>` is wanted, so an optional
        // context says what the value inside it is.
        if let TyKind::Optional(inner) = *self.types.kind(want) {
            if !matches!(self.types.kind(ret), TyKind::Optional(_)) {
                self.unify(ret, inner, generics, subst, span);
            }
        }
        std::mem::swap(self.diags, &mut scratch);
    }

    /// Reject reaching into another module for a member it did not mark
    /// `pub` — a field, a method, an associated function.
    ///
    /// The same two levels of visibility the resolver enforces for top-level
    /// names (§4.3), applied where a member is found, which is here: which
    /// field or method a `.name` means depends on the type of what is to its
    /// left, and only the checker knows that.
    fn check_member_visible(
        &mut self,
        decl_module: String,
        is_pub: bool,
        what: &str,
        name: &str,
        span: Span,
        decl: Span,
    ) -> bool {
        if is_pub || decl_module == self.module {
            return true;
        }
        // A `let (a, b) = …` initialiser is checked twice, once to see which
        // kind of binding it is; one mistake is still one diagnostic.
        let already = self
            .diags
            .iter()
            .any(|d| d.code == Some(codes::E0401) && d.primary_span() == Some(span));
        if already {
            return false;
        }
        self.diags.push(
            Diagnostic::error(
                codes::E0401,
                format!("{} `{}` is private to module `{}`", what, name, decl_module),
            )
            .with_primary(span, "not visible here")
            .with_secondary(decl, format!("this {} is not marked `pub`", what))
            .with_note(
                "unmarked members are visible only within the module that declares their \
                 type; write `pub` to export one",
            ),
        );
        false
    }

    /// The type arguments a receiver's own type was specialised with, or none
    /// when it is an ordinary declaration.
    fn receiver_args(&self, ty: TyId) -> Vec<TyId> {
        match *self.types.kind(ty) {
            TyKind::Struct(s) => {
                self.types.struct_origin_of(s).map(|(_, a)| a).unwrap_or_default()
            }
            TyKind::Enum(e) => self.types.enum_origin_of(e).map(|(_, a)| a).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// `Rect.square(2.0)` — an associated function, called through the type.
    fn associated_call(
        &mut self,
        ti: u32,
        p: &ast::Path,
        args: &[ast::Expr],
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        let name = p.last().name.clone();
        self.associated_call_named(ti, &name, p.span, args, span, expected)
    }

    /// `T.is(err)` and `T.as(err)`.
    ///
    /// Both read the tag the conversion into an `error` stored beside the
    /// message. `is` compares it; `as` compares it and, where it matches,
    /// hands back the value — so the cast is only ever reached on a path the
    /// comparison has already proved.
    fn error_downcast(
        &mut self,
        ti: u32,
        type_name: &str,
        method_name: &str,
        args: &[ast::Expr],
        p_span: Span,
        span: Span,
    ) -> hir::Expr {
        let Some(target) = self.type_ids[ti as usize] else {
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` has no associated function `{}`", type_name, method_name),
                )
                .with_primary(p_span, "no such function"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };
        let ty = match target {
            TypeTarget::Struct(s) => self.types.struct_ty(s),
            TypeTarget::Enum(e) => self.types.enum_ty(e),
            // A trait or an alias has no run-time identity of its own to
            // compare a tag against; only a concrete type does.
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`{}` is not a concrete type", type_name),
                    )
                    .with_primary(p_span, "no run-time identity to test against")
                    .with_note(
                        "`is` and `as` ask which concrete type an error carries; name a \
                         struct or an enum",
                    ),
                );
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            }
        };
        let Some(tag) = self.type_tag_of(ty) else {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        if args.len() != 1 {
            let full = format!("{}.{}", type_name, method_name);
            self.arity_error(&full, args.len(), 1, span, None);
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        let err = self.expr(&args[0], Some(TyId::ERR));
        if err.ty != TyId::ERR && !self.types.is_poisoned(err.ty) {
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}.{}` takes an `error`", type_name, method_name),
                )
                .with_primary(err.span, format!("this is {}", self.types.with_article(err.ty)))
                .with_note("it asks which failure an error was"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        // A nil error carries tag zero and no type's tag is zero, so this is
        // false rather than a trap — which is what lets it be written without
        // a nil test in front of it.
        let matches = |this: &Self, e: hir::Expr| hir::Expr {
            kind: ExprKind::Binary {
                op: hir::BinOp::EqInt,
                lhs: Box::new(hir::Expr {
                    kind: ExprKind::ErrorTag { base: Box::new(e) },
                    ty: TyId::INT,
                    span,
                }),
                rhs: Box::new(hir::Expr {
                    kind: ExprKind::Int(tag as i64),
                    ty: TyId::INT,
                    span,
                }),
            },
            ty: TyId::BOOL,
            span: {
                let _ = this;
                span
            },
        };

        if method_name == "is" {
            return matches(self, err);
        }

        // `as`: one operation, so the tag test and the read see the same
        // error. An expression cannot introduce the local that would otherwise
        // guarantee it, and re-checking the argument would evaluate it twice.
        let optional = self.types.optional_of(ty);
        hir::Expr {
            kind: ExprKind::ErrorAs { base: Box::new(err), tag },
            ty: optional,
            span,
        }
    }

    /// `Color.Blue` in value position, where the resolver found the type but
    /// not the member.
    ///
    /// The resolver cannot tell `Color.Blue` from `Rect.square`: a dotted name
    /// whose head is a type and whose tail is not one of its variants might be
    /// an associated function, which only the checker can look up. So it hands
    /// over the type, and this is where the tail has to be accounted for. It
    /// used to fall through to a field read of the type's name, which had no
    /// resolution of its own and so came back as the error type with nothing
    /// said: `let c = Color.Blue` compiled, and `c == Color.Red` printed `()`.
    fn type_member_value(
        &mut self,
        ti: u32,
        base: &ast::Expr,
        name: &ast::Ident,
        span: Span,
    ) -> hir::Expr {
        let owner = expr_text(base);
        let declared = self.resolved.type_decl(ti).name.clone();
        let simple = declared.rsplit('.').next().unwrap_or(&declared).to_string();
        if let Some(fn_index) = self.resolved.method_on(ti, &name.name) {
            let takes_self = self.resolved.fns[fn_index as usize]
                .owner
                .is_some_and(|o| o.takes_self);
            let kind = if takes_self { "a method" } else { "an associated function" };
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}.{}` is {}, not a value", owner, name.name, kind),
                )
                .with_primary(span, "named here without being called")
                .with_note(if takes_self {
                    format!("call it on a value: `value.{}(…)`", name.name)
                } else {
                    format!(
                        "call it — `{}.{}(…)` — or, to pass it on, wrap the call in a closure",
                        owner, name.name
                    )
                }),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        // `json.Json` — the whole path is the type, reached through its
        // module, and a type is no more a value spelled this way than bare.
        if simple == name.name && owner != simple {
            let full = format!("{}.{}", owner, name.name);
            let build = match self.type_ids[ti as usize] {
                Some(TypeTarget::Enum(_)) => format!("to make one, name a variant: `{}.…`", full),
                _ => format!("to build one, write a struct literal such as `{}{{ … }}`", full),
            };
            self.diags.push(
                Diagnostic::error(codes::E0200, format!("`{}` is a type, not a value", full))
                    .with_primary(span, "a type name cannot stand alone here")
                    .with_note(build),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        if !self.no_such_variant(ti, &owner, &name.name, span) {
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` has no associated function `{}`", owner, name.name),
                )
                .with_primary(name.span, "no such function"),
            );
        }
        self.lit(ExprKind::Error, TyId::ERROR, span)
    }

    /// Report `Enum.Name` naming a variant the enum does not have, worded as
    /// the resolver words the same mistake in a pattern: one mistake, one
    /// message, wherever it is written. Answers whether the type was one that
    /// has variants to be missing — an enum, or an alias, which reaches none.
    fn no_such_variant(&mut self, ti: u32, owner: &str, member: &str, span: Span) -> bool {
        let decl_span = self.resolved.type_decl(ti).span;
        let d = match self.type_ids[ti as usize] {
            Some(TypeTarget::Enum(eid)) => {
                let names: Vec<String> =
                    self.types.enum_def(eid).variants.iter().map(|v| v.name.clone()).collect();
                let mut d = Diagnostic::error(
                    codes::E0111,
                    format!("`{}` has no variant `{}`", owner, member),
                )
                .with_primary(span, "no such variant")
                .with_secondary(decl_span, "declared here");
                if !names.is_empty() {
                    d = d.with_note(format!("`{}` has: {}", owner, names.join(", ")));
                }
                d
            }
            Some(TypeTarget::Alias(_)) => Diagnostic::error(
                codes::E0111,
                format!("`{}` is a type alias, not an enum", owner),
            )
            .with_primary(span, "no such variant")
            .with_secondary(decl_span, "declared here"),
            _ => return false,
        };
        self.diags.push(d);
        true
    }

    fn associated_call_named(
        &mut self,
        ti: u32,
        method_name: &str,
        path_span: Span,
        args: &[ast::Expr],
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        let type_name = self.resolved.type_decl(ti).name.clone();
        let method_name = method_name.to_string();
        let p_span = path_span;

        // `NotFound.is(err)` and `NotFound.as(err)` — asking an error which
        // failure it was.
        //
        // **The type names itself**, which is how `Decode` already works
        // (`User.decode(doc)`) and for the same reason: §11 has no turbofish,
        // so `errors.is<T>(err)` — which §7.6 used to promise — has nowhere to
        // write its type argument. Naming the type at the front says the same
        // thing in a place the language can spell.
        // A type that declares its own `is` or `as` keeps it: the built-in
        // downcast is what a type gets when it has said nothing.
        if (method_name == "is" || method_name == "as")
            && self.resolved.method_on(ti, &method_name).is_none()
        {
            return self.error_downcast(ti, &type_name, &method_name, args, p_span, span);
        }

        let Some(fn_index) = self.resolved.method_on(ti, &method_name) else {
            // `Shape.Square(1.0)` on an enum is a variant that is not there
            // far more often than a function that is not, and it is reported
            // as the same line without its arguments is.
            let written = self.text(p_span);
            let owner = match written.rsplit_once('.') {
                Some((owner, _)) => owner.trim().to_string(),
                None => type_name.clone(),
            };
            if !self.no_such_variant(ti, &owner, &method_name, p_span) {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`{}` has no associated function `{}`", type_name, method_name),
                    )
                    .with_primary(p_span, "no such function"),
                );
            }
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        let owner = self.resolved.fns[fn_index as usize]
            .owner
            .expect("a method has an owner");
        if owner.takes_self {
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` is a method, not an associated function", method_name),
                )
                .with_primary(p_span, "takes `self`")
                .with_note(format!("call it on a value: `value.{}(…)`", method_name)),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        self.check_member_visible(
            self.resolved.module_of_fn(fn_index).to_string(),
            self.resolved.fns[fn_index as usize].is_pub || owner.trait_index.is_some(),
            "associated function",
            &method_name,
            p_span,
            self.sigs[fn_index as usize].name_span,
        );

        // An associated function on a generic type has no receiver to take
        // arguments from, so they come from the type the result is used as —
        // `let s: Stack<int> = Stack.empty()` — or failing that from its
        // arguments, as any generic call's do: `Stack.of(5)` is a
        // `Stack<int>`. Its own parameters, if it has any, come from the
        // arguments too.
        let generics = self.sigs[fn_index as usize].generics.clone();
        let block = self.block_generic_count(owner.impl_index);
        let mut subst: Vec<Option<TyId>> = vec![None; generics.len()];

        let raw_params = self.sigs[fn_index as usize].params.clone();
        let raw_ret = self.sigs[fn_index as usize].ret;
        let decl_span = self.sigs[fn_index as usize].name_span;
        self.seed_from_expected(raw_ret, expected, &generics, &mut subst, span);

        if args.len() != raw_params.len() {
            let full = format!("{}.{}", type_name, method_name);
            self.arity_error(&full, args.len(), raw_params.len(), span, Some(decl_span));
        }

        let hargs = self.check_args(&raw_params, &generics, &mut subst, args, Some(decl_span));
        if subst.iter().take(block).any(|s| s.is_none()) {
            let names: Vec<&str> = generics.iter().take(block).map(|g| g.name.as_str()).collect();
            self.diags.push(
                Diagnostic::error(
                    codes::E0209,
                    format!("cannot infer {} for `{}`", names.join(", "), type_name),
                )
                .with_primary(span, "nothing here says what this returns")
                .with_note(format!("annotate the binding: `let x: {}<...> = ...`", type_name)),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }
        let targs = self.finish_subst(&generics, &subst, span);
        self.check_bounds(&generics, &targs, span);
        self.note_generic_call(fn_index, &targs, span);
        let ret = self.apply_subst(raw_ret, &subst);

        hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(fn_index), args: hargs, targs },
            ty: ret,
            span,
        }
    }

    /// The `resolved.types` index for a nominal type, if it has one.
    /// Work out a generic struct's type arguments and return the specialised
    /// definition.
    fn solve_struct_args(
        &mut self,
        template: kite_hir::StructId,
        lit: &ast::StructLit,
        expected: Option<TyId>,
    ) -> Option<kite_hir::StructId> {
        let count = self.types.struct_def(template).generic_count;

        // An annotation settles it outright: `let p: Pair<int, str> = Pair{..}`.
        if let Some(want) = expected {
            if let TyKind::Struct(s) = *self.types.kind(want) {
                if self.instance_of(s) == Some(template) {
                    return Some(s);
                }
            }
        }

        // So does a spread base. `Control{..base, enabled: false}` is the same
        // type as `base`, whatever the listed fields happen to mention — and
        // the fields alone routinely mention nothing: a functional update that
        // changes only the non-generic part of a value would otherwise be
        // unwritable inside the generic function that holds it, which is the
        // one place it is most needed.
        //
        // Checked before the field pass rather than folded into it, because it
        // is an answer rather than a constraint: the base's type names every
        // argument at once.
        // The base is checked again for real once the specialisation is known,
        // so this look is a trial like the field pass below: its diagnostics
        // are discarded rather than reported twice.
        if let Some(base) = &lit.base {
            let trial = self.begin_trial();
            let seen = self.expr(base, None);
            self.end_trial(trial);
            if let TyKind::Struct(s) = *self.types.kind(seen.ty) {
                if self.instance_of(s) == Some(template) {
                    return Some(s);
                }
            }
        }

        let generics: Vec<GenericDef> = (0..count)
            .map(|i| {
                let ty = self.types.param_ty(i as u32, "");
                GenericDef { name: format!("#{}", i), ty, bounds: Vec::new(), span: lit.span }
            })
            .collect();
        let mut subst: Vec<Option<TyId>> = vec![None; count];
        // This pass is a trial: a field like `children: []` cannot be checked
        // until the parameter is known, and complaining about it here would
        // report a problem that the real check — which runs against the
        // specialisation — does not have. Its diagnostics are discarded.
        let trial = self.begin_trial();
        for init in &lit.fields {
            let Some(declared) =
                self.types.struct_def(template).field(&init.name.name).map(|(_, f)| f.ty)
            else {
                continue;
            };
            let v = self.expr(&init.value, None);
            self.unify(declared, v.ty, &generics, &mut subst, v.span);
        }
        self.end_trial(trial);

        let mut args = Vec::with_capacity(count);
        for (i, s) in subst.iter().enumerate() {
            match s {
                Some(t) => args.push(*t),
                None => {
                    let name = self.types.struct_def(template).name.clone();
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0209,
                            format!("cannot infer type argument {} of `{}`", i + 1, name),
                        )
                        .with_primary(lit.span, "no field pins this parameter down")
                        .with_note(
                            "annotate the binding — `let x: Name<int> = ...` — so the type \
                             arguments are stated where they cannot be worked out",
                        ),
                    );
                    return None;
                }
            }
        }
        Some(self.types.instantiate_struct(template, &args))
    }

    /// The template a specialised struct came from, if it is one.
    fn instance_of(&self, id: kite_hir::StructId) -> Option<kite_hir::StructId> {
        self.types.struct_template_of(id)
    }

    /// Resolve a surface type with the module's declarations in view, which is
    /// what lets an annotation name a struct, an enum, or a `dyn Trait`.
    fn resolve_type(&mut self, t: &ast::Type) -> TyId {
        resolve_named_ty(t, self.resolved, &self.module, self.type_ids, &self.generics, self.types, self.diags)
    }

    /// The declared-type index for a `dyn Trait`, used to consult impls.
    fn trait_index_of(&self, tr: hir::TraitId) -> Option<u32> {
        self.type_ids
            .iter()
            .position(|t| matches!(t, Some(TypeTarget::Trait(a)) if *a == tr))
            .map(|i| i as u32)
    }

    /// Whether a concrete value may stand where a trait object is wanted.
    fn coerces_to_dyn(&mut self, found: TyId, expected: TyId) -> bool {
        let TyKind::Dyn(tr) = *self.types.kind(expected) else { return false };
        self.type_implements(found, tr)
    }

    /// Whether a type implements a trait.
    ///
    /// A type parameter does exactly when its bounds say so — inside
    /// `fn outer<T: Show>`, `T` is a `Show` and may be handed to anything
    /// that asks for one. A generic type implements a trait through a block
    /// that may itself be bounded: after `impl<T: Show> Named for Box<T>`, a
    /// `Box<int>` is not `Named` at all, and a call that treated it as one
    /// reached a body calling `show` on an `int`.
    fn type_implements(&mut self, ty: TyId, tr: hir::TraitId) -> bool {
        if let TyKind::Param { index, .. } = *self.types.kind(ty) {
            return self.generic_defs.get(index as usize).is_some_and(|d| d.bounds.contains(&tr));
        }
        let (Some(ti), Some(tri)) = (self.type_index_of(ty), self.trait_index_of(tr)) else {
            return false;
        };
        if !self.resolved.implements(ti, tri) {
            return false;
        }
        let Some(defs) = self
            .trait_impls
            .get(&(ti, tri))
            .and_then(|block| self.impl_generics.get(block))
            .cloned()
        else {
            return true;
        };
        let args = self.receiver_args(ty);
        for (g, a) in defs.iter().zip(args) {
            for b in &g.bounds {
                let held = if self.is_share_bound(*b) {
                    self.share_given_params(a)
                } else {
                    self.type_implements(a, *b)
                };
                if !held {
                    return false;
                }
            }
        }
        true
    }

    /// Whether a trait is `Share`, the one bound nobody implements.
    ///
    /// The prelude is a module, so its trait is `prelude.Share` — and a
    /// program that declares its own `Share` gets the same treatment, exactly
    /// as it does for `Display`.
    fn is_share_bound(&self, tr: hir::TraitId) -> bool {
        last_segment(&self.types.trait_def(tr).name) == "Share"
    }

    /// Whether a type may cross a task boundary, where it may mention the
    /// enclosing function's type parameters.
    ///
    /// A parameter bounded by `Share` is as good as any type that is; one
    /// that is not might turn out to be anything, and so is taken to be the
    /// worst case. Standing each in for a representative type lets the one
    /// structural rule in `Types::is_share` answer, rather than a second copy
    /// of it that knows about parameters.
    fn share_given_params(&mut self, ty: TyId) -> bool {
        let mut params = Vec::new();
        params_in(self.types, ty, &mut params);
        params.retain(|p| *p != kite_hir::ty::SELF_INDEX);
        let Some(max) = params.iter().max().copied() else {
            return self.types.is_share(ty);
        };
        let stand_ins: Vec<TyId> = (0..=max)
            .map(|i| {
                let bounded = self.generic_defs.get(i as usize).is_some_and(|d| {
                    d.bounds.iter().any(|b| last_segment(&self.types.trait_def(*b).name) == "Share")
                });
                if bounded {
                    TyId::INT
                } else {
                    TyId::JS_VALUE
                }
            })
            .collect();
        let concrete = self.types.substitute(ty, &stand_ins);
        self.types.is_share(concrete)
    }

    fn type_index_of(&self, ty: TyId) -> Option<u32> {
        // A specialisation has no declaration of its own; its methods are
        // declared on the template it was made from.
        let target = match *self.types.kind(ty) {
            TyKind::Struct(s) => {
                TypeTarget::Struct(self.types.struct_template_of(s).unwrap_or(s))
            }
            TyKind::Enum(e) => TypeTarget::Enum(self.types.enum_template_of(e).unwrap_or(e)),
            _ => return None,
        };
        self.type_ids
            .iter()
            .position(|t| match (t, target) {
                (Some(TypeTarget::Struct(a)), TypeTarget::Struct(b)) => *a == b,
                (Some(TypeTarget::Enum(a)), TypeTarget::Enum(b)) => *a == b,
                _ => false,
            })
            .map(|i| i as u32)
    }

    // ---- error handling ---------------------------------------------------

    /// `let (v, err) = f()`. The value becomes Tainted and the error
    /// Unchecked; neither is usable until the error is tested.
    /// `let (a, b) = pair` for an ordinary tuple.
    ///
    /// Called with the initialiser already checked, and found to be a tuple.
    fn let_tuple(
        &mut self,
        elems: &[ast::BindElem],
        value: hir::Expr,
        span: Span,
    ) -> Option<(hir::Stmt, Flow)> {
        let TyKind::Tuple(parts) = self.types.kind(value.ty).clone() else {
            return None;
        };
        if parts.len() != elems.len() {
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!(
                        "expected {} binding{}, found {}",
                        parts.len(),
                        if parts.len() == 1 { "" } else { "s" },
                        elems.len()
                    ),
                )
                .with_primary(span, format!("this is {}", self.types.with_article(value.ty)))
                .with_note("a tuple binding names every element, or `_` for one to drop"),
            );
            return None;
        }

        // The tuple is held in a local of its own, so an initialiser with side
        // effects runs once however many elements are taken from it.
        let holder = hir::LocalId(self.synthetic_local("tuple", value.ty, span));
        let mut stmts = vec![hir::Stmt::Let {
            local: holder,
            init: Some(value),
            span,
        }];
        for (i, e) in elems.iter().enumerate() {
            let ast::BindElem::Name(name) = e else { continue };
            let Some(local) = self.resolved.lookup_binding(name.span) else { continue };
            self.locals[local as usize].ty = parts[i];
            self.init[local as usize] = Init::Assigned;
            self.taint[local as usize] = Taint::Clean;
            stmts.push(hir::Stmt::Let {
                local: hir::LocalId(local),
                init: Some(hir::Expr {
                    kind: ExprKind::FieldGet {
                        base: Box::new(hir::Expr {
                            kind: ExprKind::Local(holder),
                            ty: self.locals[holder.index()].ty,
                            span,
                        }),
                        index: i as u32,
                    },
                    ty: parts[i],
                    span: name.span,
                }),
                span: name.span,
            });
        }
        Some((hir::Stmt::Block(hir::Block { stmts }), Flow::Falls))
    }

    fn let_pair(
        &mut self,
        l: &ast::LetStmt,
        elems: &[ast::BindElem],
        span: Span,
    ) -> Option<(hir::Stmt, Flow)> {
        // A tuple binding is either a fallible result being split into its
        // value and its error, or an ordinary tuple being taken apart. Which
        // one it is comes from the initialiser's type, so that is checked
        // first — once, for both paths: checking it again on the second
        // reported every mistake in it twice.
        let annotated = l.ty.as_ref().map(|t| self.resolve_type(t));
        let checked = l.init.as_ref().map(|init| self.expr(init, annotated));
        let checked = match checked {
            Some(value) if matches!(self.types.kind(value.ty), TyKind::Tuple(_)) => {
                return self.let_tuple(elems, value, span);
            }
            other => other,
        };

        if elems.len() != 2 {
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("expected 2 bindings, found {}", elems.len()),
                )
                .with_primary(span, "a fallible result has a value and an error")
                .with_note("write `let (value, err) = f()`"),
            );
            return None;
        }

        // Dropping the *error* slot is exactly what Kite forbids.
        if let ast::BindElem::Wildcard(w) = &elems[1] {
            self.diags.push(
                Diagnostic::error(codes::E0302, "an error may not be discarded with `_`")
                    .with_primary(*w, "the error slot cannot be dropped")
                    .with_note(
                        "silently dropping errors is the single most common source of \
                         production failures in languages that permit it; write `check` to \
                         propagate, or test `err != nil` to handle it here",
                    ),
            );
        }

        let Some(call) = checked else {
            self.diags.push(
                Diagnostic::error(codes::E0204, "a tuple binding needs an initialiser")
                    .with_primary(span, "nothing to destructure"),
            );
            return None;
        };
        let Some(inner) = self.types.fallible_value(call.ty) else {
            if !self.types.is_poisoned(call.ty) {
                let found = self.types.with_article(call.ty);
                self.diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        "only a fallible call can be destructured this way",
                    )
                    .with_primary(call.span, format!("this is {}", found))
                    .with_note("`let (v, err) = …` needs a function declared `-> (T, error)`"),
                );
            }
            return None;
        };

        // The pair is evaluated once into a temporary; the two bindings read
        // its slots.
        let value_local = match &elems[0] {
            ast::BindElem::Name(n) => self.resolved.lookup_binding(n.span),
            ast::BindElem::Wildcard(_) => None,
        };
        let error_local = match &elems[1] {
            ast::BindElem::Name(n) => self.resolved.lookup_binding(n.span),
            ast::BindElem::Wildcard(_) => None,
        };

        let pair_local = self.synthetic_local("pair", call.ty, span);

        let mut stmts = vec![hir::Stmt::Let {
            local: hir::LocalId(pair_local),
            init: Some(call),
            span,
        }];

        if let Some(v) = value_local {
            self.locals[v as usize].ty = inner;
            self.init[v as usize] = Init::Assigned;
            self.taint[v as usize] = Taint::Tainted;
            stmts.push(hir::Stmt::Let {
                local: hir::LocalId(v),
                init: Some(hir::Expr {
                    kind: ExprKind::PairValue {
                        base: Box::new(hir::Expr {
                            kind: ExprKind::Local(hir::LocalId(pair_local)),
                            ty: self.types.fallible_of(inner),
                            span,
                        }),
                    },
                    ty: inner,
                    span,
                }),
                span,
            });
        }
        if let Some(e) = error_local {
            self.locals[e as usize].ty = TyId::ERR;
            self.init[e as usize] = Init::Assigned;
            self.taint[e as usize] = Taint::Unchecked;
            let pair_ty = self.types.fallible_of(inner);
            stmts.push(hir::Stmt::Let {
                local: hir::LocalId(e),
                init: Some(hir::Expr {
                    kind: ExprKind::PairError {
                        base: Box::new(hir::Expr {
                            kind: ExprKind::Local(hir::LocalId(pair_local)),
                            ty: pair_ty,
                            span,
                        }),
                    },
                    ty: TyId::ERR,
                    span,
                }),
                span,
            });
        }

        // Record which value each error guards, so testing the error can clean
        // the value.
        if let (Some(v), Some(e)) = (value_local, error_local) {
            self.guards.insert(e, v);
        }

        Some((
            hir::Stmt::Block(hir::Block { stmts }),
            Flow::Falls,
        ))
    }

    /// A closure written as a statement, which can do nothing at all.
    ///
    /// Nobody writes this on purpose. What writes it is a line continuation
    /// that did not continue: a statement carries on to the next line when it
    /// *ends* in an operator ([§2.5]), so an `||` opening a line is not the
    /// tail of the expression above — it is a closure with no parameters,
    /// because `||` where a value is expected is exactly that.
    ///
    /// ```text
    /// let ok = (c >= 48 && c <= 57)
    ///     || (c >= 65 && c <= 90)     // a closure, built and discarded
    /// ```
    ///
    /// The first line is a complete statement, the rest is inert, and the
    /// answer is quietly wrong. After a `return` the same mistake is caught as
    /// unreachable code; after a `let` nothing noticed it at all until a
    /// percent-encoder in this repository's own site encoded the letters.
    ///
    /// `&&` cannot do this — it may not begin an expression, so it is a parse
    /// error — which is why this is about `||` alone.
    ///
    /// [§2.5]: ../../../SPECIFICATION.md#25-semicolon-insertion
    fn inert_closure(&mut self, e: &ast::Expr) {
        let ast::Expr::Closure { span, .. } = e else {
            return;
        };
        self.diags.push(
            Diagnostic::error(codes::E0117, "this closure is built and thrown away")
                .with_primary(*span, "nothing calls it, so nothing in it happens")
                .with_note(
                    "an `||` opening a line is a closure with no parameters, not the \
                     continuation of the line above",
                )
                .with_note(
                    "a statement continues onto the next line when it *ends* in an \
                     operator — put the `||` at the end of the line it continues",
                ),
        );
    }

    /// An error thrown away by leaving a call as a bare statement.
    ///
    /// The taint analysis in [§7.3] is written about *bindings*: `let (v, e) =
    /// f()` marks `e` Unchecked and complains when it goes out of scope. A call
    /// written as a statement binds nothing, so none of that ever ran — and
    /// `dom.set_text(e, "hi")` dropped its error in silence.
    ///
    /// That is Go's first flaw, the one §7.1 opens by naming as the thing this
    /// language fixes, surviving in the one shape the analysis did not cover.
    /// It matters most exactly where Kite is aimed: almost every function in
    /// `std/dom` returns a bare `error`, so on the web the dropped error was
    /// the ordinary case rather than an unusual one.
    ///
    /// Writing `_ = f()` says the same thing on purpose, and is the only way to
    /// get the old behaviour — which is the point, because now it is a decision
    /// somebody made and a reader can see.
    ///
    /// [§7.3]: ../../../SPECIFICATION.md#73-correlated-results-and-taint-analysis
    fn dropped_error(&mut self, expr: &hir::Expr) {
        let pair = self.types.fallible_value(expr.ty).is_some();
        if expr.ty != TyId::ERR && !pair {
            return;
        }
        let what = if pair {
            "this call returns a value and an error, and both are thrown away"
        } else {
            "this call returns an error, and it is thrown away"
        };
        self.diags.push(
            Diagnostic::error(codes::E0302, "error is never checked")
                .with_primary(expr.span, what)
                .with_note(
                    "write `check` to propagate it, or bind it and test it — \
                     `let err = …` then `if err != nil { … }`",
                )
                .with_note("to throw it away on purpose, write `_ = …`"),
        );
    }

    /// `check err` — propagate if the error is not nil.
    ///
    /// Defined as exactly `if err != nil { return _, err }`, or `return err` in
    /// a function that answers with a bare `error`. It occupies its own line
    /// and is greppable, which preserves Go's central virtue: you can scan the
    /// left margin of a function and see every place it can fail.
    ///
    /// **A bare `-> error` return counts as fallible here**, and used not to.
    /// A function that can only fail — every wrapper in `std/dom` is one — had
    /// to write out `if err != nil { return err }` at each step, which is the
    /// boilerplate `check` exists to remove, refused at exactly the functions
    /// with the least else in them.
    fn check_stmt(
        &mut self,
        expr: &ast::Expr,
        span: Span,
        sig: &Signature,
    ) -> Option<(hir::Stmt, Flow)> {
        let bare_error = sig.ret == TyId::ERR;
        if !sig.fallible && !bare_error {
            self.diags.push(
                Diagnostic::error(codes::E0303, "`check` outside a fallible function")
                    .with_primary(span, "this would return an error")
                    .with_secondary(sig.name_span, "declared here")
                    .with_note(
                        "`check` returns the error to the caller, so the enclosing function \
                         must declare `-> (T, error)` or `-> error`",
                    ),
            );
        }

        let e = self.expr(expr, Some(TyId::ERR));
        if !self.types.satisfies(e.ty, TyId::ERR) && !self.types.is_poisoned(e.ty) {
            let found = self.types.with_article(e.ty);
            self.diags.push(
                Diagnostic::error(codes::E0200, "`check` needs an `error`")
                    .with_primary(e.span, format!("this is {}", found)),
            );
        }

        // After `check`, the error is known nil, so its value is readable.
        self.mark_checked(expr);

        let ret = if sig.fallible { sig.ret } else { TyId::ERROR };
        // A function answering with a bare `error` has no value slot to fill,
        // so the propagation is the error itself rather than a pair with a
        // hole in it.
        let propagated = if bare_error {
            self.reread_error(expr, span)
        } else {
            hir::Expr {
                kind: ExprKind::PairNew {
                    value: Box::new(hir::Expr { kind: ExprKind::Nil, ty: TyId::ERROR, span }),
                    error: Box::new(self.reread_error(expr, span)),
                },
                ty: ret,
                span,
            }
        };
        // The propagation is a `return` like any other, so what was deferred
        // runs on the way out. This is §6.3's own example — `check err` above
        // a `defer file.close()` is exactly where the file has to be closed —
        // and it used to leave without running anything.
        let leave = self.returning(hir::Stmt::Return { value: Some(propagated), span }, span);
        Some((
            hir::Stmt::If {
                cond: hir::Expr {
                    kind: ExprKind::Unary {
                        op: hir::UnOp::Not,
                        operand: Box::new(hir::Expr {
                            kind: ExprKind::IsNil { value: Box::new(e) },
                            ty: TyId::BOOL,
                            span,
                        }),
                    },
                    ty: TyId::BOOL,
                    span,
                },
                then: hir::Block { stmts: vec![leave] },
                else_: None,
                span,
            },
            Flow::Falls,
        ))
    }

    /// Whether a type is a closure the host can be handed.
    ///
    /// Every parameter a `JsValue`, at most [`JS_FUNC_MAX_ARITY`] of them, and
    /// a result that is either a `JsValue` or nothing. Anything else has no
    /// form on the other side of the boundary.
    fn is_js_handler(&self, ty: TyId) -> bool {
        let TyKind::Fn { params, ret } = self.types.kind(ty) else {
            return false;
        };
        params.len() <= JS_FUNC_MAX_ARITY
            && params.iter().all(|p| *p == TyId::JS_VALUE)
            && (*ret == TyId::UNIT || *ret == TyId::JS_VALUE)
    }

    /// Re-read the error operand for the propagation branch.
    fn reread_error(&mut self, expr: &ast::Expr, span: Span) -> hir::Expr {
        let saved = self.taint.clone();
        let e = self.expr(expr, Some(TyId::ERR));
        self.taint = saved;
        let _ = span;
        e
    }

    /// Mark an error binding checked, and clean the value it guards.
    ///
    /// `check err` is the direct case. `check errors.wrap(err, "…")` is the
    /// other one the specification shows, and it has to reach the `err` inside:
    /// a wrapper answers nil exactly when what it wrapped was nil, so falling
    /// past the `check` proves the wrapped error nil just as surely, and the
    /// value it guards is readable. Only error-typed arguments count, so
    /// `check open(path)` — where `path` is a `str` — cleans nothing.
    fn mark_checked(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::Paren { inner, .. } => self.mark_checked(inner),
            ast::Expr::Path(p) => {
                let Some(Res::Local(id)) = self.resolved.lookup_use(p.span) else {
                    return;
                };
                if self.taint[id as usize] == Taint::Unchecked {
                    self.taint[id as usize] = Taint::Clean;
                }
                if let Some(&guarded) = self.guards.get(&id) {
                    self.taint[guarded as usize] = Taint::Clean;
                }
            }
            // Only a wrapper that answers nil exactly when what it wrapped was
            // nil can speak for its argument, and the standard library's is
            // the one known to. `check ignore(err)` falls past the `check`
            // whenever `ignore` answers nil, whatever `err` was, so reading
            // through any call made the value readable on the failure path.
            // The callee is known by what it resolves to, not by its name.
            ast::Expr::Call { callee, args, .. } if self.preserves_nil(callee) => {
                for a in args {
                    if self.is_error_operand(a) {
                        self.mark_checked(a);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a callee is `errors.wrap`, which answers nil for nil and
    /// nothing else.
    fn preserves_nil(&self, callee: &ast::Expr) -> bool {
        match self.resolved.lookup_use(callee.span()) {
            Some(Res::Fn(id)) => {
                let f = &self.resolved.fns[id as usize];
                f.name == "errors.wrap" && self.resolved.module_of_item(f.decl_index) == "errors"
            }
            _ => false,
        }
    }

    /// Whether an argument is an error the enclosing `check` speaks for.
    fn is_error_operand(&self, expr: &ast::Expr) -> bool {
        match expr {
            ast::Expr::Paren { inner, .. } => self.is_error_operand(inner),
            ast::Expr::Path(p) => match self.resolved.lookup_use(p.span) {
                Some(Res::Local(id)) => self.locals[id as usize].ty == TyId::ERR,
                _ => false,
            },
            _ => false,
        }
    }

    /// Report every error binding that was never inspected.
    ///
    /// Run once at the end of the body, where all paths have merged, so a
    /// single error is reported once rather than per branch — and, for the
    /// locals declared inside `region`, wherever the state that knows about
    /// them is about to be discarded: at the end of a closure's body, and
    /// after a branch that never reaches the join.
    fn report_unchecked_errors(&mut self, region: Option<Span>) {
        let mut pending: Vec<(String, Span, bool)> = Vec::new();
        for (i, state) in self.taint.iter().enumerate() {
            if *state == Taint::Unchecked
                && !self.locals[i].synthetic
                && region.is_none_or(|r| self.declared_inside(i as u32, r))
                && self.reported_unchecked.insert(i as u32)
            {
                let local = &self.locals[i];
                let pair = self.types.fallible_value(local.ty).is_some();
                pending.push((local.name.clone(), local.span, pair));
            }
        }
        for (name, span, pair) in pending {
            let d = Diagnostic::error(codes::E0302, format!("`{}` is never checked", name));
            let d = if pair {
                d.with_primary(span, "the error in this result goes out of scope uninspected")
            } else {
                d.with_primary(span, "this error goes out of scope uninspected")
            };
            let d = d.with_note(
                "silently dropping errors is the single most common source of \
                 production failures in languages that permit it",
            );
            let d = if pair {
                d.with_note(
                    "take the result apart where it is bound — `let (value, err) = …` — \
                     and then check `err`",
                )
            } else {
                d.with_note(
                    "to propagate, write `check` on its own line; to handle it here, \
                     test `err != nil`",
                )
            };
            self.diags.push(d);
        }
    }

    fn synthetic_local(&mut self, name: &str, ty: TyId, span: Span) -> u32 {
        let id = self.locals.len() as u32;
        self.locals.push(hir::Local {
            name: format!("__{}", name),
            ty,
            mutable: false,
            span,
            synthetic: true,
        });
        self.init.push(Init::Assigned);
        self.taint.push(Taint::Clean);
        id
    }

    // ---- slices -----------------------------------------------------------

    /// `[1, 2, 3]`. Every element must share one type; an empty literal needs
    /// its type from context.
    fn slice_literal(
        &mut self,
        elems: &[ast::Expr],
        expected: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let hint = expected.and_then(|e| self.types.slice_elem(e));

        if elems.is_empty() {
            let Some(elem) = hint else {
                self.diags.push(
                    Diagnostic::error(codes::E0204, "cannot infer the element type")
                        .with_primary(span, "an empty slice has no elements to infer from")
                        .with_note("write the type, as in `let xs: [int] = []`"),
                );
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            };
            let ty = self.types.slice_of(elem);
            return hir::Expr { kind: ExprKind::SliceNew { elems: Vec::new() }, ty, span };
        }

        let mut out = Vec::with_capacity(elems.len());
        let mut elem_ty = hint;
        for e in elems {
            let v = self.expr(e, elem_ty);
            let v = match elem_ty {
                None => {
                    if !self.types.is_poisoned(v.ty) {
                        elem_ty = Some(v.ty);
                    }
                    v
                }
                Some(want) => self.accept(v, want, None),
            };
            out.push(v);
        }

        let elem = elem_ty.unwrap_or(TyId::ERROR);
        let ty = self.types.slice_of(elem);
        hir::Expr { kind: ExprKind::SliceNew { elems: out }, ty, span }
    }

    /// `xs[i]`. Traps on an out-of-range index, because that is a program bug.
    /// `.get()` is the form for when it genuinely is a runtime condition.
    fn index_expr(&mut self, base: &ast::Expr, index: &ast::Expr, span: Span) -> hir::Expr {
        let seq = self.expr(base, None);
        if self.types.is_poisoned(seq.ty) {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        // `xs[a..b]` — a window rather than an element. Handled before the
        // element forms below because the index is not an `int` here and
        // checking it as one would report the range as the mistake.
        if let ast::Expr::Range { start, end, inclusive, span: range_span } = index {
            return self.range_index(seq, start, end, *inclusive, *range_span, span);
        }

        // Map indexing always yields an optional, never a zero value.
        if let TyKind::Map(key_ty, value_ty) = *self.types.kind(seq.ty) {
            let k = self.expr(index, Some(key_ty));
            let k = self.accept(k, key_ty, None);
            self.note_compared(key_ty, k.span);
            let ty = self.types.optional_of(value_ty);
            return hir::Expr {
                kind: ExprKind::MapGet { base: Box::new(seq), key: Box::new(k) },
                ty,
                span,
            };
        }

        let Some(elem) = self.types.slice_elem(seq.ty) else {
            let found = self.types.with_article(seq.ty);
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}` cannot be indexed", self.types.name(seq.ty)),
                )
                .with_primary(seq.span, format!("this is {}", found))
                .with_note("indexing applies to slices and maps"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        let i = self.expr(index, Some(TyId::INT));
        self.expect_ty(i.ty, TyId::INT, i.span, None);

        hir::Expr {
            kind: ExprKind::Index { base: Box::new(seq), index: Box::new(i) },
            ty: elem,
            span,
        }
    }

    /// `xs[a..b]` and `s[a..b]` — a window over a sequence.
    ///
    /// A `str` reaches [`StrKind::Slice`], which already exists on every
    /// backend; a slice reaches [`ExprKind::SliceRange`]. Both clamp, and they
    /// have to agree about that: one syntax with two answers about its edges
    /// would be the kind of drift this language spends its omissions avoiding.
    fn range_index(
        &mut self,
        seq: hir::Expr,
        start: &ast::Expr,
        end: &ast::Expr,
        inclusive: bool,
        range_span: Span,
        span: Span,
    ) -> hir::Expr {
        let start_e = self.expr(start, Some(TyId::INT));
        self.expect_ty(start_e.ty, TyId::INT, start_e.span, None);
        let end_e = self.expr(end, Some(TyId::INT));
        self.expect_ty(end_e.ty, TyId::INT, end_e.span, None);

        // `a..=b` is `a..b + 1`, written out here so that no backend has to
        // know there are two kinds of range. The `+` is the one the user would
        // have written: it traps on overflow in a debug build and wraps in a
        // release one, exactly as §3.1 says every other `+` does. A `b` of
        // `max_int` is the only input that can tell, and it is not a window
        // anyone asks for.
        let end_e = if inclusive {
            let one = hir::Expr { kind: ExprKind::Int(1), ty: TyId::INT, span: range_span };
            let op = if self.release { hir::BinOp::AddIntWrap } else { hir::BinOp::AddInt };
            hir::Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(end_e), rhs: Box::new(one) },
                ty: TyId::INT,
                span: range_span,
            }
        } else {
            end_e
        };

        if seq.ty == TyId::STR {
            return hir::Expr {
                kind: ExprKind::StrOp {
                    op: kite_hir::StrKind::Slice,
                    args: vec![seq, start_e, end_e],
                },
                ty: TyId::STR,
                span,
            };
        }

        if self.types.slice_elem(seq.ty).is_some() {
            let ty = seq.ty;
            return hir::Expr {
                kind: ExprKind::SliceRange {
                    base: Box::new(seq),
                    start: Box::new(start_e),
                    end: Box::new(end_e),
                },
                ty,
                span,
            };
        }

        let found = self.types.with_article(seq.ty);
        self.diags.push(
            Diagnostic::error(
                codes::E0200,
                format!("`{}` cannot be sliced by a range", self.types.name(seq.ty)),
            )
            .with_primary(seq.span, format!("this is {}", found))
            .with_note("a range index applies to a slice or a `str`")
            .with_note(
                "a map is indexed by its key, and there is no order over keys for a range \
                 to name",
            ),
        );
        self.lit(ExprKind::Error, TyId::ERROR, span)
    }

    fn assign_index(
        &mut self,
        base: &ast::Expr,
        index: &ast::Expr,
        span: Span,
        a: &ast::AssignStmt,
    ) -> Option<(hir::Stmt, Flow)> {
        let seq = self.expr(base, None);
        if self.types.is_poisoned(seq.ty) {
            return None;
        }
        if let TyKind::Map(key_ty, value_ty) = *self.types.kind(seq.ty) {
            let mut stmts = Vec::new();
            let place = self.container_place(base, seq, "assigned into", None, &mut stmts, span)?;
            // `m[k] += 1` has nothing to add to when `k` is missing, and a map
            // entry is read as an optional for exactly that reason. The
            // operator used to be dropped and the entry overwritten with the
            // right-hand side alone.
            if a.op.to_binary().is_some() {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0201,
                        format!("`{}` cannot update a map entry", a.op.text()),
                    )
                    .with_primary(a.span, "the entry may be missing")
                    .with_note(
                        "read it with `m[k]`, which is optional, decide what a missing entry \
                         counts as, and write the result back with `m[k] = …`",
                    ),
                );
                return None;
            }
            let k = self.expr(index, Some(key_ty));
            let k = self.accept(k, key_ty, None);
            self.note_compared(key_ty, k.span);
            let v = self.expr(&a.value, Some(value_ty));
            let v = self.coerce(v, Some(value_ty));
            self.expect_ty(v.ty, value_ty, v.span, None);
            if let Some(local) = place.binding() {
                return Some((
                    hir::Stmt::MapSet { local: hir::LocalId(local), key: k, value: v, span: a.span },
                    Flow::Falls,
                ));
            }
            let key = self.hoist(k, &mut stmts, span);
            let value = self.hoist(v, &mut stmts, span);
            let set = self.change_at(place, stmts, span, |_, local, _| hir::Stmt::MapSet {
                local: hir::LocalId(local),
                key,
                value,
                span: a.span,
            });
            return Some((set, Flow::Falls));
        }

        let Some(elem) = self.types.slice_elem(seq.ty) else {
            let found = self.types.with_article(seq.ty);
            self.diags.push(
                Diagnostic::error(codes::E0200, "only a slice can be index-assigned")
                    .with_primary(seq.span, format!("this is {}", found)),
            );
            return None;
        };

        // A slice is a copy-on-write value, so writing into it changes
        // whatever holds it, which must therefore be able to change.
        let mut stmts = Vec::new();
        let place = self.container_place(base, seq, "assigned into", None, &mut stmts, span)?;

        let i = self.expr(index, Some(TyId::INT));
        self.expect_ty(i.ty, TyId::INT, i.span, None);

        let value = self.expr(&a.value, Some(elem));
        let Some(local) = place.binding() else {
            // Held anywhere but a plain binding, the slice is changed as a
            // copy that is then written back, so the index and the value are
            // worked out first. Nothing the program wrote runs between taking
            // the copy and putting it back, so nothing it does can be lost.
            let at = self.hoist(i, &mut stmts, span);
            let op = a.op.to_binary();
            let value = match op {
                None => self.accept(value, elem, None),
                Some(_) => value,
            };
            let value = self.hoist(value, &mut stmts, span);
            let set = self.change_at(place, stmts, span, |this, local, ty| {
                let base = hir::Expr { kind: ExprKind::Local(hir::LocalId(local)), ty, span };
                let value = match op {
                    None => value,
                    Some(binop) => {
                        let current = hir::Expr {
                            kind: ExprKind::Index {
                                base: Box::new(base.clone()),
                                index: Box::new(at.clone()),
                            },
                            ty: elem,
                            span,
                        };
                        let sum = this.binary(binop, current, value, a.span);
                        this.accept(sum, elem, None)
                    }
                };
                hir::Stmt::SetIndex { base, index: at, value, span: a.span }
            });
            return Some((set, Flow::Falls));
        };
        let seq = hir::Expr {
            kind: ExprKind::Local(hir::LocalId(local)),
            ty: self.locals[local as usize].ty,
            span: base.span(),
        };
        let Some(binop) = a.op.to_binary() else {
            // An element slot typed `Option<T>` takes a `T` by subsumption,
            // and the `Wrap` has to be written for that, as it is everywhere
            // else a value is stored.
            let value = self.accept(value, elem, None);
            return Some((
                hir::Stmt::SetIndex { base: seq, index: i, value, span: a.span },
                Flow::Falls,
            ));
        };
        // `xs[i] += v` reads and writes one element, so the index is worked
        // out once, into a hidden local both sides use. Checking the index
        // expression twice evaluated it twice: `xs[next()] += 1` called
        // `next` twice and added the second element to the first.
        let slot = self.synthetic_local("index", TyId::INT, span);
        let at = hir::Expr { kind: ExprKind::Local(hir::LocalId(slot)), ty: TyId::INT, span: i.span };
        let current = hir::Expr {
            kind: ExprKind::Index { base: Box::new(seq.clone()), index: Box::new(at.clone()) },
            ty: elem,
            span,
        };
        let sum = self.binary(binop, current, value, a.span);
        let sum = self.accept(sum, elem, None);
        Some((
            hir::Stmt::Block(hir::Block {
                stmts: vec![
                    hir::Stmt::Let { local: hir::LocalId(slot), init: Some(i), span },
                    hir::Stmt::SetIndex { base: seq, index: at, value: sum, span: a.span },
                ],
            }),
            Flow::Falls,
        ))
    }

    /// Where a slice or map that is about to change in place is kept.
    ///
    /// Both are copy-on-write values, so changing one's contents changes
    /// whatever holds it. A plain `var` binding is changed where it lies, as
    /// it always was. Anything deeper — `grid[i][j] = v`, `b.cells.push(x)` —
    /// becomes what the roadmap used to tell people to write by hand: copy
    /// the value out into a hidden local, change that, and assign it back
    /// through the same place, level by level. Each level is checked as an
    /// assignment to it would be, so this accepts exactly what the
    /// hand-written version would and no more.
    ///
    /// The sub-expressions the place is built from — the struct a field is
    /// read from, each index — are evaluated here, once and in the order they
    /// were written, into hidden locals bound by `stmts`. `top` is the span
    /// of the value being changed, when this call is for something holding
    /// it rather than for that value itself.
    fn container_place(
        &mut self,
        base: &ast::Expr,
        seq: hir::Expr,
        what: &str,
        top: Option<Span>,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> Option<Place> {
        let mut written = base;
        while let ast::Expr::Paren { inner, .. } = written {
            written = inner;
        }
        let ty = seq.ty;
        match seq.kind {
            ExprKind::Local(id) => {
                self.require_mutable_holder(id.0, written.span(), what, top)?;
                Some(Place { root: PlaceRoot::Local { id: id.0, ty }, steps: Vec::new() })
            }
            // An optional binding narrowed to its value. The value is taken
            // out, changed, and put back present: changing the unwrapped
            // copy where it stood is how `xs[0] = 5` inside `if xs != nil`
            // used to go missing.
            ExprKind::Unwrap { value } if matches!(value.kind, ExprKind::Local(_)) => {
                let ExprKind::Local(id) = value.kind else { return None };
                self.require_mutable_holder(id.0, written.span(), what, top)?;
                let root = PlaceRoot::Narrowed { id: id.0, declared: value.ty, ty };
                Some(Place { root, steps: Vec::new() })
            }
            ExprKind::FieldGet { base: obj, index } => {
                let struct_id = match self.types.kind(obj.ty) {
                    TyKind::Struct(sid) => Some(*sid),
                    _ => None,
                };
                let (ast::Expr::Field { base: obj_written, name, .. }, Some(sid)) =
                    (written, struct_id)
                else {
                    // A tuple is held, but it is a value whose elements are
                    // fixed once it is built: `t.0 = …` is refused, so the
                    // write back this would need is too.
                    if matches!(self.types.kind(obj.ty), TyKind::Tuple(_)) {
                        let noun = self.container_noun(ty);
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("a {} in a tuple cannot be {}", noun, what),
                            )
                            .with_primary(written.span(), "an element of a tuple")
                            .with_note(
                                "a tuple's elements are fixed once it is built; build a new \
                                 one, or keep the value in a struct's `var` field",
                            ),
                        );
                        return None;
                    }
                    self.not_a_place(written, ty, what);
                    return None;
                };
                let (mutable, fty, decl) = {
                    let f = &self.types.struct_def(sid).fields[index as usize];
                    (f.mutable, f.ty, f.span)
                };
                if !mutable {
                    let sname = self.types.struct_def(sid).name.clone();
                    let noun = self.container_noun(ty);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0114,
                            format!("cannot change immutable field `{}`", name.name),
                        )
                        .with_primary(name.span, "this field cannot change")
                        .with_secondary(decl, "declared immutable here")
                        .with_note(format!(
                            "a {} held in a field is changed by writing the field back, so the \
                             field must be `var`: write `var {}: {}` on `{}`",
                            noun,
                            name.name,
                            self.types.name(fty),
                            sname
                        )),
                    );
                    return None;
                }
                // The field is written through the struct, which is a
                // reference, so nothing above it needs writing back — but the
                // binding it is reached through must allow the write, exactly
                // as it must for `b.cells = …`.
                if !self.require_mutable_base(obj_written) {
                    return None;
                }
                let holder = self.hoist(*obj, stmts, span);
                Some(Place { root: PlaceRoot::Field { holder, index, ty }, steps: Vec::new() })
            }
            ExprKind::Index { base: outer, index } if self.types.slice_elem(outer.ty).is_some() => {
                let ast::Expr::Index { base: outer_written, .. } = written else {
                    self.not_a_place(written, ty, what);
                    return None;
                };
                // Putting the changed element back is an assignment into the
                // slice holding it, whatever was done to the element.
                let top = top.or(Some(written.span()));
                let mut place =
                    self.container_place(outer_written, *outer, "assigned into", top, stmts, span)?;
                let at = self.hoist(*index, stmts, span);
                place.steps.push((at, ty));
                Some(place)
            }
            _ => {
                self.not_a_place(written, ty, what);
                None
            }
        }
    }

    /// "slice" or "map", for a message about changing one.
    fn container_noun(&self, ty: TyId) -> &'static str {
        match self.types.kind(ty) {
            TyKind::Map(..) => "map",
            _ => "slice",
        }
    }

    /// A slice or map is changed in place only where the change has somewhere
    /// to go. A call's result or a literal is a value nothing holds, so
    /// changing it would change nothing anybody could see.
    fn not_a_place(&mut self, written: &ast::Expr, ty: TyId, what: &str) {
        let noun = self.container_noun(ty);
        let name = if noun == "map" { "m" } else { "xs" };
        self.diags.push(
            Diagnostic::error(
                codes::E0200,
                format!("this {} cannot be {}: nothing holds it", noun, what),
            )
            .with_primary(written.span(), "a value, not a place the change could be kept")
            .with_note(format!(
                "a {} is a copy-on-write value, so changing it in place changes what holds \
                 it: a `var` binding, a `var` field, or an element of a slice held by one \
                 of those",
                noun
            ))
            .with_note(format!("bind it first — `var {} = …` — and change that", name)),
        );
    }

    /// The binding a slice or map is changed through must be `var`.
    ///
    /// `top` is set when what changes is inside the binding's value rather
    /// than the value itself: `grid[0].push(1)` changes `grid`.
    fn require_mutable_holder(
        &mut self,
        id: u32,
        at: Span,
        what: &str,
        top: Option<Span>,
    ) -> Option<()> {
        let local = &self.locals[id as usize];
        if local.mutable {
            return Some(());
        }
        let (name, decl, ty) = (local.name.clone(), local.span, local.ty);
        let noun = match *self.types.kind(ty) {
            TyKind::Optional(inner) => self.container_noun(inner),
            _ => self.container_noun(ty),
        };
        let mut d = Diagnostic::error(codes::E0114, format!("`{}` cannot be {}", name, what))
            .with_primary(at, "this binding is immutable")
            .with_secondary(decl, "declared with `let` here")
            .with_note(match top {
                None => format!(
                    "a {} is a copy-on-write value, so changing its contents changes the \
                     binding; declare it `var`",
                    noun
                ),
                Some(inner) => format!(
                    "`{}` is held in `{}`, and a {} is a copy-on-write value: changing \
                     anything inside it changes the binding; declare it `var`",
                    self.text(inner),
                    name,
                    noun
                ),
            });
        if let Some(kw) = self.let_keyword_span(decl) {
            d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
        }
        self.diags.push(d);
        None
    }

    /// Bind an operand to a hidden local, so it is evaluated once and before
    /// anything is copied out. A literal or a local is left where it is:
    /// nothing between here and its use can change either.
    fn hoist(&mut self, e: hir::Expr, stmts: &mut Vec<hir::Stmt>, span: Span) -> hir::Expr {
        if matches!(
            e.kind,
            ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Str(_)
                | ExprKind::Bool(_)
                | ExprKind::Nil
                | ExprKind::Local(_)
        ) {
            return e;
        }
        let (ty, at) = (e.ty, e.span);
        let id = self.synthetic_local("place", ty, span);
        stmts.push(hir::Stmt::Let { local: hir::LocalId(id), init: Some(e), span });
        hir::Expr { kind: ExprKind::Local(hir::LocalId(id)), ty, span: at }
    }

    /// Change the value a place holds by changing a copy of it and writing
    /// the copy back. `change` is handed the local holding the copy, and its
    /// type; `stmts` already binds the operands, which run first.
    fn change_at(
        &mut self,
        place: Place,
        mut stmts: Vec<hir::Stmt>,
        span: Span,
        change: impl FnOnce(&mut Self, u32, TyId) -> hir::Stmt,
    ) -> hir::Stmt {
        let local = |(id, ty): (u32, TyId)| hir::Expr {
            kind: ExprKind::Local(hir::LocalId(id)),
            ty,
            span,
        };
        // Copy out each value from the root to the one being changed,
        // outermost first. A binding at the root is its own copy.
        let first = match &place.root {
            PlaceRoot::Local { id, ty } => (*id, *ty),
            PlaceRoot::Narrowed { id, declared, ty } => {
                let value = Box::new(local((*id, *declared)));
                let read = hir::Expr { kind: ExprKind::Unwrap { value }, ty: *ty, span };
                (self.copy_of(read, &mut stmts, span), *ty)
            }
            PlaceRoot::Field { holder, index, ty } => {
                let base = Box::new(holder.clone());
                let read = hir::Expr { kind: ExprKind::FieldGet { base, index: *index }, ty: *ty, span };
                (self.copy_of(read, &mut stmts, span), *ty)
            }
        };
        let mut chain = vec![first];
        for (at, elem) in &place.steps {
            let outer = chain[chain.len() - 1];
            let read = hir::Expr {
                kind: ExprKind::Index { base: Box::new(local(outer)), index: Box::new(at.clone()) },
                ty: *elem,
                span,
            };
            chain.push((self.copy_of(read, &mut stmts, span), *elem));
        }

        let (inner, ty) = chain[chain.len() - 1];
        stmts.push(change(self, inner, ty));

        // And write each copy back where it came from, innermost first.
        for (k, (at, _)) in place.steps.iter().enumerate().rev() {
            stmts.push(hir::Stmt::SetIndex {
                base: local(chain[k]),
                index: at.clone(),
                value: local(chain[k + 1]),
                span,
            });
        }
        match place.root {
            PlaceRoot::Local { .. } => {}
            PlaceRoot::Narrowed { id, declared, .. } => stmts.push(hir::Stmt::Assign {
                local: hir::LocalId(id),
                value: hir::Expr {
                    kind: ExprKind::Wrap { value: Box::new(local(chain[0])) },
                    ty: declared,
                    span,
                },
                span,
            }),
            PlaceRoot::Field { holder, index, .. } => stmts.push(hir::Stmt::SetField {
                base: holder,
                index,
                value: local(chain[0]),
                span,
            }),
        }
        hir::Stmt::Block(hir::Block { stmts })
    }

    /// Bind a read to a new hidden local and answer the local.
    fn copy_of(&mut self, read: hir::Expr, stmts: &mut Vec<hir::Stmt>, span: Span) -> u32 {
        let id = self.synthetic_local("held", read.ty, span);
        stmts.push(hir::Stmt::Let { local: hir::LocalId(id), init: Some(read), span });
        id
    }

    /// The local a place expression is ultimately rooted at: `a.b.c[0]` is
    /// rooted at `a`. `None` when the root is not a plain binding.
    fn root_binding(&self, e: &ast::Expr) -> Option<u32> {
        match e {
            // `self` is local 0 of every method, and is not spelled as a path.
            ast::Expr::SelfExpr(_) => Some(0),
            ast::Expr::Path(p) => match self.resolved.lookup_use(p.span) {
                Some(Res::Local(id)) => Some(id),
                _ => None,
            },
            ast::Expr::Field { base, .. } | ast::Expr::Index { base, .. } => {
                self.root_binding(base)
            }
            ast::Expr::Paren { inner, .. } => self.root_binding(inner),
            _ => None,
        }
    }

    /// Report when what is about to be modified is rooted at an immutable
    /// binding. `self` is the same rule wearing a different word: the receiver
    /// is mutable exactly when the method declared `var self`. Answers
    /// whether the modification may go ahead.
    fn require_mutable_base(&mut self, base: &ast::Expr) -> bool {
        let Some(id) = self.root_binding(base) else { return true };
        if self.locals[id as usize].mutable {
            return true;
        }
        let name = self.locals[id as usize].name.clone();
        let decl = self.locals[id as usize].span;
        if name == "self" {
            self.diags.push(
                Diagnostic::error(
                    codes::E0114,
                    "cannot modify `self` in a method that does not take `var self`",
                )
                .with_primary(base.span(), "this receiver is immutable")
                .with_secondary(decl, "declared here")
                .with_note(
                    "write `var self` as the receiver; a caller then has to hold the value \
                     in a `var` binding, which is what makes the modification visible at \
                     the call site rather than hidden inside the method",
                ),
            );
            return false;
        }
        let mut d = Diagnostic::error(
            codes::E0114,
            format!("cannot modify `{}` through an immutable binding", name),
        )
        .with_primary(base.span(), "this binding is immutable")
        .with_secondary(decl, "declared with `let` here");
        if let Some(kw) = self.let_keyword_span(decl) {
            d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
        }
        self.diags.push(d);
        false
    }

    /// `err.message()` needs an error that is there.
    ///
    /// An `error` is either nil or a failure, so reading the message off one
    /// nothing has proved present is the same mistake as reading a value whose
    /// error was never checked — and it is one the backends cannot even agree
    /// to get wrong the same way.
    fn require_error_present(&mut self, base: &ast::Expr, span: Span) {
        let mut inner = base;
        while let ast::Expr::Paren { inner: i, .. } = inner {
            inner = i;
        }
        // Only a local can be proved present, so an error reached any other
        // way — a field, a call, an element — has to be bound and tested
        // first. Waving those through was how `r.e.message()` on a nil field
        // and `err.cause().message()` on an error with no cause reached a
        // backend, which answered with an empty string or a trap depending on
        // which one it was.
        let local = match inner {
            ast::Expr::Path(p) => match self.resolved.lookup_use(p.span) {
                Some(Res::Local(id)) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(id) = local else {
            let what = self.text(inner.span()).to_string();
            self.diags.push(
                Diagnostic::error(
                    codes::E0301,
                    format!("`{}` may be nil, so it has no message", what),
                )
                .with_primary(span, "reading the message needs an error that is present")
                .with_secondary(inner.span(), "only a binding can be proved non-nil")
                .with_note(format!(
                    "bind it and test it: `let e = {}` then `if e != nil {{ … e.message() … }}`",
                    what
                ))
                .with_note(
                    "an `error` is either nil or a failure; there is no message on the nil side, \
                     and no zero value standing in for one",
                ),
            );
            return;
        };
        if self.error_nonnil.contains(&id) {
            return;
        }
        let name = self.locals[id as usize].name.clone();
        self.diags.push(
            Diagnostic::error(
                codes::E0301,
                format!("`{}` may be nil here, so it has no message", name),
            )
            .with_primary(span, "reading the message needs an error that is present")
            .with_secondary(base.span(), format!("`{}` has not been proved non-nil", name))
            .with_note(format!(
                "test it first: `if {} != nil {{ … {}.message() … }}`, or leave early with \
                 `if {} == nil {{ return … }}`",
                name, name, name
            ))
            .with_note(
                "an `error` is either nil or a failure; there is no message on the nil side, \
                 and no zero value standing in for one",
            ),
        );
    }

    /// Calling a `var self` method through an immutable binding.
    fn require_mutable_receiver(&mut self, base: &ast::Expr, method: &str) {
        let Some(id) = self.root_binding(base) else { return };
        if self.locals[id as usize].mutable {
            return;
        }
        let name = self.locals[id as usize].name.clone();
        let decl = self.locals[id as usize].span;
        let mut d = Diagnostic::error(
            codes::E0114,
            format!("`{}` may modify its receiver, but `{}` is immutable", method, name),
        )
        .with_primary(base.span(), "this binding cannot change")
        .with_secondary(decl, "declared here")
        .with_note(format!(
            "`{}` takes `var self`, so calling it needs a `var` binding",
            method
        ));
        if name == "self" {
            d = d.with_note(
                "the enclosing method takes a plain `self`; it would have to take \
                 `var self` to pass its receiver on",
            );
        } else if let Some(kw) = self.let_keyword_span(decl) {
            d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
        }
        self.diags.push(d);
    }

    /// `xs.len()`, `xs.get(i)`, `xs.push(v)`.
    fn slice_method(
        &mut self,
        base: &ast::Expr,
        seq: hir::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let elem = self.types.slice_elem(seq.ty)?;

        match name.name.as_str() {
            "len" => {
                if !args.is_empty() {
                    self.arity_error("len", args.len(), 0, span, None);
                }
                Some(hir::Expr {
                    kind: ExprKind::SliceLen { base: Box::new(seq) },
                    ty: TyId::INT,
                    span,
                })
            }

            "get" => {
                if args.len() != 1 {
                    self.arity_error("get", args.len(), 1, span, None);
                    return Some(self.lit(ExprKind::Error, TyId::ERROR, span));
                }
                let i = self.expr(&args[0], Some(TyId::INT));
                self.expect_ty(i.ty, TyId::INT, i.span, None);
                let ty = self.types.optional_of(elem);
                Some(hir::Expr {
                    kind: ExprKind::SliceGet { base: Box::new(seq), index: Box::new(i) },
                    ty,
                    span,
                })
            }

            "push" => {
                if args.len() != 1 {
                    self.arity_error("push", args.len(), 1, span, None);
                    return Some(self.lit(ExprKind::Error, TyId::ERROR, span));
                }
                let mut stmts = Vec::new();
                let place = self.container_place(base, seq, "pushed to", None, &mut stmts, span)?;
                let v = self.expr(&args[0], Some(elem));
                let v = self.accept(v, elem, None);
                // `push` is a statement, not an expression; the checker returns
                // unit and MIR emits the mutation.
                let push = match place.binding() {
                    Some(id) => hir::Stmt::SlicePush { local: hir::LocalId(id), value: v, span },
                    None => {
                        let value = self.hoist(v, &mut stmts, span);
                        self.change_at(place, stmts, span, |_, local, _| hir::Stmt::SlicePush {
                            local: hir::LocalId(local),
                            value,
                            span,
                        })
                    }
                };
                Some(self.as_statement(push, span))
            }

            _ => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`{}` has no method `{}`", self.types.name(seq.ty), name.name),
                    )
                    .with_primary(name.span, "no such method")
                    .with_note("a slice has: len, get, push")
                    .with_note(
                        "everything else a slice can do is a prelude function taking one — \
                         `enumerate(xs)`, `map(xs, f)`, `filter(xs, test)`, `sorted(xs, less)` \
                         — because a slice takes methods only from the compiler and those \
                         three are what nothing else can be built from",
                    ),
                );
                Some(self.lit(ExprKind::Error, TyId::ERROR, span))
            }
        }
    }

    // ---- enums ------------------------------------------------------------

    /// `Circle(radius: 1.0)`, `Number(3.0)`, or a unit variant like `Point`.
    /// Work out a generic enum's type arguments and return the specialisation.
    fn solve_enum_args(
        &mut self,
        template: kite_hir::EnumId,
        vi: u32,
        args: &[ast::Expr],
        expected: Option<TyId>,
        span: Span,
    ) -> Option<kite_hir::EnumId> {
        let count = self.types.enum_def(template).generic_count;

        if let Some(want) = expected {
            if let TyKind::Enum(e) = *self.types.kind(want) {
                if self.types.enum_template_of(e) == Some(template) {
                    return Some(e);
                }
            }
        }

        let generics: Vec<GenericDef> = (0..count)
            .map(|i| {
                let ty = self.types.param_ty(i as u32, "");
                GenericDef { name: format!("#{}", i), ty, bounds: Vec::new(), span }
            })
            .collect();
        let mut subst: Vec<Option<TyId>> = vec![None; count];
        let declared: Vec<TyId> = self.types.enum_def(template).variants[vi as usize]
            .fields
            .iter()
            .map(|f| f.ty)
            .collect();
        // A trial pass, as for struct literals: its diagnostics belong to the
        // real check against the specialisation, not to inference.
        let trial = self.begin_trial();
        for (i, a) in args.iter().enumerate() {
            let Some(d) = declared.get(i).copied() else { continue };
            let v = self.expr(a, None);
            self.unify(d, v.ty, &generics, &mut subst, v.span);
        }
        self.end_trial(trial);

        let mut solved = Vec::with_capacity(count);
        for (i, s) in subst.iter().enumerate() {
            match s {
                Some(t) => solved.push(*t),
                None => {
                    // A unit variant of a generic enum says nothing about the
                    // parameter — `Maybe.None` could be any `Maybe<T>` — so the
                    // binding has to.
                    let name = self.types.enum_def(template).name.clone();
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0209,
                            format!("cannot infer type argument {} of `{}`", i + 1, name),
                        )
                        .with_primary(span, "nothing here pins this parameter down")
                        .with_note(format!(
                            "annotate the binding: `let x: {}<...> = ...`",
                            name
                        )),
                    );
                    return None;
                }
            }
        }
        Some(self.types.instantiate_enum(template, &solved))
    }

    #[allow(clippy::too_many_arguments)]
    fn variant_value(
        &mut self,
        ti: u32,
        vi: u32,
        args: &[ast::Expr],
        arg_names: &[Option<ast::Ident>],
        path_span: Span,
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        let Some(TypeTarget::Enum(eid)) = self.type_ids[ti as usize] else {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };
        // A generic enum is specialised from the payload, or from the type the
        // context is expecting — the same rule struct literals follow, and for
        // the same reason: `<` in expression position is a comparison.
        let eid = if self.types.enum_def(eid).generic_count > 0 {
            match self.solve_enum_args(eid, vi, args, expected, span) {
                Some(id) => id,
                None => return self.lit(ExprKind::Error, TyId::ERROR, span),
            }
        } else {
            eid
        };
        let enum_ty = self.types.enum_ty(eid);
        self.check_type_bounds(ti, enum_ty, span);

        let (enum_name, variant_name, field_tys, named, decl_span) = {
            let def = self.types.enum_def(eid);
            let v = &def.variants[vi as usize];
            (
                def.name.clone(),
                v.name.clone(),
                v.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
                v.named,
                v.span,
            )
        };

        if args.len() != field_tys.len() {
            let full = format!("{}.{}", enum_name, variant_name);
            if field_tys.is_empty() {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0113,
                        format!("`{}` carries no payload", full),
                    )
                    .with_primary(span, "written with arguments")
                    .with_secondary(decl_span, "declared as a unit variant")
                    .with_note(format!("write it as `{}` on its own", variant_name)),
                );
            } else {
                self.arity_error(&full, args.len(), field_tys.len(), span, Some(decl_span));
            }
            let _ = named;
            for a in args {
                let _ = self.expr(a, None);
            }
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        // Named arguments are placed by name; positional ones by order.
        let field_names: Vec<String> = {
            let def = self.types.enum_def(eid);
            def.variants[vi as usize]
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect()
        };

        let mut slots: Vec<Option<hir::Expr>> = (0..field_tys.len()).map(|_| None).collect();
        for (i, a) in args.iter().enumerate() {
            let index = match arg_names.get(i).and_then(|n| n.as_ref()) {
                None => i,
                Some(n) => match field_names.iter().position(|f| *f == n.name) {
                    Some(x) => x,
                    None => {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("`{}` has no field `{}`", variant_name, n.name),
                            )
                            .with_primary(n.span, "no such field")
                            .with_note(format!(
                                "`{}` carries: {}",
                                variant_name,
                                field_names.join(", ")
                            )),
                        );
                        let _ = self.expr(a, None);
                        continue;
                    }
                },
            };
            let want = field_tys[index];
            let e = self.expr(a, Some(want));
            slots[index] = Some(self.accept(e, want, Some(decl_span)));
        }

        let mut fields = Vec::with_capacity(field_tys.len());
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Some(e) => fields.push(e),
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0113,
                            format!("missing field `{}` in `{}`", field_names[i], variant_name),
                        )
                        .with_primary(span, "every payload field must be given"),
                    );
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
            }
        }
        let _ = path_span;

        hir::Expr {
            kind: ExprKind::EnumNew { enum_id: eid, variant: vi, fields },
            ty: enum_ty,
            span,
        }
    }

    // ---- match ------------------------------------------------------------

    fn match_expr(&mut self, m: &ast::MatchExpr, expected: Option<TyId>) -> hir::Expr {
        self.match_expr_with_flow(m, expected).0
    }

    /// The same check, also reporting whether control falls out of the match.
    ///
    /// Only a statement asks: an expression that needs a value gets one or a
    /// diagnostic, and a match every arm of which leaves cannot supply one
    /// either way.
    fn match_expr_with_flow(
        &mut self,
        m: &ast::MatchExpr,
        expected: Option<TyId>,
    ) -> (hir::Expr, Flow) {
        let scrutinee = self.expr(&m.scrutinee, None);
        let scrut_ty = scrutinee.ty;

        if m.arms.is_empty() {
            self.diags.push(
                Diagnostic::error(codes::E0210, "a `match` needs at least one arm")
                    .with_primary(m.span, "no arms")
                    .with_note("an empty match can never produce a value"),
            );
            return (self.lit(ExprKind::Error, TyId::ERROR, m.span), Flow::Falls);
        }

        // Arms are alternatives, not a sequence: each starts from the state the
        // match was entered in, and what survives is the join of the arms that
        // can fall out. Letting one arm's `check` leak into the next would make
        // an error checked in one case clean in every other — and taking the
        // entry state for what survives, as definite assignment once did,
        // refused a `let` that every arm assigns.
        let entry = self.flow_state();
        let mut exit: Option<FlowState> = None;
        // The arms that leave, joined, stand for the match when every arm
        // does: nothing follows it, but the end of the function still asks
        // what was left unchecked on the way out.
        let mut left: Option<FlowState> = None;
        let mut poisoned = false;
        let mut arms = Vec::with_capacity(m.arms.len());
        let mut result_ty: Option<TyId> = None;
        let mut arm_spans: Vec<(Span, TyId)> = Vec::new();

        // Arms are checked in order so a binding can be narrowed. Once an
        // earlier arm has matched `nil`, a later binding pattern cannot receive
        // one, so it binds the unwrapped type — which is what
        // SPECIFICATION.md section 3.3 shows.
        let mut nil_covered = false;

        for arm in &m.arms {
            self.set_flow_state(entry.clone());
            let bind_ty = match *self.types.kind(scrut_ty) {
                TyKind::Optional(inner) if nil_covered => inner,
                _ => scrut_ty,
            };
            let pattern = self.pattern_with(&arm.pattern, scrut_ty, bind_ty);
            if arm.guard.is_none() && covers_nil(&pattern) {
                nil_covered = true;
            }

            let guard = arm.guard.as_ref().map(|g| {
                let c = self.expr(g, Some(TyId::BOOL));
                if !self.types.satisfies(c.ty, TyId::BOOL) && !self.types.is_poisoned(c.ty) {
                    let article = self.types.with_article(c.ty);
                    self.diags.push(
                        Diagnostic::error(codes::E0202, "a match guard must be `bool`")
                            .with_primary(c.span, format!("this is {}", article)),
                    );
                }
                c
            });

            let want = expected.or(result_ty);
            let body = match &arm.body {
                ast::MatchBody::Expr(e) => self.expr(e, want),
                ast::MatchBody::Block(b) => self.match_block(b, want),
            };
            // An arm producing a `T` where a `?T` is wanted is subsumption,
            // exactly as it is for a `let` or an argument — so `Text(s) => s`
            // and `other => nil` are arms of one `Option<str>` match rather
            // than a type error about two arms disagreeing.
            let body = self.coerce(body, want);

            // An arm that diverges contributes nothing to the join: control
            // never arrives at the join from it. What it declared goes out of
            // scope where it leaves, so that is where an error in it nobody
            // looked at is reported.
            let here = self.flow_state();
            let into = if body.ty == TyId::NEVER {
                self.report_unchecked_errors(Some(arm.span));
                &mut left
            } else {
                &mut exit
            };
            *into = Some(match into.take() {
                None => here,
                Some(acc) => self.join_flow(&acc, &here),
            });
            poisoned |= body.ty == TyId::ERROR;

            if body.ty != TyId::NEVER && !self.types.is_poisoned(body.ty) {
                match result_ty {
                    None => result_ty = Some(body.ty),
                    Some(want) if !self.types.satisfies(body.ty, want) => {
                        let (a, b) = (self.types.name(want), self.types.name(body.ty));
                        let mut d = Diagnostic::error(
                            codes::E0200,
                            "match arms have different types",
                        )
                        .with_primary(body.span, format!("this arm is a `{}`", b));
                        if let Some((s, _)) = arm_spans.first() {
                            d = d.with_secondary(*s, format!("this arm is a `{}`", a));
                        }
                        d = d.with_note("every arm of a `match` must produce the same type");
                        self.diags.push(d);
                    }
                    Some(_) => {}
                }
            }
            arm_spans.push((body.span, body.ty));

            arms.push(hir::MatchArm {
                pattern,
                guard,
                body,
                span: arm.span,
            });
        }

        // A binding one arm's pattern introduced is out of scope after the
        // match, so what the join says about it is never asked.
        self.set_flow_state(exit.or(left).unwrap_or(entry));

        let exhaustive = self.check_exhaustive(m, &arms, scrut_ty);

        // Every arm left, and control had to enter one of them — that is what
        // exhaustiveness proves — so nothing after the match runs. A guard does
        // not weaken this: coverage is established by the unguarded arms alone,
        // so a guard failing only moves control on to a later arm.
        //
        // This is reported as flow rather than written into the type. A match
        // that produces no value is a `()` in an expression position whatever
        // its arms do, and calling it `!` there would let it stand in for any
        // type at all — `1 + match …`, a struct field, an argument — none of
        // which the backends have a value to lower. Divergence is a statement
        // property here, and the statement is where it is asked about.
        let flow = if exhaustive && !arms.is_empty() && arms.iter().all(|a| a.body.ty == TyId::NEVER)
        {
            Flow::Diverges
        } else {
            Flow::Falls
        };

        // No arm produced a type, and one of them could not: the match's type
        // is unknown, not `()`, or binding it reports the one mistake again as
        // "cannot bind a value of type `()`".
        let ty = result_ty.unwrap_or(if poisoned { TyId::ERROR } else { TyId::UNIT });
        let expr = hir::Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            ty,
            span: m.span,
        };
        (expr, flow)
    }

    /// A block arm.
    ///
    /// A block that *is* one expression yields it. Anything longer runs for its
    /// effects and produces unit — there is no tail expression here, any more
    /// than there is in an `if` used as a value, which says "expected a single
    /// expression" for the same reason. A value that arrives by falling off the
    /// end of a block is exactly the hidden control flow the language is spent
    /// avoiding.
    ///
    /// So an arm wanting several statements *and* a result writes the match in
    /// statement position and returns from every arm, which
    /// [`Self::match_expr_with_flow`] recognises as diverging.
    fn match_block(&mut self, b: &ast::Block, expected: Option<TyId>) -> hir::Expr {
        match b.stmts.as_slice() {
            [ast::Stmt::Expr(e)] => self.expr(e, expected),
            _ => {
                let sig = self.current_signature();
                let (block, flow) = self.block(b, &sig);
                // A block arm runs for its effects, so it produces unit — or
                // never, when it always diverges.
                let ty = if flow == Flow::Diverges { TyId::NEVER } else { TyId::UNIT };
                hir::Expr { kind: ExprKind::Block(block), ty, span: b.span }
            }
        }
    }

    // ---- generics ---------------------------------------------------------

    /// Match a declared type against an actual one, filling in any parameters
    /// the declared type mentions.
    ///
    /// This is one-directional and structural: it never solves for anything the
    /// argument does not pin down, which is what keeps inference explainable.
    fn unify(
        &mut self,
        declared: TyId,
        actual: TyId,
        generics: &[GenericDef],
        subst: &mut Vec<Option<TyId>>,
        span: Span,
    ) {
        if self.types.is_poisoned(actual) {
            return;
        }
        if let TyKind::Param { index, .. } = *self.types.kind(declared) {
            let slot = index as usize;
            if slot >= subst.len() {
                return;
            }
            match subst[slot] {
                None => subst[slot] = Some(actual),
                Some(prev) if prev != actual && !self.types.satisfies(actual, prev) => {
                    let (a, b) = (self.types.name(prev), self.types.name(actual));
                    let name = generics[slot].name.clone();
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0209,
                            format!("conflicting types for `{}`", name),
                        )
                        .with_primary(span, format!("here `{}` would be `{}`", name, b))
                        .with_note(format!("an earlier argument already made it `{}`", a)),
                    );
                }
                Some(_) => {}
            }
            return;
        }
        // Structural descent: `[T]` against `[int]` solves `T`.
        match (self.types.kind(declared).clone(), self.types.kind(actual).clone()) {
            (TyKind::Slice(d), TyKind::Slice(a)) => self.unify(d, a, generics, subst, span),
            (TyKind::Optional(d), TyKind::Optional(a)) => self.unify(d, a, generics, subst, span),
            // A `T` argument for an `Option<T>` parameter solves through the
            // same subsumption that lets the value be passed at all.
            (TyKind::Optional(d), _) => self.unify(d, actual, generics, subst, span),
            (TyKind::Map(dk, dv), TyKind::Map(ak, av)) => {
                self.unify(dk, ak, generics, subst, span);
                self.unify(dv, av, generics, subst, span);
            }
            (TyKind::Tuple(d), TyKind::Tuple(a)) if d.len() == a.len() => {
                for (x, y) in d.iter().zip(a.iter()) {
                    self.unify(*x, *y, generics, subst, span);
                }
            }
            (TyKind::Fn { params: dp, ret: dr }, TyKind::Fn { params: ap, ret: ar })
                if dp.len() == ap.len() =>
            {
                for (x, y) in dp.iter().zip(ap.iter()) {
                    self.unify(*x, *y, generics, subst, span);
                }
                self.unify(dr, ar, generics, subst, span);
            }
            // Two specialisations of one declaration: `Tree<T>` against
            // `Tree<int>` solves `T`.
            (TyKind::Struct(d), TyKind::Struct(a)) => {
                if let (Some((dt, da)), Some((at, aa))) =
                    (self.types.struct_origin_of(d), self.types.struct_origin_of(a))
                {
                    if dt == at && da.len() == aa.len() {
                        for (x, y) in da.iter().zip(aa.iter()) {
                            self.unify(*x, *y, generics, subst, span);
                        }
                    }
                }
            }
            (TyKind::Enum(d), TyKind::Enum(a)) => {
                if let (Some((dt, da)), Some((at, aa))) =
                    (self.types.enum_origin_of(d), self.types.enum_origin_of(a))
                {
                    if dt == at && da.len() == aa.len() {
                        for (x, y) in da.iter().zip(aa.iter()) {
                            self.unify(*x, *y, generics, subst, span);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Replace every parameter in a type with what it has been solved to.
    /// Unsolved parameters are left alone; the caller reports them once.
    fn apply_subst(&mut self, ty: TyId, subst: &[Option<TyId>]) -> TyId {
        if subst.is_empty() {
            return ty;
        }
        match self.types.kind(ty).clone() {
            TyKind::Param { index, .. } => subst
                .get(index as usize)
                .copied()
                .flatten()
                .unwrap_or(ty),
            TyKind::Slice(e) => {
                let e = self.apply_subst(e, subst);
                self.types.slice_of(e)
            }
            TyKind::Optional(i) => {
                let i = self.apply_subst(i, subst);
                self.types.optional_of(i)
            }
            TyKind::Map(k, v) => {
                let (k, v) = (self.apply_subst(k, subst), self.apply_subst(v, subst));
                self.types.map_of(k, v)
            }
            TyKind::Tuple(es) => {
                let es: Vec<TyId> = es.iter().map(|e| self.apply_subst(*e, subst)).collect();
                self.types.tuple_of(es)
            }
            TyKind::Fn { params, ret } => {
                let ps: Vec<TyId> = params.iter().map(|p| self.apply_subst(*p, subst)).collect();
                let r = self.apply_subst(ret, subst);
                self.types.fn_of(ps, r)
            }
            TyKind::Fallible(v) => {
                let v = self.apply_subst(v, subst);
                self.types.fallible_of(v)
            }
            TyKind::Struct(s) => match self.types.struct_origin_of(s) {
                Some((template, args)) => {
                    let args: Vec<TyId> =
                        args.iter().map(|a| self.apply_subst(*a, subst)).collect();
                    let id = self.types.instantiate_struct(template, &args);
                    self.types.struct_ty(id)
                }
                None => ty,
            },
            TyKind::Enum(e) => match self.types.enum_origin_of(e) {
                Some((template, args)) => {
                    let args: Vec<TyId> =
                        args.iter().map(|a| self.apply_subst(*a, subst)).collect();
                    let id = self.types.instantiate_enum(template, &args);
                    self.types.enum_ty(id)
                }
                None => ty,
            },
            _ => ty,
        }
    }

    /// The substituted type, or `None` while it still mentions an unsolved
    /// parameter — in which case there is nothing useful to expect of an
    /// argument, and it is checked on its own terms.
    fn apply_subst_opt(&mut self, ty: TyId, subst: &[Option<TyId>]) -> Option<TyId> {
        if self.unsolved_in(ty, subst) {
            return None;
        }
        Some(self.apply_subst(ty, subst))
    }

    /// Whether this *declared* type still mentions a parameter the call has
    /// not worked out yet.
    ///
    /// Asked of the signature rather than of the substituted result, because
    /// the result cannot answer it. Inside a generic function the enclosing
    /// function's own parameters are ordinary bound types — as good as
    /// concrete — and they are `TyKind::Param` exactly like an unsolved one,
    /// down to sharing an index. Testing the result therefore threw away every
    /// expectation inside a generic function: `ui.decorated(ui.text_of(…), …)`
    /// could not tell its argument what it wanted, though it knew.
    ///
    /// The declared type has no such ambiguity. Every parameter in it is the
    /// callee's, so `subst` answers for each one directly.
    fn unsolved_in(&self, ty: TyId, subst: &[Option<TyId>]) -> bool {
        match self.types.kind(ty) {
            TyKind::Param { index, .. } => {
                subst.get(*index as usize).copied().flatten().is_none()
            }
            TyKind::Struct(s) => self
                .types
                .struct_origin_of(*s)
                .is_some_and(|(_, args)| args.iter().any(|a| self.unsolved_in(*a, subst))),
            TyKind::Enum(e) => self
                .types
                .enum_origin_of(*e)
                .is_some_and(|(_, args)| args.iter().any(|a| self.unsolved_in(*a, subst))),
            TyKind::Slice(e) | TyKind::Optional(e) | TyKind::Fallible(e) => {
                self.unsolved_in(*e, subst)
            }
            TyKind::Map(k, v) => self.unsolved_in(*k, subst) || self.unsolved_in(*v, subst),
            TyKind::Tuple(es) => es.iter().any(|e| self.unsolved_in(*e, subst)),
            TyKind::Fn { params, ret } => {
                params.iter().any(|p| self.unsolved_in(*p, subst))
                    || self.unsolved_in(*ret, subst)
            }
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn mentions_param(&self, ty: TyId) -> bool {
        match self.types.kind(ty) {
            TyKind::Param { .. } => true,
            TyKind::Struct(s) => self
                .types
                .struct_origin_of(*s)
                .is_some_and(|(_, args)| args.iter().any(|a| self.mentions_param(*a))),
            TyKind::Enum(e) => self
                .types
                .enum_origin_of(*e)
                .is_some_and(|(_, args)| args.iter().any(|a| self.mentions_param(*a))),
            TyKind::Slice(e) | TyKind::Optional(e) | TyKind::Fallible(e) => self.mentions_param(*e),
            TyKind::Map(k, v) => self.mentions_param(*k) || self.mentions_param(*v),
            TyKind::Tuple(es) => es.iter().any(|e| self.mentions_param(*e)),
            TyKind::Fn { params, ret } => {
                params.iter().any(|p| self.mentions_param(*p)) || self.mentions_param(*ret)
            }
            _ => false,
        }
    }

    /// The solved type arguments, reporting any parameter nothing pinned down.
    fn finish_subst(
        &mut self,
        generics: &[GenericDef],
        subst: &[Option<TyId>],
        span: Span,
    ) -> Vec<TyId> {
        let mut out = Vec::with_capacity(generics.len());
        for (i, g) in generics.iter().enumerate() {
            match subst.get(i).copied().flatten() {
                Some(t) => out.push(t),
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0209,
                            format!("cannot infer `{}`", g.name),
                        )
                        .with_primary(span, format!("`{}` is not determined by any argument", g.name))
                        .with_secondary(g.span, "declared here")
                        .with_note("Kite has no turbofish; take a value of that type instead"),
                    );
                    out.push(TyId::ERROR);
                }
            }
        }
        out
    }

    /// Every bound must hold for the type chosen.
    fn check_bounds(&mut self, generics: &[GenericDef], targs: &[TyId], span: Span) {
        for (g, t) in generics.iter().zip(targs.iter()) {
            if self.types.is_poisoned(*t) {
                continue;
            }
            for bound in &g.bounds {
                // `Share` is the one bound nobody implements: the compiler
                // decides, structurally, and the answer is "deeply immutable".
                // Most types satisfy it without their author knowing it
                // exists, which is what makes data races impossible here
                // without an annotation burden.
                if self.is_share_bound(*bound) {
                    if !self.share_given_params(*t) {
                        self.report_not_share(*t, &g.name, span, g.span);
                    }
                    continue;
                }
                if self.type_implements(*t, *bound) {
                    continue;
                }
                let (tn, bn) = (self.types.name(*t), self.types.trait_def(*bound).name.clone());
                let mut d = Diagnostic::error(
                    codes::E0208,
                    format!("`{}` does not implement `{}`", tn, bn),
                )
                .with_primary(span, format!("`{}` is required to be `{}`", g.name, bn))
                .with_secondary(g.span, "the bound is declared here");
                d = match *self.types.kind(*t) {
                    // A parameter is known only by its bounds, so the fix is
                    // to say more about it where it is declared.
                    TyKind::Param { index, .. } => {
                        match self.generic_defs.get(index as usize).map(|d| d.span) {
                            Some(decl) => d
                                .with_secondary(decl, format!("`{}` is declared here", tn))
                                .with_note(format!("add the bound: `{}: {}`", tn, bn)),
                            None => d,
                        }
                    }
                    _ if self.type_index_of(*t).is_some_and(|ti| {
                        self.trait_index_of(*bound).is_some_and(|tri| self.resolved.implements(ti, tri))
                    }) =>
                    {
                        d.with_note(format!(
                            "`{}` implements `{}` only where its type arguments meet the \
                             bounds of that `impl`",
                            tn, bn
                        ))
                    }
                    // An `impl` names a struct or an enum, so for anything
                    // else — a trait object included — suggesting one would
                    // send the reader to write something that does not parse.
                    TyKind::Dyn(tr) if tr == *bound => d.with_note(format!(
                        "`{}` stands for a type that implements `{}`, and `{}` is not \
                         one: it holds a value whose type is known only by its trait. \
                         Take a `{}` parameter instead, or pass the value before it \
                         became one",
                        g.name, bn, tn, tn
                    )),
                    TyKind::Dyn(tr) => d.with_note(format!(
                        "{} has only what `{}` declares",
                        self.types.with_article(*t),
                        self.types.trait_def(tr).name
                    )),
                    _ if self.type_index_of(*t).is_some() => {
                        d.with_note(format!("write `impl {} for {}`", bn, tn))
                    }
                    _ => d.with_note(format!(
                        "an `impl` is written for a struct or an enum, so {} cannot \
                         implement `{}`; wrap it in a struct that does",
                        self.types.with_article(*t),
                        bn
                    )),
                };
                self.diags.push(d);
            }
        }
    }

    /// A type that cannot cross a task boundary, and the field responsible.
    ///
    /// Naming the field is the whole value of the message: "not Share" says
    /// nothing a reader can act on, and "because this field is mutable" says
    /// exactly what to change.
    fn report_not_share(&mut self, ty: TyId, param: &str, span: Span, bound: Span) {
        let name = self.types.name(ty);
        let mut d = Diagnostic::error(
            codes::E0520,
            format!("`{}` cannot be moved to another task", name),
        )
        .with_primary(span, format!("`{}` is not Share", name))
        .with_secondary(bound, format!("`{}` is required to be Share here", param));
        // Two different causes, and they want opposite advice. A mutable field
        // is a data race and the fix is to stop mutating; a host reference is
        // not a race at all — it is simply meaningless in another isolate, and
        // no change to the type makes it travel.
        if self.types.mentions_host_value(ty) {
            self.diags.push(d.with_note(
                "it holds a `JsValue`, which belongs to the isolate that created it. \
                 A worker has its own JavaScript heap and nothing there for the \
                 reference to name, so send what the other task needs — a string, a \
                 number, a copy — and leave the host object here",
            ));
            return;
        }
        // A type parameter is not known to be anything, so it is not known to
        // be Share either — until its declaration says so.
        let mut params = Vec::new();
        params_in(self.types, ty, &mut params);
        if let Some(p) = params.first() {
            if let Some(def) = self.generic_defs.get(*p as usize) {
                let (pname, pspan) = (def.name.clone(), def.span);
                self.diags.push(
                    d.with_secondary(pspan, format!("`{}` is declared here", pname))
                        .with_note(format!(
                            "`{}` could be any type, and not every type may cross; write \
                             `{}: Share` to accept only the ones that can",
                            pname, pname
                        )),
                );
                return;
            }
        }
        if let Some((field, owner)) = self.first_mutable_field(ty) {
            d = d.with_secondary(
                field,
                format!("because this field is mutable, `{}` may not be shared", owner),
            );
        }
        self.diags.push(d.with_note(
            "two tasks holding one mutable value is a data race. Either make the field \
             immutable and return a new value, or keep the type on one task",
        ));
    }

    /// The first `var` field reachable from a type, and the type declaring it.
    fn first_mutable_field(&self, ty: TyId) -> Option<(Span, String)> {
        let mut seen = Vec::new();
        self.mutable_field_inner(ty, &mut seen)
    }

    fn mutable_field_inner(&self, ty: TyId, seen: &mut Vec<TyId>) -> Option<(Span, String)> {
        if seen.contains(&ty) {
            return None;
        }
        seen.push(ty);
        match self.types.kind(ty).clone() {
            TyKind::Struct(s) => {
                let def = self.types.struct_def(s);
                let name = def.name.clone();
                for f in &def.fields {
                    if f.mutable {
                        return Some((f.span, name));
                    }
                }
                let inner: Vec<TyId> = def.fields.iter().map(|f| f.ty).collect();
                inner.into_iter().find_map(|t| self.mutable_field_inner(t, seen))
            }
            TyKind::Enum(e) => {
                let def = self.types.enum_def(e);
                let name = def.name.clone();
                for v in &def.variants {
                    for f in &v.fields {
                        if f.mutable {
                            return Some((f.span, name.clone()));
                        }
                    }
                }
                let inner: Vec<TyId> = def
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|f| f.ty))
                    .collect();
                inner.into_iter().find_map(|t| self.mutable_field_inner(t, seen))
            }
            TyKind::Slice(e) | TyKind::Optional(e) | TyKind::Fallible(e) => {
                self.mutable_field_inner(e, seen)
            }
            TyKind::Map(k, v) => self
                .mutable_field_inner(k, seen)
                .or_else(|| self.mutable_field_inner(v, seen)),
            TyKind::Tuple(es) => es.into_iter().find_map(|t| self.mutable_field_inner(t, seen)),
            _ => None,
        }
    }

    /// The enclosing function's signature, so a nested block still checks
    /// `return` against the right type — the closure's own, inside one.
    fn current_signature(&self) -> Signature {
        if let Some(sig) = &self.closure_sig {
            return sig.clone();
        }
        let s = &self.sigs[self.fn_index];
        Signature {
            params: s.params.clone(),
            ret: s.ret,
            is_async: s.is_async,
            fallible: s.fallible,
            name_span: s.name_span,
            self_ty: s.self_ty,
            generics: s.generics.clone(),
        }
    }

    // ---- patterns ---------------------------------------------------------

    /// Check a pattern against the scrutinee's type and bind its names.
    fn pattern(&mut self, p: &ast::Pattern, scrut: TyId) -> hir::Pattern {
        self.pattern_with(p, scrut, scrut)
    }

    /// As [`Self::pattern`], but a bare binding takes `bind_ty` rather than the
    /// scrutinee's type. The two differ only when an optional has already had
    /// its nil case matched by an earlier arm.
    fn pattern_with(&mut self, p: &ast::Pattern, scrut: TyId, bind_ty: TyId) -> hir::Pattern {
        let checked = self.pattern_shape(p, scrut, bind_ty);
        // A pattern refused as a whole binds nothing, but the arm still reads
        // the names written in it. Poisoning them keeps the one diagnostic
        // that was reported from turning into one more per use.
        if matches!(checked, hir::Pattern::Wildcard) && !matches!(p, ast::Pattern::Wildcard(_)) {
            self.bind_poisoned(p);
        }
        checked
    }

    /// Give every name a pattern binds the poisoned type, as if assigned.
    fn bind_poisoned(&mut self, p: &ast::Pattern) {
        match p {
            ast::Pattern::Binding(name) => {
                if let Some(local) = self.resolved.lookup_binding(name.span) {
                    self.set_local_ty(local, TyId::ERROR);
                }
            }
            ast::Pattern::Variant { args: ast::PatternArgs::Positional(ps), .. }
            | ast::Pattern::Tuple { elems: ps, .. }
            | ast::Pattern::Or { alts: ps, .. } => {
                for x in ps {
                    self.bind_poisoned(x);
                }
            }
            ast::Pattern::Variant { args: ast::PatternArgs::Named(ps), .. } => {
                for (_, x) in ps {
                    self.bind_poisoned(x);
                }
            }
            ast::Pattern::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(x) => self.bind_poisoned(x),
                        None => {
                            if let Some(local) = self.resolved.lookup_binding(f.name.span) {
                                self.set_local_ty(local, TyId::ERROR);
                            }
                        }
                    }
                }
            }
            ast::Pattern::Wildcard(_)
            | ast::Pattern::Nil(_)
            | ast::Pattern::Literal(_)
            | ast::Pattern::Range { .. }
            | ast::Pattern::Error(_) => {}
        }
    }

    fn pattern_shape(&mut self, p: &ast::Pattern, scrut: TyId, bind_ty: TyId) -> hir::Pattern {
        match p {
            ast::Pattern::Wildcard(_) => hir::Pattern::Wildcard,

            // A name matching a unit variant of the scrutinee's own enum is
            // that variant, not a binding — even where the resolver already
            // declared a local for it, because the resolver declares one for
            // any name it cannot place.
            //
            // The resolver genuinely cannot place this one: two enums may each
            // declare a `Start`, and which is meant depends on what is being
            // matched. That is a type, and types are this pass's business.
            //
            // Before this, an ambiguous name became a catch-all binding: every
            // arm after it was dead, exhaustiveness was satisfied, and the
            // match silently returned the wrong answer. A variant name and a
            // binding name colliding is a much smaller surprise than that, and
            // it is one the reader can see.
            ast::Pattern::Binding(name) if self.unit_variant_of(scrut, &name.name).is_some() => {
                let (ti, vi) = self.unit_variant_of(scrut, &name.name).expect("just checked");
                self.variant_pattern(ti, vi, None, scrut, name.span)
            }

            ast::Pattern::Binding(name) => match self.resolved.lookup_binding(name.span) {
                Some(local) => {
                    self.locals[local as usize].ty = bind_ty;
                    self.init[local as usize] = Init::Assigned;
                    hir::Pattern::Binding {
                        local: hir::LocalId(local),
                        // The local holds `bind_ty`; the scrutinee is `scrut`.
                        // They differ exactly when narrowing applied.
                        unwrap: bind_ty != scrut,
                    }
                }
                // Resolution decided this names a unit variant.
                None => match self.resolved.lookup_use(name.span) {
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_pattern(ti, vi, None, scrut, name.span)
                    }
                    _ => hir::Pattern::Wildcard,
                },
            },

            ast::Pattern::Literal(e) => {
                // Against an optional, a literal is one for the value inside:
                // `1` matches a present `1`, as a variant's name matches a
                // present value of that variant.
                let want = match *self.types.kind(scrut) {
                    TyKind::Optional(inner) => inner,
                    _ => scrut,
                };
                let lit = self.expr(e, Some(want));
                self.expect_ty(lit.ty, want, lit.span, None);
                match lit.kind {
                    ExprKind::Int(v) => hir::Pattern::Int(v),
                    ExprKind::Float(v) => hir::Pattern::Float(v),
                    ExprKind::Str(s) => hir::Pattern::Str(s),
                    ExprKind::Bool(b) => hir::Pattern::Bool(b),
                    ExprKind::Unary {
                        op: hir::UnOp::NegInt | hir::UnOp::NegIntWrap,
                        operand,
                    } => match operand.kind {
                        ExprKind::Int(v) => hir::Pattern::Int(v.wrapping_neg()),
                        _ => hir::Pattern::Wildcard,
                    },
                    ExprKind::Unary { op: hir::UnOp::NegFloat, operand } => match operand.kind {
                        ExprKind::Float(v) => hir::Pattern::Float(-v),
                        _ => hir::Pattern::Wildcard,
                    },
                    _ => {
                        self.diags.push(
                            Diagnostic::error(codes::E0100, "only a literal may appear here")
                                .with_primary(e.span(), "not a literal")
                                .with_note("patterns match against constants, not expressions"),
                        );
                        hir::Pattern::Wildcard
                    }
                }
            }

            ast::Pattern::Range { start, end, inclusive, span } => {
                let a = self.expr(start, Some(TyId::INT));
                let b = self.expr(end, Some(TyId::INT));
                // `-5..=-1`: a negative end is a negation of a literal, which
                // the literal pattern above already folds. A range has to as
                // well, or it rejects the one spelling a negative bound has.
                // A release build negates with `NegIntWrap`, and a pattern
                // means the same in both builds, so both are folded.
                let bound = |e: &hir::Expr| match &e.kind {
                    ExprKind::Int(v) => Some(*v),
                    ExprKind::Unary {
                        op: hir::UnOp::NegInt | hir::UnOp::NegIntWrap,
                        operand,
                    } => match operand.kind {
                        ExprKind::Int(v) => v.checked_neg(),
                        _ => None,
                    },
                    _ => None,
                };
                // The range is compared against the scrutinee as integers, so
                // the scrutinee has to be one. Nothing checked that before: a
                // `float` or a `str` reached the backends compared as an
                // integer, and the native one refused to verify what it made.
                //
                // An `Option<int>` is accepted too: a range against an
                // optional matches a present value whose payload is in range,
                // which MIR lowers by testing for `nil` and unwrapping first,
                // as it does for every literal and variant pattern.
                let over_int = scrut == TyId::INT
                    || matches!(self.types.kind(scrut), TyKind::Optional(inner) if *inner == TyId::INT);
                if !over_int && !self.types.is_poisoned(scrut) {
                    let found = self.types.with_article(scrut);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("a range pattern cannot match {}", found),
                        )
                        .with_primary(*span, "this range matches integers")
                        .with_note(
                            "a float range would have to decide what its ends do about \
                             rounding, and there is no answer right for every program — \
                             test the bounds in a guard instead",
                        ),
                    );
                    return hir::Pattern::Wildcard;
                }
                match (bound(&a), bound(&b)) {
                    (Some(x), Some(y)) => {
                        if x > y {
                            self.diags.push(
                                Diagnostic::warning(codes::E0210, "this range is empty")
                                    .with_primary(*span, format!("{}..{} matches nothing", x, y)),
                            );
                        }
                        hir::Pattern::IntRange { start: x, end: y, inclusive: *inclusive }
                    }
                    _ if self.types.is_poisoned(a.ty) || self.types.is_poisoned(b.ty) => {
                        hir::Pattern::Wildcard
                    }
                    _ => {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                "a range pattern matches integers only",
                            )
                            .with_primary(*span, "not an integer range")
                            .with_note(
                                "a float range would have to decide what its ends do \
                                 about rounding, and there is no answer right for every \
                                 program — test the bounds in a guard instead",
                            ),
                        );
                        hir::Pattern::Wildcard
                    }
                }
            }

            ast::Pattern::Variant { path, args, span } => {
                match self.resolved.lookup_use(path.span) {
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_pattern(ti, vi, Some(args), scrut, *span)
                    }
                    Some(Res::Type(ti)) => match self.type_ids[ti as usize] {
                        Some(TypeTarget::Struct(_)) => {
                            self.diags.push(
                                Diagnostic::error(
                                    codes::E0200,
                                    "a struct is not matched like a call",
                                )
                                .with_primary(*span, "not a pattern")
                                .with_note(
                                    "a struct pattern names its fields: write \
                                     `Name{ … }`, which reads the same way the literal \
                                     that built it does",
                                ),
                            );
                            hir::Pattern::Wildcard
                        }
                        // `Shape(r)`, naming the enum where one of its variants
                        // belongs. This used to become a wildcard in silence —
                        // a catch-all that satisfied exhaustiveness on its own
                        // and bound nothing it claimed to — so it is refused.
                        _ => {
                            let decl = self.resolved.type_decl(ti);
                            let (name, kind) = (decl.name.clone(), decl.kind.describe());
                            let a = if kind.starts_with('e') { "an" } else { "a" };
                            let mut d = Diagnostic::error(
                                codes::E0200,
                                format!("`{}` is {} {}, not a variant", name, a, kind),
                            )
                            .with_primary(*span, "a pattern here names one variant");
                            if let Some(TypeTarget::Enum(eid)) = self.type_ids[ti as usize] {
                                let names: Vec<String> = self
                                    .types
                                    .enum_def(eid)
                                    .variants
                                    .iter()
                                    .map(|v| v.name.clone())
                                    .collect();
                                d = d.with_note(format!(
                                    "write one of its variants, such as `{}.{}(…)`: {}",
                                    name,
                                    names.first().map(String::as_str).unwrap_or("Variant"),
                                    names.join(", ")
                                ));
                            }
                            self.diags.push(d);
                            hir::Pattern::Wildcard
                        }
                    },
                    // Resolution has already said what is wrong with the path.
                    _ => hir::Pattern::Wildcard,
                }
            }

            ast::Pattern::Struct { path, fields, span, .. } => {
                let ti = match self.resolved.lookup_use(path.span) {
                    Some(Res::Type(ti)) => ti,
                    // `Circle{ radius }` names a variant with a struct's
                    // braces. Taking it as a wildcard would match every value
                    // and bind nothing, so it is refused.
                    Some(Res::Variant(..)) => {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("`{}` is a variant, not a struct", path.name()),
                            )
                            .with_primary(*span, "a variant pattern takes parentheses")
                            .with_note(format!(
                                "write `{}(…)`, naming its fields as `{}(field: pattern)` \
                                 if it declares them",
                                path.name(),
                                path.name()
                            )),
                        );
                        return hir::Pattern::Wildcard;
                    }
                    _ => return hir::Pattern::Wildcard,
                };
                let Some(TypeTarget::Struct(sid)) = self.type_ids[ti as usize] else {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("`{}` is not a struct", path.name()),
                        )
                        .with_primary(*span, "struct patterns need a struct"),
                    );
                    return hir::Pattern::Wildcard;
                };
                // As for variants: the pattern names the declaration and the
                // scrutinee says which specialisation.
                let sid = match *self.types.kind(scrut) {
                    TyKind::Struct(actual)
                        if self.types.struct_template_of(actual) == Some(sid) =>
                    {
                        actual
                    }
                    _ => sid,
                };

                let struct_ty = self.types.struct_ty(sid);
                if !self.types.satisfies(struct_ty, scrut) && !self.types.is_poisoned(scrut) {
                    let (a, b) = (self.types.name(scrut), self.types.name(struct_ty));
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("this pattern matches `{}`, not `{}`", b, a),
                        )
                        .with_primary(*span, "type mismatch in pattern"),
                    );
                    return hir::Pattern::Wildcard;
                }

                let mut out = Vec::new();
                for f in fields {
                    let found = self
                        .types
                        .struct_def(sid)
                        .field(&f.name.name)
                        .map(|(i, d)| (i, d.ty));
                    let Some((index, fty)) = found else {
                        let sname = self.types.struct_def(sid).name.clone();
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("`{}` has no field `{}`", sname, f.name.name),
                            )
                            .with_primary(f.name.span, "no such field"),
                        );
                        continue;
                    };
                    // Taking a value apart reads its fields, so a private one
                    // is as out of reach here as through `.`.
                    self.check_field_visible(sid, index, f.name.span);
                    let sub = match &f.pattern {
                        Some(p) => self.pattern(p, fty),
                        // `Point{ x }` binds `x` to the field's value.
                        None => match self.resolved.lookup_binding(f.name.span) {
                            Some(local) => {
                                self.locals[local as usize].ty = fty;
                                self.init[local as usize] = Init::Assigned;
                                hir::Pattern::Binding {
                                    local: hir::LocalId(local),
                                    unwrap: false,
                                }
                            }
                            None => hir::Pattern::Wildcard,
                        },
                    };
                    out.push((index as u32, sub));
                }
                hir::Pattern::Struct { struct_id: sid, fields: out }
            }

            ast::Pattern::Or { alts, .. } => {
                let pats: Vec<hir::Pattern> = alts.iter().map(|a| self.pattern(a, scrut)).collect();
                // Whichever alternative matched, the arm runs with every name
                // the pattern binds — so every alternative has to bind them.
                // `A(x) | B => x + 1` reached `B` with `x` never written, and
                // the arm read a register nothing had put a value in.
                let names: Vec<Vec<(String, u32)>> = pats
                    .iter()
                    .map(|p| {
                        let mut ids = Vec::new();
                        pattern_bindings(p, &mut ids);
                        let mut named: Vec<(String, u32)> = ids
                            .into_iter()
                            .map(|id| (self.locals[id as usize].name.clone(), id))
                            .collect();
                        named.sort();
                        named
                    })
                    .collect();
                // One missing name explains the pattern; listing every name it
                // affects would be the same mistake again.
                let missing = names.iter().flatten().find_map(|(name, id)| {
                    names
                        .iter()
                        .position(|other| !other.iter().any(|(n, _)| n == name))
                        .map(|j| (name.clone(), *id, j))
                });
                if let Some((name, id, j)) = missing {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("`{}` is not bound in every alternative", name),
                        )
                        .with_primary(self.locals[id as usize].span, "bound here")
                        .with_secondary(alts[j].span(), format!("`{}` is not bound here", name))
                        .with_note(
                            "the arm runs whichever alternative matched, so a name it reads \
                             has to be bound by all of them",
                        ),
                    );
                }
                // Where two alternatives bind the same name, the types have to
                // agree as well, or the arm would read one slot two ways.
                if let Some(first) = names.first() {
                    for other in &names[1..] {
                        for (name, id) in other {
                            let Some((_, want)) = first.iter().find(|(n, _)| n == name) else {
                                continue;
                            };
                            let (a, b) = (self.locals[*want as usize].ty, self.locals[*id as usize].ty);
                            if a != b && !self.types.is_poisoned(a) && !self.types.is_poisoned(b) {
                                let (an, bn) = (self.types.name(a), self.types.name(b));
                                self.diags.push(
                                    Diagnostic::error(
                                        codes::E0200,
                                        format!("`{}` is bound with two different types", name),
                                    )
                                    .with_primary(self.locals[*id as usize].span, format!("a `{}` here", bn))
                                    .with_secondary(self.locals[*want as usize].span, format!("a `{}` here", an)),
                                );
                            }
                        }
                    }
                }
                hir::Pattern::Or(pats)
            }

            ast::Pattern::Tuple { elems, span } => {
                let TyKind::Tuple(element_tys) = self.types.kind(scrut).clone() else {
                    if !self.types.is_poisoned(scrut) {
                        let found = self.types.with_article(scrut);
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("a tuple pattern cannot match {}", found),
                            )
                            .with_primary(*span, "not a tuple"),
                        );
                    }
                    return hir::Pattern::Wildcard;
                };
                if elems.len() != element_tys.len() {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0113,
                            format!(
                                "this tuple has {} element{}, but the pattern names {}",
                                element_tys.len(),
                                if element_tys.len() == 1 { "" } else { "s" },
                                elems.len()
                            ),
                        )
                        .with_primary(*span, "arity does not match"),
                    );
                    return hir::Pattern::Wildcard;
                }
                let subs = elems
                    .iter()
                    .zip(&element_tys)
                    .map(|(p, ty)| self.pattern(p, *ty))
                    .collect();
                hir::Pattern::Tuple { ty: scrut, elems: subs }
            }
            ast::Pattern::Nil(span) => {
                if !matches!(self.types.kind(scrut), TyKind::Optional(_))
                    && !self.types.is_poisoned(scrut)
                {
                    let found = self.types.with_article(scrut);
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!("`nil` cannot match {}", found),
                        )
                        .with_primary(*span, "only an optional is ever nil"),
                    );
                }
                hir::Pattern::Nil
            }
            ast::Pattern::Error(_) => hir::Pattern::Wildcard,
        }
    }

    /// The declaration index and variant index of a unit variant with this
    /// name on the scrutinee's enum, if there is one.
    fn unit_variant_of(&self, scrut: TyId, name: &str) -> Option<(u32, u32)> {
        let TyKind::Enum(eid) = *self.types.kind(scrut) else { return None };
        let vi = self
            .types
            .enum_def(eid)
            .variants
            .iter()
            .position(|v| v.name == name && v.fields.is_empty())?;
        // Patterns name the declaration, so a specialisation resolves back to
        // the template it came from.
        let template = self.types.enum_template_of(eid).unwrap_or(eid);
        let ti = self
            .type_ids
            .iter()
            .position(|t| matches!(t, Some(TypeTarget::Enum(e)) if *e == template))?;
        Some((ti as u32, vi as u32))
    }

    fn variant_pattern(
        &mut self,
        ti: u32,
        vi: u32,
        args: Option<&ast::PatternArgs>,
        scrut: TyId,
        span: Span,
    ) -> hir::Pattern {
        let Some(TypeTarget::Enum(eid)) = self.type_ids[ti as usize] else {
            return hir::Pattern::Wildcard;
        };
        // A pattern names the declaration, not a specialisation: `Some(n)` is
        // written the same whatever the enum holds. The scrutinee says which
        // specialisation is meant.
        let eid = match *self.types.kind(scrut) {
            TyKind::Enum(actual) if self.types.enum_template_of(actual) == Some(eid) => actual,
            _ => eid,
        };
        let enum_ty = self.types.enum_ty(eid);

        if !self.types.satisfies(enum_ty, scrut) && !self.types.is_poisoned(scrut) {
            let (want, got) = (self.types.name(scrut), self.types.name(enum_ty));
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("this pattern matches `{}`, not `{}`", got, want),
                )
                .with_primary(span, "type mismatch in pattern")
                .with_note(format!("the value being matched is {}", self.types.with_article(scrut))),
            );
            return hir::Pattern::Wildcard;
        }

        let (variant_name, field_tys, field_names, decl_span) = {
            let def = self.types.enum_def(eid);
            let v = &def.variants[vi as usize];
            (
                v.name.clone(),
                v.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
                v.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
                v.span,
            )
        };

        let sub = match args {
            None | Some(ast::PatternArgs::Positional(_)) if field_tys.is_empty() => {
                if let Some(ast::PatternArgs::Positional(ps)) = args {
                    if !ps.is_empty() {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0113,
                                format!("`{}` carries no payload", variant_name),
                            )
                            .with_primary(span, "written with a payload pattern")
                            .with_secondary(decl_span, "declared as a unit variant"),
                        );
                    }
                }
                Vec::new()
            }

            None => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0113,
                        format!(
                            "`{}` carries {} value{}",
                            variant_name,
                            field_tys.len(),
                            if field_tys.len() == 1 { "" } else { "s" }
                        ),
                    )
                    .with_primary(span, "the payload must be matched too")
                    .with_secondary(decl_span, "declared here")
                    .with_note(format!(
                        "write `{}({})` to bind it, or `{}(_)` to ignore it",
                        variant_name,
                        field_names.join(", "),
                        variant_name
                    )),
                );
                field_tys.iter().map(|_| hir::Pattern::Wildcard).collect()
            }

            Some(ast::PatternArgs::Positional(ps)) => {
                if ps.len() != field_tys.len() {
                    self.arity_error(
                        &variant_name,
                        ps.len(),
                        field_tys.len(),
                        span,
                        Some(decl_span),
                    );
                    for p in ps {
                        self.bind_poisoned(p);
                    }
                    field_tys.iter().map(|_| hir::Pattern::Wildcard).collect()
                } else {
                    ps.iter()
                        .zip(&field_tys)
                        .map(|(p, ty)| self.pattern(p, *ty))
                        .collect()
                }
            }

            Some(ast::PatternArgs::Named(named)) => {
                let mut out: Vec<hir::Pattern> =
                    field_tys.iter().map(|_| hir::Pattern::Wildcard).collect();
                for (name, p) in named {
                    match field_names.iter().position(|f| *f == name.name) {
                        Some(i) => out[i] = self.pattern(p, field_tys[i]),
                        None => {
                            self.diags.push(
                                Diagnostic::error(
                                    codes::E0200,
                                    format!("`{}` has no field `{}`", variant_name, name.name),
                                )
                                .with_primary(name.span, "no such field")
                                .with_note(format!(
                                    "`{}` carries: {}",
                                    variant_name,
                                    field_names.join(", ")
                                )),
                            );
                            self.bind_poisoned(p);
                        }
                    }
                }
                out
            }
        };

        hir::Pattern::Variant { enum_id: eid, variant: vi, fields: sub }
    }

    /// Reports whether the unguarded arms cover every value, so a caller can
    /// tell whether control is obliged to enter one of them.
    fn check_exhaustive(
        &mut self,
        m: &ast::MatchExpr,
        arms: &[hir::MatchArm],
        scrut: TyId,
    ) -> bool {
        if self.types.is_poisoned(scrut) {
            return false;
        }
        // A guarded arm may fail at run time, so it cannot make a match
        // exhaustive and is excluded from the coverage set.
        let unguarded: Vec<&hir::Pattern> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| &a.pattern)
            .collect();

        self.report_unreachable_arms(m, arms, scrut);

        let missing = exhaustive::missing_patterns(scrut, &unguarded, self.types);
        if missing.is_empty() {
            return true;
        }

        let names: Vec<String> = missing.iter().map(|x| format!("`{}`", x.0)).collect();
        let all_guarded = !arms.is_empty() && arms.iter().all(|a| a.guard.is_some());

        let mut d = Diagnostic::error(
            codes::E0210,
            format!(
                "non-exhaustive match: {} not covered",
                names.join(", ")
            ),
        )
        .with_primary(m.scrutinee.span(), "this value is not fully matched")
        .with_note(
            "exhaustiveness is what makes adding a variant safe: the compiler shows you every \
             place that must change",
        );
        if all_guarded {
            d = d.with_note(
                "every arm here has a guard, and a guard may fail at run time, so none of them \
                 counts towards coverage",
            );
        }
        self.diags.push(d);
        false
    }

    /// Warn about every arm the ones above it leave nothing for.
    ///
    /// The usual cause is a name meant as a variant that is not one — `Dir`
    /// where the enum says `Directory` — which is a binding, takes every value,
    /// and silently makes each arm after it dead. So when a catch-all binding
    /// above is spelled almost like a variant, the warning says which.
    fn report_unreachable_arms(&mut self, m: &ast::MatchExpr, arms: &[hir::MatchArm], scrut: TyId) {
        let rows: Vec<(&hir::Pattern, bool)> =
            arms.iter().map(|a| (&a.pattern, a.guard.is_some())).collect();
        for i in exhaustive::unreachable_arms(scrut, &rows, self.types) {
            let Some(arm) = m.arms.get(i) else { continue };
            let mut d = Diagnostic::warning(codes::E0116, "unreachable match arm")
                .with_primary(arm.pattern.span(), "no value can reach this arm")
                .with_note("every value it matches is taken by an arm above it");
            if let Some((at, name, variant)) = self.near_miss_binding(m, arms, scrut, i) {
                d = d
                    .with_secondary(at, format!("`{}` takes every value that reaches it", name))
                    .with_note(format!(
                        "`{}` is not a variant, so it is a binding; did you mean `{}`?",
                        name, variant
                    ));
            }
            self.diags.push(d);
        }
    }

    /// An arm above `before` that is a bare, unguarded binding spelled almost
    /// like a variant of the scrutinee's enum: its span, its name, and the
    /// variant.
    fn near_miss_binding(
        &self,
        m: &ast::MatchExpr,
        arms: &[hir::MatchArm],
        scrut: TyId,
        before: usize,
    ) -> Option<(Span, String, String)> {
        let inner = match *self.types.kind(scrut) {
            TyKind::Optional(inner) => inner,
            _ => scrut,
        };
        let TyKind::Enum(e) = *self.types.kind(inner) else { return None };
        let variants: Vec<String> =
            self.types.enum_def(e).variants.iter().map(|v| v.name.clone()).collect();
        for (arm, checked) in m.arms.iter().zip(arms).take(before) {
            let ast::Pattern::Binding(name) = &arm.pattern else { continue };
            // Only a name written like a variant — capitalised — is taken for
            // one that was misspelled; `other` and `x` mean what they say.
            if checked.guard.is_some() || !name.name.starts_with(char::is_uppercase) {
                continue;
            }
            let close = variants.iter().find(|v| {
                v.starts_with(name.name.as_str())
                    || name.name.starts_with(v.as_str())
                    || edit_distance(&name.name.to_lowercase(), &v.to_lowercase()) <= 2
            });
            if let Some(v) = close {
                return Some((name.span, name.name.clone(), v.clone()));
            }
        }
        None
    }

    // ---- structs ----------------------------------------------------------

    /// `Point{ x: 1.0, y: 2.0 }`.
    ///
    /// Every field must be given unless `..base` supplies the rest. There are
    /// no zero values in Kite, which removes Go's most common production bug:
    /// a forgotten field silently becoming `0`, `""`, or `nil`.
    /// A struct literal. For a generic struct the type arguments are inferred
    /// from the field values, or taken from the expected type.
    ///
    /// There is no `Pair<int, str>{...}` spelling because `<` in expression
    /// position is a comparison — the same reason Kite has no turbofish for
    /// functions. Inference is the consistent answer rather than a workaround.
    fn struct_literal_with(&mut self, lit: &ast::StructLit, expected: Option<TyId>) -> hir::Expr {
        let Some(Res::Type(ti)) = self.resolved.lookup_use(lit.path.span) else {
            return self.lit(ExprKind::Error, TyId::ERROR, lit.span);
        };
        let Some(TypeTarget::Struct(sid)) = self.type_ids[ti as usize] else {
            let kind = self.resolved.type_decl(ti).kind.describe();
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}` is not a struct", lit.path.name()),
                )
                .with_primary(lit.path.span, format!("this is a {}", kind)),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, lit.span);
        };

        // A generic struct is specialised here: the parameters are solved from
        // the field values, and everything below works on the specialisation.
        let sid = if self.types.struct_def(sid).generic_count > 0 {
            match self.solve_struct_args(sid, lit, expected) {
                Some(id) => id,
                None => return self.lit(ExprKind::Error, TyId::ERROR, lit.span),
            }
        } else {
            sid
        };

        let struct_ty = self.types.struct_ty(sid);
        let field_count = self.types.struct_def(sid).fields.len();
        // A bound on the declaration's parameters holds for every value that
        // is built, which is here: `struct Holder<T: Show>` is a promise that
        // a `Holder`'s `T` can be shown, and a body relying on it would call
        // `show` on whatever was put in if nothing kept the promise.
        self.check_type_bounds(ti, struct_ty, lit.span);

        // A struct with a private field can only be built where the field
        // can be named — `..base` included, which would copy a value its
        // author meant nobody outside to be able to forge or alter.
        self.check_constructible(sid, lit.path.span);

        // `Point{ ..p, y: 5.0 }` starts from an existing value.
        let base = lit.base.as_ref().map(|b| self.expr(b, Some(struct_ty)));
        if let Some(b) = &base {
            if !self.types.satisfies(b.ty, struct_ty) && !self.types.is_poisoned(b.ty) {
                let (found, want) = (self.types.name(b.ty), self.types.name(struct_ty));
                self.diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!("expected `{}`, found `{}`", want, found),
                    )
                    .with_primary(b.span, format!("`..` needs a `{}`", want)),
                );
            }
        }

        // Resolve each written field to its declared position.
        let mut given: Vec<Option<hir::Expr>> = (0..field_count).map(|_| None).collect();
        for init in &lit.fields {
            let found = self
                .types
                .struct_def(sid)
                .field(&init.name.name)
                .map(|(i, f)| (i, f.ty, f.is_pub, f.span));

            let Some((index, ty, _is_pub, decl_span)) = found else {
                let name = self.types.struct_def(sid).name.clone();
                let known: Vec<String> = self
                    .types
                    .struct_def(sid)
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
                self.diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!("`{}` has no field `{}`", name, init.name.name),
                    )
                    .with_primary(init.name.span, "no such field")
                    .with_note(format!("`{}` has: {}", name, known.join(", "))),
                );
                let _ = self.expr(&init.value, None);
                continue;
            };

            if given[index].is_some() {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0112,
                        format!("field `{}` is given more than once", init.name.name),
                    )
                    .with_primary(init.name.span, "duplicated here"),
                );
            }
            let value = self.expr(&init.value, Some(ty));
            // Subsumption applies here as everywhere else: a `T` written for
            // an `Option<T>` field is wrapped, and a concrete value for a
            // `dyn Trait` field widens. Without this the VM tolerated the
            // mismatch — its registers are untyped — and Wasm rejected it.
            let value = self.coerce(value, Some(ty));
            self.expect_ty(value.ty, ty, value.span, Some(decl_span));
            given[index] = Some(value);
        }

        // Fill the gaps from `..base`, or report them.
        let mut fields = Vec::with_capacity(field_count);
        let mut missing = Vec::new();
        for (i, slot) in given.into_iter().enumerate() {
            match slot {
                Some(e) => fields.push(e),
                None if base.is_some() => {
                    let ty = self.types.struct_def(sid).fields[i].ty;
                    // Each gap re-reads the base. Evaluating it once and
                    // projecting is a MIR concern, not a semantic one.
                    let base_expr = self.clone_base_read(lit, struct_ty);
                    fields.push(hir::Expr {
                        kind: ExprKind::FieldGet {
                            base: Box::new(base_expr),
                            index: i as u32,
                        },
                        ty,
                        span: lit.span,
                    });
                }
                None => missing.push(self.types.struct_def(sid).fields[i].name.clone()),
            }
        }

        if !missing.is_empty() {
            let name = self.types.struct_def(sid).name.clone();
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!(
                        "missing field{} {} in `{}`",
                        if missing.len() == 1 { "" } else { "s" },
                        missing
                            .iter()
                            .map(|m| format!("`{}`", m))
                            .collect::<Vec<_>>()
                            .join(", "),
                        name
                    ),
                )
                .with_primary(lit.span, "every field must be given")
                .with_note(
                    "Kite has no zero values: a struct literal that omits a field is an error, \
                     not a silent default",
                ),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, lit.span);
        }

        hir::Expr {
            kind: ExprKind::StructNew { struct_id: sid, fields },
            ty: struct_ty,
            span: lit.span,
        }
    }

    /// Re-read the `..base` expression for a gap in a functional update.
    fn clone_base_read(&mut self, lit: &ast::StructLit, struct_ty: TyId) -> hir::Expr {
        match &lit.base {
            Some(b) => self.expr(b, Some(struct_ty)),
            None => self.lit(ExprKind::Error, TyId::ERROR, lit.span),
        }
    }

    /// `p.x`
    fn field_access(&mut self, base: &ast::Expr, name: &ast::Ident, span: Span) -> hir::Expr {
        let obj = self.expr(base, None);

        if self.types.is_poisoned(obj.ty) {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        // A tuple's elements are positional: `pair.0` reads the first.
        if let TyKind::Tuple(elems) = self.types.kind(obj.ty).clone() {
            match name.name.parse::<usize>() {
                Ok(index) if index < elems.len() => {
                    return hir::Expr {
                        kind: ExprKind::FieldGet { base: Box::new(obj), index: index as u32 },
                        ty: elems[index],
                        span,
                    }
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            format!(
                                "a `{}` has no `{}`",
                                self.types.name(obj.ty),
                                name.name
                            ),
                        )
                        .with_primary(name.span, "not an element of this tuple")
                        .with_note(format!(
                            "its elements are 0 to {}, or take it apart with `let (a, b) = …`",
                            elems.len().saturating_sub(1)
                        )),
                    );
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
            }
        }

        let TyKind::Struct(sid) = *self.types.kind(obj.ty) else {
            let found = self.types.with_article(obj.ty);
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}` has no fields", self.types.name(obj.ty)),
                )
                .with_primary(obj.span, format!("this is {}", found))
                .with_secondary(name.span, "field access needs a struct"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        match self.types.struct_def(sid).field(&name.name).map(|(i, f)| (i, f.ty)) {
            Some((index, ty)) => {
                self.check_field_visible(sid, index, name.span);
                // The third of the family the editor could not describe, and
                // for the same reason: a field is found through the receiver's
                // type, which only the checker has.
                let owner = self.types.name(obj.ty);
                let shown = self.types.name(ty);
                self.solved
                    .locals
                    .push((name.span, format!("{}.{}", owner, name.name), shown));
                hir::Expr {
                    kind: ExprKind::FieldGet { base: Box::new(obj), index: index as u32 },
                    ty,
                    span,
                }
            }
            None => {
                let sname = self.types.struct_def(sid).name.clone();
                let known: Vec<String> = self
                    .types
                    .struct_def(sid)
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
                let mut d = Diagnostic::error(
                    codes::E0200,
                    format!("`{}` has no field `{}`", sname, name.name),
                )
                .with_primary(name.span, "no such field");
                if self
                    .type_index_of(obj.ty)
                    .is_some_and(|ti| self.resolved.method_on(ti, &name.name).is_some())
                {
                    d = d.with_note("this is a method; call it with `()`");
                }
                d = d.with_note(if known.is_empty() {
                    format!("`{}` has no fields", sname)
                } else {
                    format!("`{}` has: {}", sname, known.join(", "))
                });
                self.diags.push(d);
                self.lit(ExprKind::Error, TyId::ERROR, span)
            }
        }
    }

    fn assign_field(
        &mut self,
        base: &ast::Expr,
        name: &ast::Ident,
        span: Span,
        a: &ast::AssignStmt,
    ) -> Option<(hir::Stmt, Flow)> {

        let obj = self.expr(base, None);
        if self.types.is_poisoned(obj.ty) {
            return None;
        }

        let TyKind::Struct(sid) = *self.types.kind(obj.ty) else {
            let found = self.types.with_article(obj.ty);
            self.diags.push(
                Diagnostic::error(codes::E0200, "only a struct has fields to assign")
                    .with_primary(obj.span, format!("this is {}", found)),
            );
            return None;
        };

        let found = self
            .types
            .struct_def(sid)
            .field(&name.name)
            .map(|(i, f)| (i, f.ty, f.mutable, f.span));
        let Some((index, ty, mutable, decl_span)) = found else {
            let sname = self.types.struct_def(sid).name.clone();
            self.diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!("`{}` has no field `{}`", sname, name.name),
                )
                .with_primary(name.span, "no such field"),
            );
            return None;
        };
        if !self.check_field_visible(sid, index, name.span) {
            return None;
        }

        if !mutable {
            let sname = self.types.struct_def(sid).name.clone();
            self.diags.push(
                Diagnostic::error(
                    codes::E0114,
                    format!("cannot assign to immutable field `{}`", name.name),
                )
                .with_primary(name.span, "this field cannot change")
                .with_secondary(decl_span, "declared immutable here")
                .with_note(format!(
                    "fields are immutable unless marked `var`; write `var {}: {}` on `{}`, or \
                     build a new value with `{}{{ ..old, {}: new }}`",
                    name.name,
                    self.types.name(ty),
                    sname,
                    sname,
                    name.name
                )),
            );
            return None;
        }

        // A `var` field is only assignable through a binding that may itself
        // change. Inside a method that is what `var self` declares: a plain
        // `self` receiver promises the caller nothing was modified, and
        // honouring the field's `var` while ignoring the receiver's would let
        // it be modified anyway.
        let _ = self.require_mutable_base(base);

        let value = self.expr(&a.value, Some(ty));
        let Some(binop) = a.op.to_binary() else {
            let value = self.coerce(value, Some(ty));
            self.expect_ty(value.ty, ty, value.span, Some(decl_span));
            return Some((
                hir::Stmt::SetField { base: obj, index: index as u32, value, span: a.span },
                Flow::Falls,
            ));
        };
        // `make().n += 1` reads and writes one field of one struct, so the
        // struct is worked out once, into a hidden local both sides use —
        // a struct is a reference, so the write lands in the same one.
        // Checking the base twice called `make` twice.
        let holder = self.synthetic_local("base", obj.ty, span);
        let read = hir::Expr { kind: ExprKind::Local(hir::LocalId(holder)), ty: obj.ty, span: obj.span };
        let current = hir::Expr {
            kind: ExprKind::FieldGet { base: Box::new(read.clone()), index: index as u32 },
            ty,
            span,
        };
        let sum = self.binary(binop, current, value, a.span);
        let sum = self.coerce(sum, Some(ty));
        self.expect_ty(sum.ty, ty, sum.span, Some(decl_span));
        Some((
            hir::Stmt::Block(hir::Block {
                stmts: vec![
                    hir::Stmt::Let { local: hir::LocalId(holder), init: Some(obj), span },
                    hir::Stmt::SetField { base: read, index: index as u32, value: sum, span: a.span },
                ],
            }),
            Flow::Falls,
        ))
    }

    fn if_expr(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        else_: &ast::ElseBranch,
        span: Span,
        expected: Option<TyId>,
    ) -> hir::Expr {
        // An inline `if` narrows an optional exactly as the statement form
        // does, which is why no `?.` or `??` operator is needed — and a test
        // of an error cleans the value it guards in the branch where it is
        // nil, which is §7.5's own `if err != nil { 8080 } else { port }`.
        let narrowing = self.nil_test(cond);
        let tested = self.error_tested_by(cond);
        let c = self.condition(cond);

        // The branches are alternatives, joined afterwards, exactly as the
        // statement form's are: a branch is one expression, but a `match` in
        // it can hold statements, and what they do happens on one side only.
        let entry = self.flow_state();
        self.enter_branch(narrowing, tested, true);
        // The type the context wants steers both branches, as it does a
        // `let`'s initialiser — `let x: Option<int> = if c { 5 } else { nil }`
        // has no other place to learn what the `nil` is. Failing that, the
        // first branch steers the second, as the first arm of a `match` does.
        let t = self.block_value(then, expected);
        let t = self.coerce(t, expected);
        let then_exit = self.flow_state();
        self.set_flow_state(entry);

        self.enter_branch(narrowing, tested, false);
        let want = expected.or((!self.types.is_poisoned(t.ty)).then_some(t.ty));
        let e = match else_ {
            ast::ElseBranch::Block(b) => self.block_value(b, want),
            ast::ElseBranch::If(nested) => match nested.else_.as_deref() {
                Some(inner_else) => {
                    self.if_expr(&nested.cond, &nested.then, inner_else, nested.span, want)
                }
                // `if a { 1 } else if b { 2 }` has no value when neither test
                // holds. It used to become an error node with nothing said,
                // and the program ran with a hole where the value should be.
                None => {
                    self.diags.push(
                        Diagnostic::error(codes::E0200, "an `if` used as a value needs an `else`")
                            .with_primary(nested.span, "no value when this test is false")
                            .with_note(
                                "every path through a value `if` has to produce one: end the \
                                 chain with `else { … }`",
                            ),
                    );
                    self.lit(ExprKind::Error, TyId::ERROR, nested.span)
                }
            },
        };
        let e = self.coerce(e, want);
        let else_exit = self.flow_state();
        let merged = match (t.ty == TyId::NEVER, e.ty == TyId::NEVER) {
            (true, _) => else_exit,
            (_, true) => then_exit,
            (false, false) => self.join_flow(&then_exit, &else_exit),
        };
        self.set_flow_state(merged);

        // One branch may need converting to the other's type — an `int` beside
        // an `Option<int>`, a concrete value beside a `dyn` — and the
        // conversion has to be in the tree for the backends to lay the two
        // out alike. Either side may be the one converted.
        let (t, e) = if t.ty == e.ty || self.types.is_poisoned(t.ty) || self.types.is_poisoned(e.ty) {
            (t, e)
        } else {
            let e = self.coerce(e, Some(t.ty));
            let t = if e.ty == t.ty { t } else { self.coerce(t, Some(e.ty)) };
            (t, e)
        };
        let ty = if t.ty == TyId::NEVER {
            e.ty
        } else if e.ty == TyId::NEVER {
            t.ty
        } else {
            if e.ty != t.ty && !self.types.is_poisoned(t.ty) && !self.types.is_poisoned(e.ty) {
                self.diags.push(
                    Diagnostic::error(codes::E0200, "`if` branches have different types")
                        .with_primary(e.span, format!("this branch is {}", self.types.with_article(e.ty)))
                        .with_secondary(t.span, format!("this branch is {}", self.types.with_article(t.ty)))
                        .with_note("every branch of a value `if` must produce the same type"),
                );
            }
            // One poisoned branch poisons the whole: its type is unknown, and
            // taking the other's would let the one mistake be reported again
            // wherever the value goes.
            if self.types.is_poisoned(e.ty) { e.ty } else { t.ty }
        };

        hir::Expr {
            kind: ExprKind::If { cond: Box::new(c), then: Box::new(t), else_: Box::new(e) },
            ty,
            span,
        }
    }

    /// A block used for its value must be a single expression. Kite has no
    /// implicit tail expression in statement blocks.
    fn block_value(&mut self, b: &ast::Block, expected: Option<TyId>) -> hir::Expr {
        match b.stmts.as_slice() {
            [ast::Stmt::Expr(e)] => self.expr(e, expected),
            _ => {
                self.diags.push(
                    Diagnostic::error(codes::E0200, "this block must produce a value")
                        .with_primary(b.span, "expected a single expression")
                        .with_note("an `if` used as a value takes one expression per branch"),
                );
                self.lit(ExprKind::Error, TyId::ERROR, b.span)
            }
        }
    }

    // ---- operators --------------------------------------------------------

    fn unary(&mut self, op: ast::UnaryOp, val: hir::Expr, span: Span) -> hir::Expr {
        if self.types.is_poisoned(val.ty) {
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        }
        let (hop, ty) = match (op, val.ty) {
            // Negating `int`'s minimum overflows: a trap in a debug build and
            // a wrap in a release one, like every other overflow.
            (ast::UnaryOp::Neg, TyId::INT) if self.release => (hir::UnOp::NegIntWrap, TyId::INT),
            (ast::UnaryOp::Neg, TyId::INT) => (hir::UnOp::NegInt, TyId::INT),
            (ast::UnaryOp::Neg, TyId::FLOAT) => (hir::UnOp::NegFloat, TyId::FLOAT),
            (ast::UnaryOp::Not, TyId::BOOL) => (hir::UnOp::Not, TyId::BOOL),
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0201,
                        format!("`{}` cannot be applied to `{}`", op.text(), self.types.name(val.ty)),
                    )
                    .with_primary(val.span, format!("this is {}", self.types.with_article(val.ty)))
                    .with_note(match op {
                        ast::UnaryOp::Neg => "`-` applies to `int` and `float`",
                        ast::UnaryOp::Not => "`!` applies to `bool`",
                    }),
                );
                return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
            }
        };
        hir::Expr {
            kind: ExprKind::Unary { op: hop, operand: Box::new(val) },
            ty,
            span,
        }
    }

    fn binary(
        &mut self,
        op: ast::BinaryOp,
        l: hir::Expr,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        use ast::BinaryOp as B;
        use hir::BinOp as H;

        if self.types.is_poisoned(l.ty) || self.types.is_poisoned(r.ty) {
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        }

        // Equality is structural for all types (§5.2), and a `T` stands
        // wherever an `Option<T>` does — so `found == 5` asks whether `found`
        // is present and five, by comparing two optionals.
        let optional_of = |types: &Types, outer: TyId, inner: TyId| {
            matches!(types.kind(outer), TyKind::Optional(t) if *t == inner)
        };
        let (l, r) = if matches!(op, B::Eq | B::Ne) && optional_of(self.types, l.ty, r.ty) {
            let want = l.ty;
            (l, self.coerce(r, Some(want)))
        } else if matches!(op, B::Eq | B::Ne) && optional_of(self.types, r.ty, l.ty) {
            let want = r.ty;
            (self.coerce(l, Some(want)), r)
        } else {
            (l, r)
        };

        if l.ty != r.ty {
            self.mismatched_operands(op, &l, &r, span);
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        }

        let t = l.ty;
        let resolved = match (op, t) {
            // Overflow traps in a debug build and wraps in a release one.
            (B::Add, TyId::INT) if self.release => Some((H::AddIntWrap, TyId::INT)),
            (B::Sub, TyId::INT) if self.release => Some((H::SubIntWrap, TyId::INT)),
            (B::Mul, TyId::INT) if self.release => Some((H::MulIntWrap, TyId::INT)),
            (B::Add, TyId::INT) => Some((H::AddInt, TyId::INT)),
            (B::Sub, TyId::INT) => Some((H::SubInt, TyId::INT)),
            (B::Mul, TyId::INT) => Some((H::MulInt, TyId::INT)),
            (B::Div, TyId::INT) => Some((H::DivInt, TyId::INT)),
            (B::Rem, TyId::INT) => Some((H::RemInt, TyId::INT)),

            (B::Add, TyId::FLOAT) => Some((H::AddFloat, TyId::FLOAT)),
            (B::Sub, TyId::FLOAT) => Some((H::SubFloat, TyId::FLOAT)),
            (B::Mul, TyId::FLOAT) => Some((H::MulFloat, TyId::FLOAT)),
            (B::Div, TyId::FLOAT) => Some((H::DivFloat, TyId::FLOAT)),

            (B::Add, TyId::STR) => Some((H::ConcatStr, TyId::STR)),

            (B::BitAnd, TyId::INT) => Some((H::BitAnd, TyId::INT)),
            (B::BitOr, TyId::INT) => Some((H::BitOr, TyId::INT)),
            (B::BitXor, TyId::INT) => Some((H::BitXor, TyId::INT)),
            // A shift count outside `0..=63` traps in a debug build and is
            // taken modulo 64 in a release one — the overflow rule again.
            (B::Shl, TyId::INT) if self.release => Some((H::ShlWrap, TyId::INT)),
            (B::Shr, TyId::INT) if self.release => Some((H::ShrWrap, TyId::INT)),
            (B::Shl, TyId::INT) => Some((H::Shl, TyId::INT)),
            (B::Shr, TyId::INT) => Some((H::Shr, TyId::INT)),

            (B::Eq, TyId::INT) => Some((H::EqInt, TyId::BOOL)),
            (B::Ne, TyId::INT) => Some((H::NeInt, TyId::BOOL)),
            (B::Lt, TyId::INT) => Some((H::LtInt, TyId::BOOL)),
            (B::Le, TyId::INT) => Some((H::LeInt, TyId::BOOL)),
            (B::Gt, TyId::INT) => Some((H::GtInt, TyId::BOOL)),
            (B::Ge, TyId::INT) => Some((H::GeInt, TyId::BOOL)),

            (B::Eq, TyId::FLOAT) => Some((H::EqFloat, TyId::BOOL)),
            (B::Ne, TyId::FLOAT) => Some((H::NeFloat, TyId::BOOL)),
            (B::Lt, TyId::FLOAT) => Some((H::LtFloat, TyId::BOOL)),
            (B::Le, TyId::FLOAT) => Some((H::LeFloat, TyId::BOOL)),
            (B::Gt, TyId::FLOAT) => Some((H::GtFloat, TyId::BOOL)),
            (B::Ge, TyId::FLOAT) => Some((H::GeFloat, TyId::BOOL)),

            (B::Eq, TyId::BOOL) => Some((H::EqBool, TyId::BOOL)),
            (B::Ne, TyId::BOOL) => Some((H::NeBool, TyId::BOOL)),
            (B::Eq, TyId::STR) => Some((H::EqStr, TyId::BOOL)),
            (B::Ne, TyId::STR) => Some((H::NeStr, TyId::BOOL)),
            // By code point, which is what sorting a list of names needs and
            // is *not* what a person means by alphabetical order in every
            // language. Collation is a table and a locale, and neither belongs
            // in an operator.
            (B::Lt, TyId::STR) => Some((H::LtStr, TyId::BOOL)),
            (B::Le, TyId::STR) => Some((H::LeStr, TyId::BOOL)),
            (B::Gt, TyId::STR) => Some((H::GtStr, TyId::BOOL)),
            (B::Ge, TyId::STR) => Some((H::GeStr, TyId::BOOL)),

            // Aggregates compare structurally, per the specification.
            (B::Eq, _) if self.types.is_equatable(t) => Some((H::EqValue, TyId::BOOL)),
            (B::Ne, _) if self.types.is_equatable(t) => Some((H::NeValue, TyId::BOOL)),
            // Two values of the same type parameter. `Eq` is derived for
            // every type structurally, so this holds for whatever the
            // parameter turns out to be — and monomorphisation has replaced it
            // with a concrete type long before any backend sees it.
            //
            // Every type but a function, a trait object and a host value, that
            // is — so the comparison is written down, and each call is held to
            // it once every body has been seen.
            (B::Eq, _) if matches!(self.types.kind(t), TyKind::Param { .. }) => {
                self.note_compared(t, span);
                Some((H::EqValue, TyId::BOOL))
            }
            (B::Ne, _) if matches!(self.types.kind(t), TyKind::Param { .. }) => {
                self.note_compared(t, span);
                Some((H::NeValue, TyId::BOOL))
            }

            _ => None,
        };

        let Some((hop, ty)) = resolved else {
            let mut d = Diagnostic::error(
                codes::E0201,
                format!("`{}` cannot be applied to two `{}` values", op.text(), self.types.name(t)),
            )
            .with_primary(span, "no such operation");
            if op.is_arithmetic() && t == TyId::STR {
                d = d.with_note("`+` concatenates strings; the other arithmetic operators do not");
            }
            if op == B::Rem && t == TyId::FLOAT {
                d = d.with_note("use `math.rem` for floating-point remainder");
            }
            // Only for the ordering operators. `==` on an unordered type is not
            // failing because the type has no order — it is failing for its own
            // reason, which the notes below give, and "is not ordered" beside
            // them reads as a second, wrong explanation.
            if matches!(op, B::Lt | B::Le | B::Gt | B::Ge) && !self.types.is_ordered(t) {
                d = d.with_note(format!("`{}` is not ordered", self.types.name(t)));
            }
            if matches!(op, B::Eq | B::Ne) && self.types.mentions_host_value(t) {
                // Not an omission: `externref` is outside Wasm's `eq`
                // hierarchy, so there is no comparison to lower this to. The
                // question people mean is identity, and identity is the host's
                // `===` rather than anything structural.
                d = d.with_note(
                    "a host object has no structure Kite can see, so there is nothing to \
                     compare field by field",
                );
                d = d.with_note("`js.same(a, b)` asks the host whether they are the same object");
            } else if matches!(op, B::Eq | B::Ne) && !self.types.is_equatable(t) {
                d = d.with_note(
                    "equality is structural, so every field must itself be equatable; \
                     functions and trait objects are not",
                );
            }
            self.diags.push(d);
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        };

        // A secret compared with `==` is a timing oracle. Provenance is read
        // syntactically — a value that came *straight* from `crypto` — which
        // catches the shape people actually write and claims nothing about the
        // ones it cannot see.
        if matches!(hop, H::EqStr | H::NeStr) && (self.came_from_crypto(&l) || self.came_from_crypto(&r)) {
            self.diags.push(
                Diagnostic::warning(codes::E0600, "comparing a secret with `==`")
                    .with_primary(span, "this comparison stops at the first difference")
                    .with_note(
                        "write `crypto.equal(a, b)`, which takes the same time whichever \
                         way it goes",
                    ),
            );
        }

        // Float equality is a footgun the specification calls out — with an
        // exception the specification also states, and this used to ignore:
        // *neither operand may be a literal*.
        //
        // The carve-out is the whole difference between a useful lint and a
        // noisy one. `a == b` on two computed floats is almost always a bug.
        // `x == 0.0` is almost always deliberate: it is the guard written
        // before a division or a logarithm, where the question really is
        // "exactly zero?" and a tolerance would answer a different one. The
        // lint fired on every such guard in `std/math`, which is how a warning
        // teaches people to stop reading warnings.
        let literal = |e: &hir::Expr| matches!(e.kind, ExprKind::Float(_));
        if matches!(hop, H::EqFloat | H::NeFloat) && !literal(&l) && !literal(&r) {
            self.diags.push(
                Diagnostic::warning(codes::E0201, "comparing floats for exact equality")
                    .with_primary(span, "floating-point equality is rarely what you want")
                    .with_note(
                        "compare within a tolerance instead: the prelude's \
                         `approx_eq(a, b, tolerance)`",
                    ),
            );
        }

        hir::Expr {
            kind: ExprKind::Binary { op: hop, lhs: Box::new(l), rhs: Box::new(r) },
            ty,
            span,
        }
    }

    /// `==` on values whose type is a parameter: the parameter is compared,
    /// and every caller will be held to it.
    fn note_compared(&mut self, ty: TyId, span: Span) {
        let mut params = Vec::new();
        params_in(self.types, ty, &mut params);
        if let Some(row) = self.facts.compared.get_mut(self.fn_index) {
            for p in params {
                if let Some(slot) = row.get_mut(p as usize) {
                    slot.get_or_insert(span);
                }
            }
        }
    }

    /// A call that chose a generic callee's type arguments, kept to be held
    /// to whatever the callee's body turns out to compare.
    fn note_generic_call(&mut self, callee: u32, targs: &[TyId], span: Span) {
        if targs.is_empty() {
            return;
        }
        self.facts.calls.push(GenericCall {
            caller: self.fn_index,
            callee: callee as usize,
            targs: targs.to_vec(),
            span,
        });
    }

    /// Whether a value came straight from `crypto`.
    ///
    /// Deliberately shallow: following a value through bindings and across
    /// function boundaries would be a provenance analysis, and one that
    /// stopped anywhere would give people confidence it has not earned. This
    /// catches the shape people write — comparing a freshly computed digest or
    /// token — says what to write instead, and claims nothing more.
    fn came_from_crypto(&self, e: &hir::Expr) -> bool {
        match &e.kind {
            ExprKind::Call { callee, .. } => self
                .resolved
                .fns
                .get(callee.0 as usize)
                .is_some_and(|f| f.name.starts_with("crypto.")),
            _ => false,
        }
    }

    fn mismatched_operands(
        &mut self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
        span: Span,
    ) {
        let mut d = Diagnostic::error(
            codes::E0201,
            format!("`{}` cannot be applied to `{}` and `{}`", op.text(), self.types.name(l.ty), self.types.name(r.ty)),
        )
        .with_primary(span, "operand types differ")
        .with_secondary(l.span, format!("`{}`", self.types.name(l.ty)))
        .with_secondary(r.span, format!("`{}`", self.types.name(r.ty)));

        if self.types.is_numeric(l.ty) && self.types.is_numeric(r.ty) {
            d = d.with_note(
                "Kite performs no implicit numeric conversion; write an explicit `as` cast",
            );
        }
        self.diags.push(d);
    }

    // ---- helpers ----------------------------------------------------------

    fn lit(&self, kind: ExprKind, ty: TyId, span: Span) -> hir::Expr {
        hir::Expr { kind, ty, span }
    }

    /// A mutating method as a unit-typed expression.
    ///
    /// `xs.push(v)` and `m.remove(k)` are statements the grammar happens to
    /// spell as calls, and HIR has no statement position inside an expression
    /// — so the statement is wrapped in a `match` on `true`, which every
    /// backend already lowers to a plain block.
    fn as_statement(&self, stmt: hir::Stmt, span: Span) -> hir::Expr {
        hir::Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(hir::Expr {
                    kind: ExprKind::Bool(true),
                    ty: TyId::BOOL,
                    span,
                }),
                arms: vec![hir::MatchArm {
                    pattern: hir::Pattern::Wildcard,
                    guard: None,
                    body: hir::Expr {
                        kind: ExprKind::Block(hir::Block { stmts: vec![stmt] }),
                        ty: TyId::UNIT,
                        span,
                    },
                    span,
                }],
            },
            ty: TyId::UNIT,
            span,
        }
    }

    fn text(&self, span: Span) -> &'a str {
        self.sources.span_text(span)
    }

    /// Decode a string literal's contents. Phase 1 handles escapes; string
    /// interpolation arrives with `Display` in Phase 2.
    /// `"a \(x) b"` becomes concatenation. Each hole is rendered with `ToStr`,
    /// and the pieces are folded left to right so the result reads in source
    /// order even though `+` is what actually runs.
    ///
    /// There is no format-string language here and no way to get one, which is
    /// the point: no width specifiers to learn and no injection surface.
    fn interpolated(&mut self, parts: &[ast::StrPart], span: Span) -> hir::Expr {
        let mut acc: Option<hir::Expr> = None;
        for part in parts {
            let piece = match part {
                ast::StrPart::Text(s) => {
                    let value = self.text_run_value(*s);
                    self.lit(ExprKind::Str(value), TyId::STR, *s)
                }
                ast::StrPart::Hole(e) => {
                    let v = self.expr(e, None);
                    self.render(v)
                }
            };
            acc = Some(match acc {
                None => piece,
                Some(lhs) => hir::Expr {
                    span,
                    kind: ExprKind::Binary {
                        op: hir::BinOp::ConcatStr,
                        lhs: Box::new(lhs),
                        rhs: Box::new(piece),
                    },
                    ty: TyId::STR,
                },
            });
        }
        acc.unwrap_or_else(|| self.lit(ExprKind::Str(String::new()), TyId::STR, span))
    }

    /// The `show` method a type gets from implementing `Display`, if it does.
    ///
    /// The trait is found by name, because it is declared in the prelude rather
    /// than in the compiler — which is what lets a program define its own if it
    /// wants to, and what stops the compiler from having an opinion about how
    /// anything reads.
    fn display_method(&self, ty: TyId) -> Option<u32> {
        let trait_index = self.resolved.type_by_name("Display")?;
        if !matches!(self.type_ids.get(trait_index as usize)?, Some(TypeTarget::Trait(_))) {
            return None;
        }
        let type_index = self.type_index_of(ty)?;
        self.resolved.trait_method(type_index, trait_index, "show")
    }

    /// The `message` method a type gets from implementing `Error`, if it does.
    ///
    /// Found by name in the prelude, exactly as `Display` is, so the compiler
    /// holds no opinion about what a failure is beyond the one the standard
    /// library wrote down.
    fn error_method(&self, ty: TyId) -> Option<u32> {
        let trait_index = self.resolved.type_by_name("Error")?;
        if !matches!(self.type_ids.get(trait_index as usize)?, Some(TypeTarget::Trait(_))) {
            return None;
        }
        let type_index = self.type_index_of(ty)?;
        self.resolved.trait_method(type_index, trait_index, "message")
    }

    /// Whether a value of this type may stand where an `error` is wanted.
    fn coerces_to_error(&self, found: TyId) -> bool {
        found != TyId::ERR && self.error_method(found).is_some()
    }

    /// The "carries nothing" operand, for an error slot with no value or no
    /// cause. Typed `ERR` rather than a real optional because no Kite
    /// expression reads it: it is a null reference the backends recognise.
    fn nothing(span: Span) -> hir::Expr {
        hir::Expr { kind: ExprKind::Nil, ty: TyId::ERR, span }
    }

    /// The run-time identity of a concrete type, as a vtable row would carry
    /// it, or `None` for a type with no tag. `TypeTag::encode` never yields
    /// zero — structs count from one — which is what lets zero mean "carries
    /// nothing" without colliding with a real type.
    fn type_tag_of(&self, ty: TyId) -> Option<u32> {
        Some(match *self.types.kind(ty) {
            TyKind::Struct(s) => kite_hir::TypeTag::Struct(s).encode(),
            TyKind::Enum(e) => kite_hir::TypeTag::Enum(e).encode(),
            _ => return None,
        })
    }

    /// A value as text. Primitives render themselves; anything else needs
    /// `Display`.
    fn render(&mut self, v: hir::Expr) -> hir::Expr {
        if self.types.is_poisoned(v.ty) {
            return hir::Expr { span: v.span, kind: ExprKind::Error, ty: TyId::STR };
        }
        if v.ty == TyId::STR {
            return v;
        }
        if matches!(*self.types.kind(v.ty), TyKind::Int | TyKind::Float | TyKind::Bool) {
            let span = v.span;
            return hir::Expr { span, kind: ExprKind::ToStr { value: Box::new(v) }, ty: TyId::STR };
        }
        if let Some(shown) = self.show_through_display(v.clone()) {
            return shown;
        }
        let name = self.types.name(v.ty);
        let mut d = Diagnostic::error(codes::E0207, format!("`{}` has no text form", name))
            .with_primary(v.span, "this cannot be rendered")
            .with_note(
                "`int`, `float`, `bool` and `str` render themselves. Anything else needs \
                 `Display`",
            );
        d = match *self.types.kind(v.ty) {
            // Neither of these can be given an `impl`: a parameter is known by
            // its bounds, and a trait object by its trait.
            TyKind::Param { .. } => d.with_note(format!("add the bound: `{}: Display`", name)),
            TyKind::Dyn(_) => d.with_note(format!(
                "{} shows only what its trait declares; call one of its methods, or \
                 make the trait `Display` itself and hold a `dyn Display`",
                self.types.with_article(v.ty)
            )),
            _ => d.with_note(format!(
                "write `impl Display for {} {{ fn show(self) -> str {{ … }} }}`",
                name
            )),
        };
        self.diags.push(d);
        hir::Expr { span: v.span, kind: ExprKind::Error, ty: TyId::STR }
    }

    /// The `Display` trait, found by name as `display_method` finds it.
    fn display_trait(&self) -> Option<hir::TraitId> {
        let index = self.resolved.type_by_name("Display")?;
        match self.type_ids.get(index as usize)? {
            Some(TypeTarget::Trait(tr)) => Some(*tr),
            _ => None,
        }
    }

    /// A value rendered through its `Display`, if it has one.
    ///
    /// A concrete type calls its own `show`. A `dyn Display`, and a type
    /// parameter bounded by it, have none to name here, so they call through
    /// the trait — the same dispatch any other method of theirs gets, which
    /// monomorphisation turns into a direct call for the parameter.
    fn show_through_display(&mut self, v: hir::Expr) -> Option<hir::Expr> {
        let tr = self.display_trait()?;
        let span = v.span;
        let through_trait = match *self.types.kind(v.ty) {
            TyKind::Dyn(t) => t == tr,
            TyKind::Param { .. } => self.type_implements(v.ty, tr),
            _ => false,
        };
        if through_trait {
            let method = self.types.trait_def(tr).method("show")?.0 as u32;
            return Some(hir::Expr {
                kind: ExprKind::CallVirtual { trait_id: tr, method, args: vec![v], targs: Vec::new() },
                ty: TyId::STR,
                span,
            });
        }
        if !self.type_implements(v.ty, tr) {
            return None;
        }
        let show = self.display_method(v.ty)?;
        let targs = self.receiver_args(v.ty);
        Some(hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(show), args: vec![v], targs },
            ty: TyId::STR,
            span,
        })
    }

    fn string_value(&mut self, span: Span) -> String {
        let raw = self.text(span);
        if let Some(s) = raw.strip_prefix("\"\"\"") {
            let body = s.strip_suffix("\"\"\"").unwrap_or(s);
            let dedented = dedent_block(body);
            return self.decode_escapes(&dedented, span);
        }
        let s = raw.strip_prefix('"').unwrap_or(raw);
        let inner = s.strip_suffix('"').unwrap_or(s);
        self.decode_escapes(inner, span)
    }

    /// The text of one literal run inside an interpolated string. The parser
    /// has already excluded the quotes and the holes.
    fn text_run_value(&mut self, span: Span) -> String {
        let raw = self.text(span).to_string();
        self.decode_escapes(&raw, span)
    }

    fn decode_escapes(&mut self, inner: &str, span: Span) -> String {
        decode_escapes_into(inner, span, self.diags)
    }

    /// Insert the wrap a `T` needs to stand where an `Option<T>` is wanted.
    ///
    /// Doing it here, at every site, keeps the representation change visible in
    /// the IR rather than left for a backend to infer.
    fn coerce(&mut self, e: hir::Expr, expected: Option<TyId>) -> hir::Expr {
        let Some(want) = expected else { return e };
        if e.ty == want || self.types.is_poisoned(e.ty) {
            return e;
        }
        let kind = self.types.kind(want).clone();
        match kind {
            TyKind::Optional(inner) if inner == e.ty => hir::Expr {
                span: e.span,
                kind: ExprKind::Wrap { value: Box::new(e) },
                ty: want,
            },
            // A concrete value becoming a trait object. Nothing about the value
            // changes; the node records that dispatch is now dynamic.
            TyKind::Dyn(tr) if self.coerces_to_dyn(e.ty, want) => hir::Expr {
                span: e.span,
                kind: ExprKind::ToDyn { value: Box::new(e), trait_id: tr },
                ty: want,
            },
            // A value implementing `Error`, standing where an `error` is
            // wanted. The `message` call is inserted **here**, at the point of
            // conversion, so it is an ordinary call in the IR that every
            // backend already knows how to lower — nothing about the error
            // representation changes, and there is no new instruction for three
            // backends to agree about.
            //
            // It also means the message is rendered where the failure
            // happened, which is where its context is freshest — and the value
            // it was rendered *from* is kept beside it, with its type tag, so
            // that `NotFound.is(err)` can ask which failure this was rather
            // than matching on the text.
            TyKind::Err if self.coerces_to_error(e.ty) => {
                let span = e.span;
                let message = self.error_method(e.ty).expect("checked");
                let targs = self.receiver_args(e.ty);
                let tag = self.type_tag_of(e.ty).unwrap_or(0);
                // The value is read twice — once to render it, once to keep it
                // — so it goes into a local first. Rendering may run arbitrary
                // Kite, and evaluating the operand twice would run it twice.
                let carried = e.clone();
                let rendered = hir::Expr {
                    kind: ExprKind::Call { callee: hir::FnId(message), args: vec![e], targs },
                    ty: TyId::STR,
                    span,
                };
                hir::Expr {
                    kind: ExprKind::ErrorNew {
                        message: Box::new(rendered),
                        value: Box::new(carried),
                        tag: Box::new(hir::Expr {
                            kind: ExprKind::Int(tag as i64),
                            ty: TyId::INT,
                            span,
                        }),
                        cause: Box::new(Self::nothing(span)),
                    },
                    ty: TyId::ERR,
                    span,
                }
            }
            _ => e,
        }
    }

    /// A value standing where a `want` is required: converted, then checked.
    ///
    /// The two halves belong together. A value that is acceptable only
    /// because of a conversion — a `T` into an `Option<T>`, a concrete type
    /// into a `dyn Trait` or an `error` — is acceptable only *once converted*,
    /// and a site that checked it without converting it handed a bare `int`
    /// to a slot the backends lay out as a boxed optional: the VM shrugged,
    /// native code crashed, and the Wasm module did not validate. Every site
    /// that takes a value goes through here so that cannot be forgotten.
    fn accept(&mut self, e: hir::Expr, want: TyId, because: Option<Span>) -> hir::Expr {
        let e = self.coerce(e, Some(want));
        self.expect_ty(e.ty, want, e.span, because);
        e
    }

    /// Report a value whose type is not the one required.
    ///
    /// Strict: the conversions are [`Self::coerce`]'s job, and a value that
    /// needed one and did not get it is a mismatch here rather than something
    /// waved through for the backends to disagree about. Only poison and
    /// `!` — which never produce a value — stand in for anything.
    fn expect_ty(&mut self, found: TyId, expected: TyId, span: Span, because: Option<Span>) {
        if found == expected || self.types.is_poisoned(found) || self.types.is_poisoned(expected) {
            return;
        }
        // A type built around one that was already reported — `[Foo]` where
        // `Foo` does not exist — is as poisoned as the bare error, and
        // comparing it would print `expected [<error>]` for the mistake that
        // was reported where `Foo` was written.
        if self.mentions_error(found) || self.mentions_error(expected) {
            return;
        }
        let mut d = Diagnostic::error(
            codes::E0200,
            format!("expected `{}`, found `{}`", self.types.name(expected), self.types.name(found)),
        )
        .with_primary(span, format!("this is {}", self.types.with_article(found)));

        if let Some(b) = because {
            d = d.with_secondary(b, format!("`{}` required here", self.types.name(expected)));
        }
        if let TyKind::Dyn(tr) = *self.types.kind(expected) {
            if self.type_index_of(found).is_some() {
                let (tn, fname) =
                    (self.types.trait_def(tr).name.clone(), self.types.name(found));
                d = d.with_note(format!(
                    "`{}` does not implement `{}`; write `impl {} for {}` to use it here",
                    fname, tn, tn, fname
                ));
            }
        }
        // The same courtesy the `dyn` case gets: a nominal type in an error
        // slot is almost always a type that meant to be an error and has not
        // said so yet.
        if expected == TyId::ERR && self.type_index_of(found).is_some() {
            let name = self.types.name(found);
            d = d.with_note(format!(
                "a type may stand here once it implements `Error`: write \
                 `impl Error for {} {{ fn message(self) -> str {{ … }} }}`",
                name
            ));
            d = d.with_note("or build one from text with `errors.new(\"…\")`");
        }
        if self.types.is_numeric(found) && self.types.is_numeric(expected) {
            d = d.with_note(format!(
                "Kite performs no implicit numeric conversion; write `... as {}`",
                self.types.name(expected)
            ));
        }
        self.diags.push(d);
    }

    /// Whether a type is, or is built from, one that failed to check.
    fn mentions_error(&self, ty: TyId) -> bool {
        match self.types.kind(ty) {
            _ if ty == TyId::ERROR => true,
            TyKind::Slice(e) | TyKind::Optional(e) | TyKind::Fallible(e) => self.mentions_error(*e),
            TyKind::Map(k, v) => self.mentions_error(*k) || self.mentions_error(*v),
            TyKind::Tuple(es) => es.iter().any(|e| self.mentions_error(*e)),
            TyKind::Fn { params, ret } => {
                params.iter().any(|p| self.mentions_error(*p)) || self.mentions_error(*ret)
            }
            _ => false,
        }
    }

    fn arity_error(
        &mut self,
        name: &str,
        given: usize,
        want: usize,
        span: Span,
        decl: Option<Span>,
    ) {
        let mut d = Diagnostic::error(
            codes::E0113,
            format!(
                "`{}` takes {} argument{}, but {} {} given",
                name,
                want,
                if want == 1 { "" } else { "s" },
                given,
                if given == 1 { "was" } else { "were" }
            ),
        )
        .with_primary(span, format!("{} given here", given));
        if let Some(d2) = decl {
            d = d.with_secondary(d2, "declared here");
        }
        d = d.with_note(
            "Kite has no default arguments, variadics, or overloading; a function needing many \
             optional inputs takes a struct",
        );
        self.diags.push(d);
    }

    /// The span of the `let` keyword preceding a binding, so E0114 can offer a
    /// `var` replacement.
    fn let_keyword_span(&self, name_span: Span) -> Option<Span> {
        let before = self.sources.text_before(name_span);
        let trimmed = before.trim_end();
        if trimmed.ends_with("let") {
            let end = trimmed.len() as u32;
            Some(Span::new(name_span.file, end - 3, end))
        } else {
            None
        }
    }

    /// A builtin named where a value is expected.
    ///
    /// Permanent rather than pending: a builtin has no body to take a
    /// reference to, and several of them choose what to do from the type of
    /// the argument they are given — so there is no one function to hand over.
    fn builtin_not_a_value(&mut self, span: Span) {
        self.diags.push(
            Diagnostic::error(codes::E0200, "a builtin is not a value")
                .with_primary(span, "this names a builtin rather than a function")
                .with_note(
                    "a builtin has no body to point at, and several choose what to do from \
                     the type of their argument",
                )
                .with_note("wrap it in a closure, which names the types it works on"),
        );
    }
}

/// Whether a checked pattern matches `nil`.
///
/// Read off the checked pattern rather than the surface one, because only the
/// checker knows what a bare name is: `A` against an `Option<E>` names the
/// variant, which is never nil, while the surface form is indistinguishable
/// from a binding that catches everything. Deciding from the surface let a
/// later arm bind the unwrapped `E` from a scrutinee that was still `nil`.
fn covers_nil(p: &hir::Pattern) -> bool {
    match p {
        hir::Pattern::Nil | hir::Pattern::Wildcard | hir::Pattern::Binding { .. } => true,
        hir::Pattern::Or(alts) => alts.iter().any(covers_nil),
        _ => false,
    }
}

/// The locals a checked pattern binds, in the order they appear.
///
/// An alternation is read through its first alternative: the checker has
/// already required every alternative to bind the same names.
fn pattern_bindings(p: &hir::Pattern, out: &mut Vec<u32>) {
    match p {
        hir::Pattern::Binding { local, .. } => out.push(local.0),
        hir::Pattern::Variant { fields, .. } | hir::Pattern::Tuple { elems: fields, .. } => {
            for f in fields {
                pattern_bindings(f, out);
            }
        }
        hir::Pattern::Struct { fields, .. } => {
            for (_, f) in fields {
                pattern_bindings(f, out);
            }
        }
        hir::Pattern::Or(alts) => {
            if let Some(first) = alts.first() {
                pattern_bindings(first, out);
            }
        }
        hir::Pattern::Wildcard
        | hir::Pattern::Int(_)
        | hir::Pattern::Float(_)
        | hir::Pattern::Str(_)
        | hir::Pattern::Bool(_)
        | hir::Pattern::IntRange { .. }
        | hir::Pattern::Nil => {}
    }
}

fn short_circuit(op: ast::BinaryOp) -> Option<hir::BinOp> {
    match op {
        ast::BinaryOp::And => Some(hir::BinOp::And),
        ast::BinaryOp::Or => Some(hir::BinOp::Or),
        _ => None,
    }
}

/// Verify every `impl Trait for Type` block: the trait is implemented once,
/// every required method is present, and each signature matches.
///
/// Returns which block implements each trait for each type — `(type index,
/// trait index)` to item index — because a block's own bounds are part of
/// whether a generic type implements the trait at all.
fn check_impls(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    type_ids: &[Option<TypeTarget>],
    types: &mut Types,
    sigs: &[Signature],
    trait_method_generics: &std::collections::HashMap<(hir::TraitId, usize), Vec<GenericDef>>,
    diags: &mut DiagBag,
) -> std::collections::HashMap<(u32, u32), usize> {
    // (trait index, type index) -> the span that first claimed it. Exactly one
    // implementation per pair is what makes trait resolution decidable.
    let mut claimed: std::collections::HashMap<(u32, u32), Span> =
        std::collections::HashMap::new();
    let mut blocks: std::collections::HashMap<(u32, u32), usize> =
        std::collections::HashMap::new();

    for (item_index, item) in file.items.iter().enumerate() {
        let ast::Item::Impl(imp) = item else { continue };
        let Some(tp) = &imp.trait_path else { continue };

        // Both names mean what they mean where the `impl` is written — which
        // is how the resolver registered its methods. Looking them up from
        // the root instead found nothing for an `impl` inside any module, and
        // an implementation nothing checked was one a `dyn` call could reach
        // with the wrong signature.
        let module = resolved.module_of_item(item_index);
        let (Some(ti), Some(target)) = (
            resolved.type_by_name_in(module, &tp.text()),
            resolved.type_by_name_in(module, &imp.self_ty.text()),
        ) else {
            continue;
        };
        let Some(TypeTarget::Trait(tid)) = type_ids[ti as usize] else {
            continue;
        };

        if let Some(&prev) = claimed.get(&(ti, target)) {
            diags.push(
                Diagnostic::error(
                    codes::E0112,
                    format!(
                        "`{}` is implemented for `{}` more than once",
                        tp.name(),
                        imp.self_ty.name()
                    ),
                )
                .with_primary(imp.span, "duplicate implementation")
                .with_secondary(prev, "first implemented here")
                .with_note(
                    "exactly one implementation per trait and type is what makes trait \
                     resolution decidable and separate compilation possible",
                ),
            );
            continue;
        }
        claimed.insert((ti, target), imp.span);
        blocks.insert((target, ti), item_index);

        let def = types.trait_def(tid);

        // Every method without a default must be provided.
        let mut missing = Vec::new();
        for m in &def.methods {
            if m.has_default {
                continue;
            }
            if !imp.methods.iter().any(|x| x.name.name == m.name) {
                missing.push(m.name.clone());
            }
        }
        if !missing.is_empty() {
            diags.push(
                Diagnostic::error(
                    codes::E0200,
                    format!(
                        "`{}` does not implement {} of `{}`",
                        imp.self_ty.name(),
                        missing
                            .iter()
                            .map(|m| format!("`{}`", m))
                            .collect::<Vec<_>>()
                            .join(", "),
                        tp.name()
                    ),
                )
                .with_primary(imp.span, "incomplete implementation")
                .with_secondary(def.span, "trait declared here"),
            );
        }

        // Every provided method must belong to the trait, and match its shape.
        for m in &imp.methods {
            let def = types.trait_def(tid);
            let Some((di, decl)) = def.method(&m.name.name) else {
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!("`{}` is not a method of `{}`", m.name.name, tp.name()),
                    )
                    .with_primary(m.name.span, "not declared by the trait")
                    .with_note(
                        "a trait implementation may only define the trait's methods; put \
                         anything else in an inherent `impl` block",
                    ),
                );
                continue;
            };
            let (decl_params, decl_ret, decl_fallible, decl_span, decl_generics) =
                (decl.params.clone(), decl.ret, decl.fallible, decl.span, decl.generic_count);

            if decl.takes_self != m.self_param.is_some() {
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!("`{}` has the wrong receiver", m.name.name),
                    )
                    .with_primary(m.sig_span, if decl.takes_self {
                        "the trait declares this with `self`"
                    } else {
                        "the trait declares this without `self`"
                    })
                    .with_secondary(decl_span, "declared here"),
                );
            } else if decl.takes_self
                && decl.var_self != m.self_param.as_ref().is_some_and(|s| s.is_var)
            {
                // A caller reaching the method through the trait — a `dyn`, or
                // a bound — sees the trait's receiver and nothing else. An
                // implementation that quietly took `var self` would modify a
                // value its caller holds in a `let`, which is the change the
                // rule exists to keep visible at the call site; one that took
                // plain `self` where the trait promised `var self` would be
                // refusing a permission every caller already paid for.
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!("`{}` has the wrong receiver", m.name.name),
                    )
                    .with_primary(
                        m.sig_span,
                        if decl.var_self {
                            "the trait declares this with `var self`"
                        } else {
                            "the trait declares this with `self`, which may not modify it"
                        },
                    )
                    .with_secondary(decl_span, "declared here")
                    .with_note(
                        "whether a method may modify its receiver is part of the signature \
                         every caller through the trait relies on, so the two must agree",
                    ),
                );
            }

            if decl_params.len() != m.params.len() {
                diags.push(
                    Diagnostic::error(
                        codes::E0113,
                        format!(
                            "`{}` takes {} parameter{}, but the trait declares {}",
                            m.name.name,
                            m.params.len(),
                            if m.params.len() == 1 { "" } else { "s" },
                            decl_params.len()
                        ),
                    )
                    .with_primary(m.sig_span, "signature does not match")
                    .with_secondary(decl_span, "declared here"),
                );
                continue;
            }
            if decl_generics != m.generics.len() {
                diags.push(
                    Diagnostic::error(
                        codes::E0208,
                        format!(
                            "`{}` has {} type parameter{}, but the trait declares {}",
                            m.name.name,
                            m.generics.len(),
                            if m.generics.len() == 1 { "" } else { "s" },
                            decl_generics
                        ),
                    )
                    .with_primary(m.sig_span, "signature does not match")
                    .with_secondary(decl_span, "declared here"),
                );
                continue;
            }

            // Matching arity is not matching types. A call through `dyn Trait`
            // is typed from the declaration and dispatched to the body, so an
            // implementation free to disagree about what it accepts is a
            // reinterpretation of whatever the caller passed: the native
            // backend will read an `int` as a pointer.
            let Some(fi) = resolved.trait_method(target, ti, &m.name.name) else {
                continue;
            };
            let sig = &sigs[fi as usize];

            // The declaration speaks of `Self` and of its own type parameters;
            // the implementation, of its type and of its parameters. Those are
            // the same things by position, so the declaration is translated
            // before the two are compared.
            let block = sig.generics.len().saturating_sub(m.generics.len());
            let own: Vec<TyId> = sig.generics[..block].iter().map(|g| g.ty).collect();
            let implementing = match type_ids[target as usize] {
                Some(TypeTarget::Struct(s)) if !own.is_empty() => {
                    let id = types.instantiate_struct(s, &own);
                    types.struct_ty(id)
                }
                Some(TypeTarget::Enum(e)) if !own.is_empty() => {
                    let id = types.instantiate_enum(e, &own);
                    types.enum_ty(id)
                }
                other => named_ty(other, types),
            };
            let mut map = vec![(types.self_param(), implementing)];
            if let Some(declared) = trait_method_generics.get(&(tid, di)) {
                for (d, g) in declared.iter().zip(&sig.generics[block..]) {
                    map.push((d.ty, g.ty));
                }
            }
            let decl_params: Vec<TyId> =
                decl_params.iter().map(|p| replace_types(types, *p, &map)).collect();
            let decl_ret = replace_types(types, decl_ret, &map);

            for (i, (&want, &got)) in decl_params.iter().zip(sig.params.iter()).enumerate() {
                if want == got || types.is_poisoned(want) || types.is_poisoned(got) {
                    continue;
                }
                let span = m.params[i].ty.span();
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!(
                            "`{}` takes {} here, but the trait declares {}",
                            m.name.name,
                            types.with_article(got),
                            types.with_article(want)
                        ),
                    )
                    .with_primary(span, format!("this is {}", types.with_article(got)))
                    .with_secondary(decl_span, "declared here")
                    .with_note(
                        "a call through `dyn Trait` is checked against the trait, so an \
                         implementation that takes something else would receive a value of \
                         the wrong type",
                    ),
                );
            }

            let got_ret = if sig.fallible {
                types.fallible_value(sig.ret).unwrap_or(sig.ret)
            } else {
                sig.ret
            };
            let ret_span = m.ret.as_ref().map_or(m.sig_span, |r| r.span());
            if sig.fallible != decl_fallible {
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!(
                            "`{}` {} an error, but the trait declares it {}",
                            m.name.name,
                            if sig.fallible { "returns" } else { "does not return" },
                            if decl_fallible { "fallible" } else { "infallible" },
                        ),
                    )
                    .with_primary(ret_span, "fallibility does not match")
                    .with_secondary(decl_span, "declared here")
                    .with_note(
                        "whether a call can fail is part of the signature every caller sees, \
                         so the trait and its implementations must agree",
                    ),
                );
            } else if got_ret != decl_ret
                && !types.is_poisoned(got_ret)
                && !types.is_poisoned(decl_ret)
            {
                diags.push(
                    Diagnostic::error(
                        codes::E0200,
                        format!(
                            "`{}` returns {}, but the trait declares {}",
                            m.name.name,
                            types.with_article(got_ret),
                            types.with_article(decl_ret)
                        ),
                    )
                    .with_primary(ret_span, format!("this is {}", types.with_article(got_ret)))
                    .with_secondary(decl_span, "declared here"),
                );
            }
        }
    }
    blocks
}

/// A trait is usable as an object only when every method can be called
/// through one (§10.3): it takes `self`, so there is a value to dispatch on;
/// it is not generic, so there is one body to call rather than one per type
/// argument; and it does not mention `Self`, which is a different type behind
/// every `dyn`. The rule belongs at every place a `dyn` is written, which is
/// why it lives beside type resolution rather than in one caller.
fn check_object_safe(tr: kite_hir::TraitId, span: Span, types: &Types, diags: &mut DiagBag) {
    let def = types.trait_def(tr);
    let offenders: Vec<String> = def
        .methods
        .iter()
        .filter_map(|m| types.not_dispatchable(m).map(|why| format!("`{}` {}", m.name, why)))
        .collect();
    if offenders.is_empty() {
        return;
    }
    let mut d = Diagnostic::error(
        codes::E0206,
        format!("`{}` cannot be a trait object", def.name),
    )
    .with_primary(span, "not every method of this trait can be called through a `dyn`");
    for o in offenders {
        d = d.with_note(o);
    }
    diags.push(d.with_note(
        "take a type parameter bounded by the trait instead — `fn f<T: Trait>(x: T)` — \
         or move what a `dyn` needs into a trait of its own",
    ));
}

fn named_ty(target: Option<TypeTarget>, types: &mut Types) -> TyId {
    match target {
        Some(TypeTarget::Struct(s)) => types.struct_ty(s),
        Some(TypeTarget::Enum(e)) => types.enum_ty(e),
        Some(TypeTarget::Trait(t)) => types.dyn_ty(t),
        Some(TypeTarget::Alias(ty)) => ty,
        None => TyId::ERROR,
    }
}

/// Expand every `type` alias to the type it names.
///
/// One alias may name another, so this walks the alias graph depth-first. A
/// cycle has no underlying type to expand to and would otherwise recurse
/// forever, so it is reported and the alias becomes the error type. An alias
/// still being visited when it is reached again *is* the cycle, which is what
/// `Visit::Visiting` records.
///
/// Generic aliases parse but are not expanded: substituting arguments through
/// an alias is a second instantiation path, and the language has one. Saying
/// so beats leaving a program that compiles to the wrong thing.
fn expand_aliases(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    type_ids: &mut [Option<TypeTarget>],
    types: &mut Types,
    diags: &mut DiagBag,
) {
    #[derive(Clone, Copy, PartialEq)]
    enum Visit {
        No,
        Visiting,
        Done,
    }

    let alias_at = |i: usize| match &file.items[resolved.types[i].decl_index] {
        ast::Item::TypeAlias(a) => Some(a),
        _ => None,
    };

    let mut state = vec![Visit::No; resolved.types.len()];
    // An explicit stack rather than recursion: the graph is user-written, and
    // a long chain of aliases should not be able to overflow the compiler.
    for start in 0..resolved.types.len() {
        if alias_at(start).is_none() || state[start] != Visit::No {
            continue;
        }
        let mut stack = vec![start];
        while let Some(&i) = stack.last() {
            let Some(alias) = alias_at(i) else {
                stack.pop();
                continue;
            };
            if state[i] == Visit::Done {
                stack.pop();
                continue;
            }
            let module = resolved.module_of_item(resolved.types[i].decl_index);
            if state[i] == Visit::No {
                state[i] = Visit::Visiting;
                if !alias.generics.is_empty() {
                    diags.push(
                        Diagnostic::error(codes::E0214, "a type alias cannot be generic")
                            .with_primary(alias.span, "type parameters are not accepted here")
                            .with_note(
                                "an alias is another spelling for one type; write the generic \
                                 type itself, or a struct that wraps it",
                            ),
                    );
                    type_ids[i] = Some(TypeTarget::Alias(TyId::ERROR));
                    state[i] = Visit::Done;
                    stack.pop();
                    continue;
                }
                // Expand the aliases this one names first, so `type A = B`
                // sees B's expansion rather than B's absence.
                let mut pending = false;
                for j in aliases_named_by(&alias.ty, resolved, module) {
                    match state[j] {
                        Visit::No => {
                            stack.push(j);
                            pending = true;
                        }
                        Visit::Visiting => {
                            diags.push(
                                Diagnostic::error(codes::E0214, "a type alias cannot be circular")
                                    .with_primary(alias.span, "this alias is defined as itself")
                                    .with_note(
                                        "an alias is replaced by what it names, so a cycle names \
                                         nothing",
                                    ),
                            );
                            type_ids[j] = Some(TypeTarget::Alias(TyId::ERROR));
                            state[j] = Visit::Done;
                        }
                        Visit::Done => {}
                    }
                }
                if pending {
                    continue;
                }
            }
            // Every alias this one names is expanded, so this one can be.
            let ty = resolve_named_ty(&alias.ty, resolved, module, type_ids, &[], types, diags);
            type_ids[i] = Some(TypeTarget::Alias(ty));
            state[i] = Visit::Done;
            stack.pop();
        }
    }
}

/// The declared-type indices of every alias named anywhere inside a written type.
fn aliases_named_by(t: &ast::Type, resolved: &ResolveMap, module: &str) -> Vec<usize> {
    let mut out = Vec::new();
    walk_named_types(t, &mut |p: &ast::TypePath| {
        if let Some(i) = resolved.type_by_name_in(module, &p.text()) {
            out.push(i as usize);
        }
    });
    out
}

/// Every path written inside a type, including those nested in slices, maps,
/// optionals, tuples, functions and type arguments.
fn walk_named_types(t: &ast::Type, f: &mut impl FnMut(&ast::TypePath)) {
    match t {
        ast::Type::Path(p) => {
            f(p);
            for a in &p.args {
                walk_named_types(a, f);
            }
        }
        ast::Type::Slice { elem, .. } => walk_named_types(elem, f),
        ast::Type::Map { key, value, .. } => {
            walk_named_types(key, f);
            walk_named_types(value, f);
        }
        ast::Type::Optional { inner, .. } => walk_named_types(inner, f),
        ast::Type::Tuple { elems, .. } => elems.iter().for_each(|e| walk_named_types(e, f)),
        ast::Type::Fn { params, ret, .. } => {
            params.iter().for_each(|p| walk_named_types(p, f));
            if let Some(r) = ret {
                walk_named_types(r, f);
            }
        }
        // `dyn Trait` names a trait, never an alias, but it is walked so that
        // adding a variant to `ast::Type` cannot silently skip this pass.
        ast::Type::Dyn { path, .. } => f(path),
        ast::Type::Error(_) => {}
    }
}

/// Resolve a surface type, consulting the module's declared types before
/// falling back to the primitives.
/// Check each constant's written type against the value it was given.
///
/// Separate from evaluation because a type annotation names a type, and types
/// are not known until the arena is built — while a value is known from the
/// source text alone. Splitting them keeps the evaluator free of the type
/// system entirely.
fn check_const_annotations(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    values: &ConstTable,
    type_ids: &[Option<TypeTarget>],
    types: &mut Types,
    diags: &mut DiagBag,
) {
    for (i, sig) in resolved.consts.iter().enumerate() {
        let ast::Item::Const(c) = &file.items[sig.decl_index] else { continue };
        let Some(written) = &c.ty else { continue };
        let Some(value) = values.get(i as u32) else { continue };

        let module = resolved.module_of_item(sig.decl_index);
        let want = resolve_named_ty(written, resolved, module, type_ids, &[], types, diags);
        let got = match value {
            ConstValue::Bool(_) => TyId::BOOL,
            ConstValue::Int(_) => TyId::INT,
            ConstValue::Float(_) => TyId::FLOAT,
            ConstValue::Str(_) => TyId::STR,
        };
        if want == got || types.is_poisoned(want) {
            continue;
        }
        let mut d = Diagnostic::error(
            codes::E0200,
            format!(
                "expected {}, found {}",
                types.with_article(want),
                types.with_article(got)
            ),
        )
        .with_primary(c.value.span(), format!("this is {}", types.with_article(got)))
        .with_secondary(written.span(), "the type written here");
        // The one mismatch worth a suggestion, because it is the one a reader
        // from almost any other language expects to work.
        if want == TyId::FLOAT && got == TyId::INT {
            d = d.with_note(
                "Kite performs no implicit numeric conversion: write the literal with a \
                 decimal point",
            );
        }
        diags.push(d);
    }
}

fn resolve_named_ty(
    t: &ast::Type,
    resolved: &ResolveMap,
    // The module the reference was written in. Its own declarations win, so
    // `Node` inside `std/ui.kite` is `ui.Node` without the file saying so.
    module: &str,
    type_ids: &[Option<TypeTarget>],
    generics: &[(String, TyId)],
    types: &mut Types,
    diags: &mut DiagBag,
) -> TyId {
    match t {
        ast::Type::Path(p) if !p.args.is_empty() => {
            let args: Vec<TyId> = p
                .args
                .iter()
                .map(|a| resolve_named_ty(a, resolved, module, type_ids, generics, types, diags))
                .collect();
            // `Task<T>` is the compiler's, like `Option<T>`: a program may
            // name one but cannot declare one, and there is nothing useful a
            // hand-written `Task` could be.
            if p.is_generic_name("Task") {
                if args.len() != 1 {
                    diags.push(
                        Diagnostic::error(codes::E0208, "`Task` takes one type argument")
                            .with_primary(p.span, "the type the task produces"),
                    );
                    return TyId::ERROR;
                }
                let id = types.task_of(args[0], p.span);
                return types.struct_ty(id);
            }
            let found = resolved.type_by_name_in(module, &p.text());
            if let Some(i) = found {
                check_type_visible(resolved, module, i, p.span, &p.text(), diags);
            }
            let target = found.and_then(|i| type_ids[i as usize]);
            match target {
                Some(TypeTarget::Struct(s)) => {
                    let want = types.struct_def(s).generic_count;
                    if !arity_ok(p, want, args.len(), types.struct_def(s).span, diags) {
                        return TyId::ERROR;
                    }
                    let id = types.instantiate_struct(s, &args);
                    types.struct_ty(id)
                }
                Some(TypeTarget::Enum(e)) => {
                    let want = types.enum_def(e).generic_count;
                    if !arity_ok(p, want, args.len(), types.enum_def(e).span, diags) {
                        return TyId::ERROR;
                    }
                    let id = types.instantiate_enum(e, &args);
                    types.enum_ty(id)
                }
                _ => {
                    diags.push(
                        Diagnostic::error(
                            codes::E0204,
                            format!("`{}` does not take type arguments", p.name()),
                        )
                        .with_primary(p.span, "type arguments are not accepted here"),
                    );
                    TyId::ERROR
                }
            }
        }

        // A path with no type arguments: a name, possibly module-qualified.
        ast::Type::Path(p) => {
            // A parameter shadows everything: inside `fn f<T>(...)`, `T` is the
            // parameter even if a type of that name is declared elsewhere.
            if p.is_simple() {
                if let Some((_, id)) = generics.iter().find(|(n, _)| n == p.name()) {
                    return *id;
                }
                if let Some(prim) = Types::primitive_from_name(p.name()) {
                    return prim;
                }
            }
            let found = resolved.type_by_name_in(module, &p.text());
            if let Some(i) = found {
                check_type_visible(resolved, module, i, p.span, &p.text(), diags);
            }
            match found {
                Some(i) => match type_ids[i as usize] {
                    Some(TypeTarget::Trait(_)) => {
                        diags.push(
                            Diagnostic::error(
                                codes::E0204,
                                format!("`{}` is a trait, not a type", p.name()),
                            )
                            .with_primary(p.span, "traits name behaviour, not values")
                            .with_note(format!(
                                "write `dyn {}` for a trait object, or use it as a bound",
                                p.name()
                            )),
                        );
                        TyId::ERROR
                    }
                    other => {
                        // A generic declaration named without arguments is not
                        // a type: `List` says nothing about what it holds.
                        if let Some(TypeTarget::Struct(s)) = other {
                            if types.struct_def(s).generic_count > 0 {
                                return missing_args(p, types.struct_def(s).generic_count, diags);
                            }
                        }
                        if let Some(TypeTarget::Enum(e)) = other {
                            if types.enum_def(e).generic_count > 0 {
                                return missing_args(p, types.enum_def(e).generic_count, diags);
                            }
                        }
                        named_ty(other, types)
                    }
                },
                None => resolve_ty(t, types, diags),
            }
        }
        ast::Type::Slice { elem, .. } => {
            let e = resolve_named_ty(elem, resolved, module, type_ids, generics, types, diags);
            types.slice_of(e)
        }
        ast::Type::Map { key, value, .. } => {
            let k = resolve_named_ty(key, resolved, module, type_ids, generics, types, diags);
            let v = resolve_named_ty(value, resolved, module, type_ids, generics, types, diags);
            check_map_key(types, k, key.span(), diags);
            types.map_of(k, v)
        }
        ast::Type::Optional { inner, .. } => {
            let i = resolve_named_ty(inner, resolved, module, type_ids, generics, types, diags);
            types.optional_of(i)
        }
        ast::Type::Tuple { elems, .. } => {
            let es: Vec<TyId> = elems
                .iter()
                .map(|e| resolve_named_ty(e, resolved, module, type_ids, generics, types, diags))
                .collect();
            if es.is_empty() {
                TyId::UNIT
            } else {
                types.tuple_of(es)
            }
        }
        ast::Type::Fn { params, ret, .. } => {
            let ps: Vec<TyId> = params
                .iter()
                .map(|p| resolve_named_ty(p, resolved, module, type_ids, generics, types, diags))
                .collect();
            let r = match ret {
                Some(r) => resolve_named_ty(r, resolved, module, type_ids, generics, types, diags),
                None => TyId::UNIT,
            };
            let r = fallible_form(r, types);
            types.fn_of(ps, r)
        }
        ast::Type::Dyn { path, span } => match resolved.type_by_name_in(module, &path.text()) {
            Some(i) => match type_ids[i as usize] {
                Some(TypeTarget::Trait(tr)) => {
                    check_type_visible(resolved, module, i, path.span, &path.text(), diags);
                    check_object_safe(tr, *span, types, diags);
                    types.dyn_ty(tr)
                }
                _ => {
                    diags.push(
                        Diagnostic::error(
                            codes::E0204,
                            format!("`{}` is not a trait", path.name()),
                        )
                        .with_primary(*span, "`dyn` needs a trait"),
                    );
                    TyId::ERROR
                }
            },
            None => {
                diags.push(
                    Diagnostic::error(codes::E0204, format!("unknown trait `{}`", path.name()))
                        .with_primary(*span, "no such trait"),
                );
                TyId::ERROR
            }
        },
        other => resolve_ty(other, types, diags),
    }
}

/// A map compares its keys with `==`, so a key type is one `==` is defined
/// on: not a function, a trait object or a host value, nor anything holding
/// one. A type parameter is taken on trust here and held to it by whoever
/// chooses it. Returns whether the key may be used.
fn check_map_key(types: &Types, key: TyId, span: Span, diags: &mut DiagBag) -> bool {
    if types.is_poisoned(key) || equatable_given_params(types, key, &mut Vec::new()) {
        return true;
    }
    let name = types.name(key);
    let mut d = Diagnostic::error(codes::E0201, format!("`{}` cannot be a map key", name))
        .with_primary(span, "a map compares its keys with `==`, which this does not have");
    d = if types.mentions_host_value(key) {
        d.with_note(
            "a host object has no structure Kite can see, so there is nothing to compare \
             field by field; keep a key the host object is known by — an id, a name — instead",
        )
    } else {
        d.with_note(
            "equality is structural, so every field must itself be equatable; functions and \
             trait objects are not",
        )
    };
    diags.push(d);
    false
}

/// Reject naming another module's private type in a type — a parameter, a
/// field, an annotation, a bound. The resolver holds expressions to §4.3;
/// types are resolved here, so they are held to it here.
fn check_type_visible(
    resolved: &ResolveMap,
    module: &str,
    index: u32,
    span: Span,
    written: &str,
    diags: &mut DiagBag,
) {
    let decl = &resolved.types[index as usize];
    let home = resolved.module_of_item(decl.decl_index);
    // The root module is the program's own, and the prelude is in scope
    // everywhere by construction — the same two exemptions the resolver makes.
    if decl.is_pub || home.is_empty() || home == kite_resolve::PRELUDE || home == module {
        return;
    }
    diags.push(
        Diagnostic::error(codes::E0401, format!("`{}` is private to module `{}`", written, home))
            .with_primary(span, "not visible here")
            .with_secondary(decl.span, format!("this {} is not marked `pub`", decl.kind.describe()))
            .with_note(
                "unmarked declarations are visible only within their own module; write `pub` \
                 to export one",
            ),
    );
}

/// Resolve a surface type to a [`TyId`], interning composite types as it goes.
fn resolve_ty(t: &ast::Type, types: &mut Types, diags: &mut DiagBag) -> TyId {
    match t {
        ast::Type::Path(p) if p.is_simple() => match Types::primitive_from_name(p.name()) {
            Some(ty) => ty,
            None => {
                let mut d =
                    Diagnostic::error(codes::E0204, format!("unknown type `{}`", p.name()))
                        .with_primary(p.span, "not a known type");
                if let Some(near) = nearest_type_name(p.name(), types) {
                    d = d.with_note(format!("a similar type is in scope: `{}`", near));
                } else {
                    d = d.with_note(format!(
                        "known types: {}",
                        types.known_type_names().join(", ")
                    ));
                }
                diags.push(d);
                TyId::ERROR
            }
        },

        ast::Type::Slice { elem, .. } => {
            let e = resolve_ty(elem, types, diags);
            types.slice_of(e)
        }
        ast::Type::Map { key, value, .. } => {
            let k = resolve_ty(key, types, diags);
            let v = resolve_ty(value, types, diags);
            types.map_of(k, v)
        }
        ast::Type::Optional { inner, .. } => {
            let i = resolve_ty(inner, types, diags);
            types.optional_of(i)
        }
        ast::Type::Tuple { elems, .. } => {
            let es: Vec<TyId> = elems.iter().map(|e| resolve_ty(e, types, diags)).collect();
            if es.is_empty() {
                TyId::UNIT
            } else {
                types.tuple_of(es)
            }
        }
        ast::Type::Fn { params, ret, .. } => {
            let ps: Vec<TyId> = params.iter().map(|p| resolve_ty(p, types, diags)).collect();
            let r = match ret {
                Some(r) => resolve_ty(r, types, diags),
                None => TyId::UNIT,
            };
            types.fn_of(ps, r)
        }

        // A name that did not resolve. This used to answer "generic types are
        // not available yet … generics arrive later in Phase 2", which was
        // true once and has not been for a long time — a program can be told
        // the language lacks a feature it is using two lines further up, and
        // sent to a roadmap to have that confirmed. The real fault is always
        // the name.
        ast::Type::Path(p) => {
            let mut d = Diagnostic::error(codes::E0204, format!("unknown type `{}`", p.text()))
                .with_primary(p.span, "no type of this name is in scope");
            if p.segments.len() > 1 {
                // `json.Json` with no `use std/json` is the common way to
                // arrive here, and the qualifier says which import is missing.
                d = d.with_note(format!(
                    "`{}` is qualified by `{}` — is that module imported?",
                    p.text(),
                    p.segments[0].name
                ));
            }
            diags.push(d);
            TyId::ERROR
        }
        ast::Type::Dyn { path, span } => {
            diags.push(
                Diagnostic::error(codes::E0204, format!("unknown trait `{}`", path.text()))
                    .with_primary(*span, "no trait of this name is in scope"),
            );
            TyId::ERROR
        }
        ast::Type::Error(_) => TyId::ERROR,
    }
}

/// Whether a `break` inside this body leaves *this* loop.
///
/// An unlabelled `break` in a nested loop belongs to that loop, and a labelled
/// one belongs to whichever loop carries the label — so only the two cases
/// that name this loop count.
fn breaks_out(block: &ast::Block, label: Option<&str>) -> bool {
    fn in_block(b: &ast::Block, label: Option<&str>, nested: bool) -> bool {
        b.stmts.iter().any(|s| in_stmt(s, label, nested))
    }
    fn in_if(i: &ast::IfStmt, label: Option<&str>, nested: bool) -> bool {
        in_block(&i.then, label, nested)
            || match i.else_.as_deref() {
                Some(ast::ElseBranch::Block(b)) => in_block(b, label, nested),
                Some(ast::ElseBranch::If(next)) => in_if(next, label, nested),
                None => false,
            }
    }
    fn in_stmt(s: &ast::Stmt, label: Option<&str>, nested: bool) -> bool {
        match s {
            ast::Stmt::Break { label: written, .. } => match written {
                None => !nested,
                Some(l) => Some(l.name.as_str()) == label,
            },
            ast::Stmt::If(i) => in_if(i, label, nested),
            // An inner loop captures an unlabelled `break`, so from here on
            // only a labelled one can leave this one.
            ast::Stmt::For(f) => in_block(&f.body, label, true),
            ast::Stmt::Match(m) => m.arms.iter().any(|a| match &a.body {
                ast::MatchBody::Block(b) => in_block(b, label, nested),
                ast::MatchBody::Expr(_) => false,
            }),
            _ => false,
        }
    }
    in_block(block, label, false)
}

/// `(T, error)` as a function's result: the fallible form, which is what the
/// same words mean on a declaration's `->`. Anywhere a result is written as a
/// type — a closure's `->`, a function type's — the tuple is read this way
/// too, so that a fallible closure, a fallible function used as a value and
/// the type naming either are all one type.
fn fallible_form(ty: TyId, types: &mut Types) -> TyId {
    match types.kind(ty) {
        TyKind::Tuple(parts) if parts.len() == 2 && parts[1] == TyId::ERR => {
            let value = parts[0];
            types.fallible_of(value)
        }
        _ => ty,
    }
}

/// Call `f` on every statement nested anywhere under `s`, `s` included — in
/// a branch, a loop, a `match` arm, or an arm of a `match` inside an
/// expression. A closure's body is only entered when `closures` is set: it is
/// a function of its own, and most questions asked this way are about one
/// function.
fn visit_stmts(s: &ast::Stmt, closures: bool, f: &mut dyn FnMut(&ast::Stmt)) {
    f(s);
    let expr = |e: &ast::Expr, f: &mut dyn FnMut(&ast::Stmt)| visit_expr_stmts(e, closures, f);
    match s {
        ast::Stmt::Let(l) => {
            if let Some(e) = &l.init {
                expr(e, f);
            }
        }
        ast::Stmt::Var(v) => expr(&v.init, f),
        ast::Stmt::Assign(a) => {
            expr(&a.target, f);
            expr(&a.value, f);
        }
        ast::Stmt::Return(r) => match &r.value {
            Some(ast::ReturnValue::Single(e)) | Some(ast::ReturnValue::Fail { error: e, .. }) => {
                expr(e, f)
            }
            Some(ast::ReturnValue::Pair { value, error, .. }) => {
                expr(value, f);
                expr(error, f);
            }
            None => {}
        },
        ast::Stmt::Check { expr: e, .. }
        | ast::Stmt::Defer { expr: e, .. }
        | ast::Stmt::Discard { value: e, .. }
        | ast::Stmt::Expr(e) => expr(e, f),
        ast::Stmt::If(i) => visit_if_stmts(i, closures, f),
        ast::Stmt::For(l) => {
            match &l.header {
                ast::ForHeader::In { iter, .. } => expr(iter, f),
                ast::ForHeader::While(c) => expr(c, f),
                ast::ForHeader::Loop => {}
            }
            for s in &l.body.stmts {
                visit_stmts(s, closures, f);
            }
        }
        ast::Stmt::Match(m) => visit_match_stmts(m, closures, f),
        ast::Stmt::Break { .. } | ast::Stmt::Continue { .. } | ast::Stmt::Error(_) => {}
    }
}

fn visit_if_stmts(i: &ast::IfStmt, closures: bool, f: &mut dyn FnMut(&ast::Stmt)) {
    visit_expr_stmts(&i.cond, closures, f);
    for s in &i.then.stmts {
        visit_stmts(s, closures, f);
    }
    match i.else_.as_deref() {
        Some(ast::ElseBranch::Block(b)) => {
            for s in &b.stmts {
                visit_stmts(s, closures, f);
            }
        }
        Some(ast::ElseBranch::If(next)) => visit_if_stmts(next, closures, f),
        None => {}
    }
}

fn visit_match_stmts(m: &ast::MatchExpr, closures: bool, f: &mut dyn FnMut(&ast::Stmt)) {
    visit_expr_stmts(&m.scrutinee, closures, f);
    for arm in &m.arms {
        if let Some(g) = &arm.guard {
            visit_expr_stmts(g, closures, f);
        }
        match &arm.body {
            ast::MatchBody::Expr(e) => visit_expr_stmts(e, closures, f),
            ast::MatchBody::Block(b) => {
                for s in &b.stmts {
                    visit_stmts(s, closures, f);
                }
            }
        }
    }
}

/// The statements inside an expression: those of a `match` arm written as a
/// block, of the branches of a value `if`, and of a closure when asked.
fn visit_expr_stmts(e: &ast::Expr, closures: bool, f: &mut dyn FnMut(&ast::Stmt)) {
    let sub = |e: &ast::Expr, f: &mut dyn FnMut(&ast::Stmt)| visit_expr_stmts(e, closures, f);
    match e {
        ast::Expr::Int(_)
        | ast::Expr::Float(_)
        | ast::Expr::Str(_)
        | ast::Expr::Char(_)
        | ast::Expr::Bool { .. }
        | ast::Expr::Nil(_)
        | ast::Expr::Path(_)
        | ast::Expr::SelfExpr(_)
        | ast::Expr::ImpliedInt { .. }
        | ast::Expr::Error(_) => {}
        ast::Expr::Interpolated { parts, .. } => {
            for part in parts {
                if let ast::StrPart::Hole(h) = part {
                    sub(h, f);
                }
            }
        }
        ast::Expr::Unary { operand: x, .. }
        | ast::Expr::Field { base: x, .. }
        | ast::Expr::Cast { expr: x, .. }
        | ast::Expr::Await { expr: x, .. }
        | ast::Expr::Paren { inner: x, .. } => sub(x, f),
        ast::Expr::Binary { lhs: a, rhs: b, .. }
        | ast::Expr::Index { base: a, index: b, .. }
        | ast::Expr::Range { start: a, end: b, .. } => {
            sub(a, f);
            sub(b, f);
        }
        ast::Expr::Call { callee, args, .. } => {
            sub(callee, f);
            for a in args {
                sub(a, f);
            }
        }
        ast::Expr::If { cond, then, else_, .. } => {
            sub(cond, f);
            for s in &then.stmts {
                visit_stmts(s, closures, f);
            }
            match else_.as_ref() {
                ast::ElseBranch::Block(b) => {
                    for s in &b.stmts {
                        visit_stmts(s, closures, f);
                    }
                }
                ast::ElseBranch::If(i) => visit_if_stmts(i, closures, f),
            }
        }
        ast::Expr::Tuple { elems, .. } | ast::Expr::Slice { elems, .. } => {
            for x in elems {
                sub(x, f);
            }
        }
        ast::Expr::Map { entries, .. } => {
            for entry in entries {
                sub(&entry.key, f);
                sub(&entry.value, f);
            }
        }
        ast::Expr::StructLit(lit) => {
            if let Some(b) = &lit.base {
                sub(b, f);
            }
            for field in &lit.fields {
                sub(&field.value, f);
            }
        }
        ast::Expr::Match(m) => visit_match_stmts(m, closures, f),
        ast::Expr::Closure { body, .. } => {
            if closures {
                match body.as_ref() {
                    ast::ClosureBody::Expr(x) => sub(x, f),
                    ast::ClosureBody::Block(b) => {
                        for s in &b.stmts {
                            visit_stmts(s, closures, f);
                        }
                    }
                }
            }
        }
    }
}

/// Whether a statement registers a `defer` for the function it is in.
fn stmt_defers(s: &ast::Stmt) -> bool {
    let mut found = false;
    visit_stmts(s, false, &mut |s| found |= matches!(s, ast::Stmt::Defer { .. }));
    found
}

/// The same for a closure whose body is an expression.
fn expr_defers(e: &ast::Expr) -> bool {
    let mut found = false;
    visit_expr_stmts(e, false, &mut |s| found |= matches!(s, ast::Stmt::Defer { .. }));
    found
}

/// A block string's body, with the indentation the closing delimiter sets
/// removed from every line.
///
/// The closing delimiter decides, rather than the shallowest line, because a
/// block string is written *at* an indentation level and its author is looking
/// at the closing quotes when they choose one. The line break after the
/// opening delimiter and the one before the closing delimiter belong to the
/// syntax rather than to the text, so both go.
pub(crate) fn dedent_block(body: &str) -> String {
    let body = body.strip_prefix("\r\n").or_else(|| body.strip_prefix('\n')).unwrap_or(body);
    // Everything after the last line break is the closing delimiter's own
    // indentation.
    let (text, indent) = match body.rfind('\n') {
        Some(at) => (&body[..at], &body[at + 1..]),
        None => (body, ""),
    };
    if !indent.chars().all(|c| c == ' ' || c == '\t') {
        // The closing delimiter is not on a line of its own, so there is no
        // indentation to take off.
        return body.to_string();
    }
    text.split('\n')
        .map(|line| line.strip_prefix(indent).unwrap_or(line.trim_start_matches([' ', '\t'])))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A type with some of the types inside it replaced — `Self` by the type
/// implementing a trait, or one declaration's parameters by another's.
fn replace_types(types: &mut Types, ty: TyId, map: &[(TyId, TyId)]) -> TyId {
    if let Some((_, to)) = map.iter().find(|(from, _)| *from == ty) {
        return *to;
    }
    match types.kind(ty).clone() {
        TyKind::Slice(e) => {
            let e = replace_types(types, e, map);
            types.slice_of(e)
        }
        TyKind::Optional(e) => {
            let e = replace_types(types, e, map);
            types.optional_of(e)
        }
        TyKind::Fallible(e) => {
            let e = replace_types(types, e, map);
            types.fallible_of(e)
        }
        TyKind::Map(k, v) => {
            let (k, v) = (replace_types(types, k, map), replace_types(types, v, map));
            types.map_of(k, v)
        }
        TyKind::Tuple(es) => {
            let es = es.iter().map(|e| replace_types(types, *e, map)).collect();
            types.tuple_of(es)
        }
        TyKind::Fn { params, ret } => {
            let ps = params.iter().map(|p| replace_types(types, *p, map)).collect();
            let r = replace_types(types, ret, map);
            types.fn_of(ps, r)
        }
        TyKind::Struct(s) => match types.struct_origin_of(s) {
            Some((template, args)) => {
                let args: Vec<TyId> = args.iter().map(|a| replace_types(types, *a, map)).collect();
                let id = types.instantiate_struct(template, &args);
                types.struct_ty(id)
            }
            None => ty,
        },
        TyKind::Enum(e) => match types.enum_origin_of(e) {
            Some((template, args)) => {
                let args: Vec<TyId> = args.iter().map(|a| replace_types(types, *a, map)).collect();
                let id = types.instantiate_enum(template, &args);
                types.enum_ty(id)
            }
            None => ty,
        },
        _ => ty,
    }
}

/// What generic bodies ask of their type arguments that no bound says.
///
/// `==` on a type parameter is the one such demand. Equality is structural and
/// defined for every type except functions, trait objects and host values, so
/// `fn eq<T>(a: T, b: T) -> bool { return a == b }` is sound for almost every
/// `T` and meaningless for those three — where the backends used to disagree
/// about what it answered. A bound would make every caller say so; recording
/// the comparison and checking each call's arguments against it asks nothing
/// of the programs that were already right.
#[derive(Default)]
struct GenericFacts {
    /// Per function, per type parameter: where its body compares one with `==`.
    compared: Vec<Vec<Option<Span>>>,
    /// Every call that chose a generic callee's type arguments.
    calls: Vec<GenericCall>,
}

struct GenericCall {
    caller: usize,
    callee: usize,
    targs: Vec<TyId>,
    span: Span,
}

/// Every type parameter a type mentions, by index.
fn params_in(types: &Types, ty: TyId, out: &mut Vec<u32>) {
    match types.kind(ty) {
        TyKind::Param { index, .. } => {
            if !out.contains(index) {
                out.push(*index);
            }
        }
        TyKind::Struct(s) => {
            if let Some((_, args)) = types.struct_origin_of(*s) {
                for a in args {
                    params_in(types, a, out);
                }
            }
        }
        TyKind::Enum(e) => {
            if let Some((_, args)) = types.enum_origin_of(*e) {
                for a in args {
                    params_in(types, a, out);
                }
            }
        }
        TyKind::Slice(e) | TyKind::Optional(e) | TyKind::Fallible(e) => params_in(types, *e, out),
        TyKind::Map(k, v) => {
            params_in(types, *k, out);
            params_in(types, *v, out);
        }
        TyKind::Tuple(es) => {
            for e in es {
                params_in(types, *e, out);
            }
        }
        TyKind::Fn { params, ret } => {
            for p in params {
                params_in(types, *p, out);
            }
            params_in(types, *ret, out);
        }
        _ => {}
    }
}

/// Whether `==` is defined on a type once its parameters are chosen — taking
/// each parameter to be something comparable, which the parameter's own
/// callers are then held to.
fn equatable_given_params(types: &Types, ty: TyId, seen: &mut Vec<TyId>) -> bool {
    let mut params = Vec::new();
    params_in(types, ty, &mut params);
    if params.is_empty() {
        return types.is_equatable(ty);
    }
    if seen.contains(&ty) {
        return true;
    }
    seen.push(ty);
    let ok = match types.kind(ty).clone() {
        TyKind::Param { .. } => true,
        TyKind::Slice(e) | TyKind::Optional(e) => equatable_given_params(types, e, seen),
        TyKind::Tuple(es) => es.iter().all(|e| equatable_given_params(types, *e, seen)),
        TyKind::Struct(s) => {
            let fields: Vec<TyId> = types.struct_def(s).fields.iter().map(|f| f.ty).collect();
            fields.into_iter().all(|t| equatable_given_params(types, t, seen))
        }
        TyKind::Enum(e) => {
            let fields: Vec<TyId> = types
                .enum_def(e)
                .variants
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| f.ty))
                .collect();
            fields.into_iter().all(|t| equatable_given_params(types, t, seen))
        }
        // Whatever the arguments, a function or a map is compared by the
        // rule for its kind, which a parameter inside it does not change.
        _ => false,
    };
    seen.pop();
    ok
}

/// Hold every call to a generic function to the comparisons its body makes.
///
/// Run once every body has been checked, because a call may be checked before
/// the body it reaches. A caller forwarding its own parameter to one that is
/// compared inherits the comparison, so the facts are closed over the call
/// graph before anything is reported.
fn check_compared_params(
    facts: &mut GenericFacts,
    resolved: &ResolveMap,
    sigs: &[Signature],
    types: &Types,
    diags: &mut DiagBag,
) {
    loop {
        let mut changed = false;
        for call in &facts.calls {
            for (i, t) in call.targs.iter().enumerate() {
                if facts.compared[call.callee].get(i).copied().flatten().is_none() {
                    continue;
                }
                let mut params = Vec::new();
                params_in(types, *t, &mut params);
                for p in params {
                    if let Some(slot) = facts.compared[call.caller].get_mut(p as usize) {
                        if slot.is_none() {
                            *slot = Some(call.span);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    for call in &facts.calls {
        for (i, t) in call.targs.iter().enumerate() {
            let Some(at) = facts.compared[call.callee].get(i).copied().flatten() else {
                continue;
            };
            if types.is_poisoned(*t) || equatable_given_params(types, *t, &mut Vec::new()) {
                continue;
            }
            let name = last_segment(&resolved.fns[call.callee].name).to_string();
            let param = sigs[call.callee].generics.get(i).map(|g| g.name.clone()).unwrap_or_default();
            let mut d = Diagnostic::error(
                codes::E0201,
                format!(
                    "`{}` compares its `{}` with `==`, and {} cannot be compared",
                    name,
                    param,
                    types.with_article(*t)
                ),
            )
            .with_primary(call.span, format!("`{}` is {} here", param, types.with_article(*t)))
            .with_secondary(at, "compared here");
            d = if types.mentions_host_value(*t) {
                d.with_note(
                    "a host object has no structure Kite can see, so there is nothing to \
                     compare field by field; `js.same(a, b)` asks the host whether two are \
                     the same object",
                )
            } else {
                d.with_note(
                    "equality is structural, so every field must itself be equatable; \
                     functions and trait objects are not",
                )
            };
            diags.push(d);
        }
    }
}

/// The last segment of a possibly-qualified name.
fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// A dotted name's text, for diagnostics about a qualified call.
fn expr_text(e: &ast::Expr) -> String {
    match e {
        ast::Expr::Path(p) => p.text(),
        ast::Expr::Field { base, name, .. } => format!("{}.{}", expr_text(base), name.name),
        _ => "…".to_string(),
    }
}

/// Nearest known type name by edit distance, when close enough to be a typo.
fn nearest_type_name(name: &str, types: &Types) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for cand in types.known_type_names() {
        let d = edit_distance(name, &cand);
        if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, cand));
        }
    }
    let (dist, cand) = best?;
    (dist <= (name.len() / 3).max(1)).then_some(cand)
}

/// Levenshtein distance, two-row variant.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Turn the source text between a literal's quotes into the string it means.
///
/// A free function because two callers need it and only one of them has a
/// [`Checker`]: a module-level constant is evaluated before any body is, so it
/// has no function to be inside.
pub(crate) fn decode_escapes_into(inner: &str, span: Span, diags: &mut DiagBag) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('u') => {
                let mut hex = String::new();
                if chars.next() == Some('{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        hex.push(c);
                    }
                }
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => diags.push(
                        Diagnostic::error(codes::E0003, "invalid unicode escape")
                            .with_primary(span, format!("`\\u{{{}}}` is not a character", hex)),
                    ),
                }
            }
            // The parser removes every `\(`, so one reaching here is a
            // literal backslash before a paren, which is what it looks like.
            Some('(') => {
                out.push('\\');
                out.push('(');
            }
            Some(other) => {
                diags.push(
                    Diagnostic::error(codes::E0003, "invalid escape sequence")
                        .with_primary(span, format!("`\\{}` is not recognised", other))
                        .with_note("valid escapes: \\n \\t \\r \\0 \\\\ \\\" \\' \\u{...}"),
                );
            }
            None => {}
        }
    }
    out
}

pub(crate) fn parse_int(text: &str) -> Option<i64> {
    let text: String = text.chars().filter(|c| *c != '_').collect();
    let text = strip_int_suffix(&text);
    if let Some(h) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok();
    }
    if let Some(o) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        return i64::from_str_radix(o, 8).ok();
    }
    if let Some(b) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        return i64::from_str_radix(b, 2).ok();
    }
    text.parse().ok()
}

/// Whether an integer literal's digits are `2^63`: out of range alone, and
/// `int`'s minimum when a `-` is written in front of them.
pub(crate) fn is_int_min_magnitude(text: &str) -> bool {
    let text: String = text.chars().filter(|c| *c != '_').collect();
    let text = strip_int_suffix(&text);
    let magnitude = if let Some(h) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(o) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        u64::from_str_radix(o, 8).ok()
    } else if let Some(b) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        u64::from_str_radix(b, 2).ok()
    } else {
        text.parse::<u64>().ok()
    };
    magnitude == Some(i64::MIN.unsigned_abs())
}

pub(crate) fn parse_float(text: &str) -> Option<f64> {
    let text: String = text.chars().filter(|c| *c != '_').collect();
    let text = text
        .strip_suffix("f64")
        .or_else(|| text.strip_suffix("f32"))
        .unwrap_or(&text);
    text.parse().ok()
}

fn strip_int_suffix(text: &str) -> &str {
    for s in ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"] {
        if let Some(t) = text.strip_suffix(s) {
            return t;
        }
    }
    text
}

#[cfg(test)]
mod tests;
