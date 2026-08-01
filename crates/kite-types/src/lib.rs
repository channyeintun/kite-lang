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
use kite_span::Span;

mod exhaustive;

pub fn check(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    src: &str,
    diags: &mut DiagBag,
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

    // Now fill in fields and variants, resolving their types against the
    // arena, which already knows every name.
    for (i, decl) in resolved.types.iter().enumerate() {
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
                                            &f.ty, resolved, &type_ids, &mut types, diags,
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
                                            ty, resolved, &type_ids, &mut types, diags,
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
                let methods = tr
                    .methods
                    .iter()
                    .map(|m| kite_hir::TraitMethodDef {
                        name: m.name.name.clone(),
                        params: m
                            .params
                            .iter()
                            .map(|p| {
                                resolve_named_ty(&p.ty, resolved, &type_ids, &mut types, diags)
                            })
                            .collect(),
                        ret: match &m.ret {
                            None => TyId::UNIT,
                            Some(r) => resolve_named_ty(
                                r.value_type(), resolved, &type_ids, &mut types, diags,
                            ),
                        },
                        takes_self: m.self_param.is_some(),
                        has_default: m.body.is_some(),
                        span: m.name.span,
                    })
                    .collect();
                types.set_trait_methods(tid, methods);
            }
            (Some(TypeTarget::Struct(sid)), ast::Item::Struct(s)) => {
                let fields = s
                    .fields
                    .iter()
                    .map(|f| kite_hir::FieldDef {
                        name: f.name.name.clone(),
                        ty: resolve_named_ty(&f.ty, resolved, &type_ids, &mut types, diags),
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

    // Signatures next, so calls can be checked in either direction.
    let mut sigs = Vec::new();
    for sig in &resolved.fns {
        let (params, ret, fallible, name_span, self_ty) = match sig.owner {
            None => {
                let ast::Item::Fn(f) = &file.items[sig.decl_index] else {
                    unreachable!("a free-function signature points at a function")
                };
                let params = f
                    .params
                    .iter()
                    .map(|p| resolve_named_ty(&p.ty, resolved, &type_ids, &mut types, diags))
                    .collect();
                let ret = match &f.ret {
                    None => TyId::UNIT,
                    Some(r) => {
                        resolve_named_ty(r.value_type(), resolved, &type_ids, &mut types, diags)
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
                    .map(|p| resolve_named_ty(&p.ty, resolved, &type_ids, &mut types, diags))
                    .collect();
                let ret = match &m.ret {
                    None => TyId::UNIT,
                    Some(r) => {
                        resolve_named_ty(r.value_type(), resolved, &type_ids, &mut types, diags)
                    }
                };
                let self_ty = if owner.takes_self {
                    Some(named_ty(type_ids[owner.type_index as usize], &mut types))
                } else {
                    None
                };
                let fallible = m.ret.as_ref().is_some_and(|r| r.is_fallible());
                let ret = if fallible { types.fallible_of(ret) } else { ret };
                (params, ret, fallible, m.name.span, self_ty)
            }
        };
        sigs.push(Signature { params, ret, fallible, name_span, self_ty });
    }

    check_impls(file, resolved, &type_ids, &types, diags);

    for (i, sig) in resolved.fns.iter().enumerate() {
        let mut checker = Checker {
            resolved,
            sigs: &sigs,
            type_ids: &type_ids,
            types: &mut types,
            src,
            diags,
            fn_index: i,
            locals: Vec::new(),
            init: Vec::new(),
            taint: Vec::new(),
            guards: std::collections::HashMap::new(),
            loop_depth: 0,
        };
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
    }

    hir::Program {
        types,
        fns,
        entry: resolved.fn_by_name("main").map(hir::FnId),
    }
}

#[derive(Clone, Copy, Debug)]
enum TypeTarget {
    Struct(kite_hir::StructId),
    Enum(kite_hir::EnumId),
    Trait(kite_hir::TraitId),
}

struct Signature {
    params: Vec<TyId>,
    ret: TyId,
    fallible: bool,
    name_span: Span,
    /// For a method, the type its `self` has.
    self_ty: Option<TyId>,
}

struct Checker<'a> {
    resolved: &'a ResolveMap,
    sigs: &'a [Signature],
    /// Arena handles for each entry in `resolved.types`, parallel by index.
    type_ids: &'a [Option<TypeTarget>],
    /// The interned type arena, built up as declarations are checked.
    types: &'a mut Types,
    src: &'a str,
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
    loop_depth: u32,
}

/// Whether a local certainly holds a value at this point.
///
/// The specification permits `let x: int` followed by assignment in branches,
/// "provided the compiler can prove exactly one assignment occurs on every path
/// before first use". This is that proof: a two-element lattice, merged at
/// every branch join, which is the same machinery as the error-taint analysis
/// arriving in Phase 3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Init {
    /// Declared without a value and not yet assigned on this path.
    Unassigned,
    Assigned,
}

impl Init {
    /// A local is assigned after a join only when it is assigned on *every*
    /// incoming path.
    fn merge(self, other: Init) -> Init {
        if self == Init::Assigned && other == Init::Assigned {
            Init::Assigned
        } else {
            Init::Unassigned
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
            Some(b) => self.block(b, sig),
            // A trait method with no default body. Nothing to check.
            None => (hir::Block::default(), Flow::Diverges),
        };

        self.report_unchecked_errors();

        if body.is_some() && sig.ret != TyId::UNIT && flow == Flow::Falls {
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
            name: name.to_string(),
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
                let e = self.match_expr(m, None);
                let flow = if e.ty == TyId::NEVER { Flow::Diverges } else { Flow::Falls };
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
                let expr = self.expr(e, None);
                let flow = if expr.ty == TyId::NEVER { Flow::Diverges } else { Flow::Falls };
                Some((hir::Stmt::Expr(expr), flow))
            }

            ast::Stmt::Check { expr, span } => self.check_stmt(expr, *span, sig),
            ast::Stmt::Defer { span, .. } => {
                self.not_yet(*span, "`defer`", "scope-exit release arrives in a later phase");
                None
            }
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
        let annotated = l.ty.as_ref().map(|t| resolve_ty(t, self.types, self.diags));

        let init = l.init.as_ref().map(|e| self.expr(e, annotated));

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
        // A `let` with an initialiser holds a value from here on; one without
        // stays unassigned until a branch writes it.
        if init.is_some() {
            self.init[local_id as usize] = Init::Assigned;
        }
        let _ = sig;
        Some((
            hir::Stmt::Let { local: hir::LocalId(local_id), init, span: l.span },
            Flow::Falls,
        ))
    }

    fn var_stmt(&mut self, v: &ast::VarStmt, _sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        let local_id = self.resolved.lookup_binding(v.name.span)?;
        let annotated = v.ty.as_ref().map(|t| resolve_ty(t, self.types, self.diags));
        let init = self.expr(&v.init, annotated);

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
            self.not_yet(
                a.target.span(),
                "assignment to this expression",
                "indexing arrives later in Phase 2",
            );
            return None;
        };
        let Some(Res::Local(local_id)) = self.resolved.lookup_use(p.span) else {
            return None;
        };

        let slot = local_id as usize;
        let local_ty = self.locals[slot].ty;
        let mutable = self.locals[slot].mutable;
        let decl_span = self.locals[slot].span;
        let name = self.locals[slot].name.clone();
        let already_assigned = self.init[slot] == Init::Assigned;

        // An immutable binding may be written exactly once, and only if it was
        // declared without an initialiser. That is what makes
        // `let z: int` followed by branch assignment legal.
        if !mutable && already_assigned {
            let mut d = Diagnostic::error(
                codes::E0114,
                format!("cannot assign to immutable binding `{}`", name),
            )
            .with_secondary(decl_span, "declared immutable here")
            .with_primary(p.span, "cannot assign");

            if let Some(kw) = self.let_keyword_span(decl_span) {
                d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
            }
            self.diags.push(d);
        } else if !mutable && self.loop_depth > 0 {
            // Inside a loop the assignment could run more than once, which
            // would be a second write to an immutable binding.
            self.diags.push(
                Diagnostic::error(
                    codes::E0114,
                    format!("cannot assign to immutable binding `{}` inside a loop", name),
                )
                .with_primary(p.span, "this assignment may run more than once")
                .with_secondary(decl_span, "declared immutable here")
                .with_note("declare it `var` if it is meant to change"),
            );
        }

        self.init[slot] = Init::Assigned;

        let value = self.expr(&a.value, Some(local_ty));

        let value = match a.op.to_binary() {
            None => {
                self.expect_ty(value.ty, local_ty, value.span, Some(decl_span));
                value
            }
            Some(binop) => {
                // `n += 1` is checked as `n = n + 1`, so the operand rules and
                // their messages are shared.
                let lhs = hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(local_id)),
                    ty: local_ty,
                    span: p.span,
                };
                self.binary(binop, lhs, value, a.span)
            }
        };

        Some((
            hir::Stmt::Assign { local: hir::LocalId(local_id), value, span: a.span },
            Flow::Falls,
        ))
    }

    fn return_stmt(&mut self, r: &ast::ReturnStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        match &r.value {
            None => {
                if sig.ret != TyId::UNIT {
                    self.diags.push(
                        Diagnostic::error(codes::E0203, "missing return value")
                            .with_primary(r.span, format!("expected a `{}`", self.types.name(sig.ret)))
                            .with_secondary(sig.name_span, "declared here"),
                    );
                }
                Some((hir::Stmt::Return { value: None, span: r.span }, Flow::Diverges))
            }
            Some(ast::ReturnValue::Single(e)) if sig.fallible => {
                let inner = self.types.fallible_value(sig.ret).unwrap_or(TyId::ERROR);
                let value = self.expr(e, Some(inner));
                self.expect_ty(value.ty, inner, value.span, Some(sig.name_span));
                self.diags.push(
                    Diagnostic::error(codes::E0203, "a fallible function returns two values")
                        .with_primary(r.span, "only one value returned")
                        .with_secondary(sig.name_span, "declared `(T, error)` here")
                        .with_note("write `return value, nil` on the success path"),
                );
                None
            }

            Some(ast::ReturnValue::Single(e)) => {
                let value = self.expr(e, Some(sig.ret));
                if sig.ret == TyId::UNIT {
                    self.diags.push(
                        Diagnostic::error(codes::E0200, "returning a value from a `()` function")
                            .with_primary(value.span, format!("this is {}", self.types.with_article(value.ty)))
                            .with_secondary(sig.name_span, "no return type declared here"),
                    );
                } else {
                    self.expect_ty(value.ty, sig.ret, value.span, Some(sig.name_span));
                }
                Some((
                    hir::Stmt::Return { value: Some(value), span: r.span },
                    Flow::Diverges,
                ))
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
                self.expect_ty(v.ty, inner, v.span, Some(sig.name_span));
                let e = self.expr(error, Some(TyId::ERR));
                self.expect_ty(e.ty, TyId::ERR, e.span, None);
                Some((
                    hir::Stmt::Return {
                        value: Some(hir::Expr {
                            kind: ExprKind::PairNew {
                                value: Box::new(v),
                                error: Box::new(e),
                            },
                            ty: sig.ret,
                            span: *span,
                        }),
                        span: r.span,
                    },
                    Flow::Diverges,
                ))
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
                self.expect_ty(e.ty, TyId::ERR, e.span, None);
                self.mark_checked(error);
                Some((
                    hir::Stmt::Return {
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
                    },
                    Flow::Diverges,
                ))
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
        let entry_init = self.init.clone();

        let entry_taint = self.taint.clone();
        // Inside `if err != nil { … }` the value is still not valid.
        let narrowed = self.apply_narrowing(narrowing, true);
        let mut then_entry = self.taint.clone();
        self.clean_guarded(&mut then_entry, tested, true);
        self.taint = then_entry;
        let (then, then_flow) = self.block(&i.then, sig);
        self.undo_narrowing(narrowed);
        let then_init = std::mem::replace(&mut self.init, entry_init.clone());
        let then_taint = std::mem::replace(&mut self.taint, entry_taint.clone());

        let (else_, else_flow) = match i.else_.as_deref() {
            None => (None, Flow::Falls),
            Some(ast::ElseBranch::Block(b)) => {
                let narrowed = self.apply_narrowing(narrowing, false);
                let mut else_entry = entry_taint.clone();
                self.clean_guarded(&mut else_entry, tested, false);
                self.taint = else_entry;
                let (blk, f) = self.block(b, sig);
                self.undo_narrowing(narrowed);
                (Some(blk), f)
            }
            Some(ast::ElseBranch::If(nested)) => {
                let (stmt, f) = self.if_stmt(nested, sig)?;
                (Some(hir::Block { stmts: vec![stmt] }), f)
            }
        };
        let else_init = std::mem::take(&mut self.init);
        let else_taint = std::mem::take(&mut self.taint);

        // A guarded value becomes clean after `if err != nil { return … }`,
        // because control only reaches here when the error was nil.
        self.taint = match (then_flow, else_flow, i.else_.is_some()) {
            // `if err != nil { return … }` — control continues only when the
            // error was nil, so the value it guards is now valid.
            (Flow::Diverges, _, false) => {
                let mut merged = entry_taint.clone();
                self.clean_guarded(&mut merged, tested, false);
                merged
            }
            (Flow::Diverges, _, true) => else_taint.clone(),
            (_, Flow::Diverges, true) => then_taint.clone(),
            (_, _, false) => entry_taint.clone(),
            _ => then_taint
                .iter()
                .zip(&else_taint)
                .map(|(a, b)| a.merge(*b))
                .collect(),
        };

        self.init = match (then_flow, else_flow, i.else_.is_some()) {
            // No `else`: control can arrive having skipped the `then` entirely.
            (_, _, false) => entry_init,
            (Flow::Diverges, Flow::Diverges, _) => else_init,
            (Flow::Diverges, Flow::Falls, _) => else_init,
            (Flow::Falls, Flow::Diverges, _) => then_init,
            (Flow::Falls, Flow::Falls, _) => then_init
                .iter()
                .zip(&else_init)
                .map(|(a, b)| a.merge(*b))
                .collect(),
        };

        // Without an `else`, control can always fall through.
        let flow = if i.else_.is_none() {
            Flow::Falls
        } else {
            then_flow.merge(else_flow)
        };

        Some((hir::Stmt::If { cond, then, else_, span: i.span }, flow))
    }

    fn for_stmt(&mut self, f: &ast::ForStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        let label = f.label.as_ref().map(|l| l.name.clone());
        self.loop_depth += 1;

        // A loop body may run zero times, so nothing it assigns can be assumed
        // assigned afterwards. The entry state is restored at the end.
        let entry_init = self.init.clone();

        let result = match &f.header {
            ast::ForHeader::In { binding, iter } => {
                let ast::Binding::Name(name) = binding else {
                    self.not_yet(binding.span(), "tuple loop bindings", "arrives in Phase 2");
                    self.loop_depth -= 1;
                    return None;
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
                        self.loop_depth -= 1;
                        self.init = entry_init;
                        return None;
                    };
                    let local_id = self.resolved.lookup_binding(name.span)?;
                    self.locals[local_id as usize].ty = elem;
                    self.init[local_id as usize] = Init::Assigned;

                    let (body, _) = self.block(&f.body, sig);
                    self.loop_depth -= 1;
                    self.init = entry_init;
                    return Some((
                        hir::Stmt::ForSlice {
                            var: hir::LocalId(local_id),
                            slice: seq,
                            body,
                            label,
                            span: f.span,
                        },
                        Flow::Falls,
                    ));
                };

                let start_e = self.expr(start, Some(TyId::INT));
                let end_e = self.expr(end, Some(TyId::INT));
                self.expect_ty(start_e.ty, TyId::INT, start_e.span, None);
                self.expect_ty(end_e.ty, TyId::INT, end_e.span, None);

                let local_id = self.resolved.lookup_binding(name.span)?;
                self.locals[local_id as usize].ty = TyId::INT;
                // The loop itself supplies the counter's value.
                self.init[local_id as usize] = Init::Assigned;

                let (body, _) = self.block(&f.body, sig);
                hir::Stmt::ForRange {
                    var: hir::LocalId(local_id),
                    start: start_e,
                    end: end_e,
                    inclusive: *inclusive,
                    body,
                    label,
                    span: f.span,
                }
            }
            ast::ForHeader::While(c) => {
                let cond = self.condition(c);
                let (body, _) = self.block(&f.body, sig);
                hir::Stmt::While { cond, body, label, span: f.span }
            }
            ast::ForHeader::Loop => {
                let (body, _) = self.block(&f.body, sig);
                hir::Stmt::Loop { body, label, span: f.span }
            }
        };

        self.loop_depth -= 1;
        self.init = entry_init;
        // A loop is treated as falling through even when it has no exit; proving
        // otherwise needs reachability analysis that arrives with MIR.
        Some((result, Flow::Falls))
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

    /// Narrow a local to its unwrapped type for the branch where it cannot be
    /// nil. Returns what to restore afterwards.
    fn apply_narrowing(
        &mut self,
        narrowing: Option<(u32, bool)>,
        in_then: bool,
    ) -> Option<(u32, TyId)> {
        let (id, is_eq) = narrowing?;
        // `x == nil` narrows in the *else*; `x != nil` narrows in the *then*.
        if is_eq == in_then {
            return None;
        }
        let TyKind::Optional(inner) = *self.types.kind(self.locals[id as usize].ty) else {
            return None;
        };
        let previous = self.locals[id as usize].ty;
        self.locals[id as usize].ty = inner;
        Some((id, previous))
    }

    fn undo_narrowing(&mut self, saved: Option<(u32, TyId)>) {
        if let Some((id, ty)) = saved {
            self.locals[id as usize].ty = ty;
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

            ast::Expr::Path(p) => self.path_expr(p),
            ast::Expr::Paren { inner, .. } => self.expr(inner, expected),

            ast::Expr::Unary { op, operand, span } => {
                let val = self.expr(operand, expected);
                self.unary(*op, val, *span)
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
                self.call(callee, args, arg_names, *span)
            }

            ast::Expr::If { cond, then, else_, span } => self.if_expr(cond, then, else_, *span),

            ast::Expr::Range { span, .. } => {
                self.not_yet(
                    *span,
                    "ranges outside a `for` header",
                    "range values arrive with the Iterate trait in Phase 2",
                );
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }

            ast::Expr::Char(span) => {
                self.not_yet(*span, "`char`", "arrives in Phase 2");
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
                            .with_note("annotate the binding, as in `let x: ?int = nil`"),
                    );
                    self.lit(ExprKind::Error, TyId::ERROR, *span)
                }
            },
            // A method's receiver is local 0, which is what makes a method
            // call and a plain call the same thing after checking.
            ast::Expr::SelfExpr(span) => match self.sigs[self.fn_index].self_ty {
                Some(ty) => hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(0)),
                    ty,
                    span: *span,
                },
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
                    Some(Res::Builtin(_)) | Some(Res::Fn(_)) => {
                        self.not_yet(
                            *span,
                            "using a function as a value",
                            "closures arrive later in Phase 2",
                        );
                        self.lit(ExprKind::Error, TyId::ERROR, *span)
                    }
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_value(ti, vi, &[], &[], *span, *span)
                    }
                    _ => self.field_access(base, name, *span),
                }
            }

            ast::Expr::StructLit(lit) => self.struct_literal(lit),

            ast::Expr::Match(m) => self.match_expr(m, expected),

            ast::Expr::Map { span, .. } => {
                self.not_yet(*span, "map literals", "maps arrive later in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Index { base, index, span } => self.index_expr(base, index, *span),
            ast::Expr::Cast { span, .. } => {
                self.not_yet(*span, "`as` casts", "arrives in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Await { span, .. } => {
                self.not_yet(*span, "`await`", "concurrency arrives in Phase 5");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Tuple { span, .. } => {
                self.not_yet(*span, "tuples", "arrives in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Slice { elems, span } => self.slice_literal(elems, expected, *span),
            ast::Expr::Closure { span, .. } => {
                self.not_yet(*span, "closures", "arrives in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Error(span) => self.lit(ExprKind::Error, TyId::ERROR, *span),
        }
    }

    fn path_expr(&mut self, p: &ast::Path) -> hir::Expr {
        match self.resolved.lookup_use(p.span) {
            Some(Res::Local(id)) => {
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
                if self.init[id as usize] == Init::Unassigned {
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
                hir::Expr {
                    kind: ExprKind::Local(hir::LocalId(id)),
                    ty: self.locals[id as usize].ty,
                    span: p.span,
                }
            }
            Some(Res::Fn(_)) | Some(Res::Builtin(_)) => {
                // Naming a function without calling it needs closures.
                self.not_yet(
                    p.span,
                    "using a function as a value",
                    "closures arrive later in Phase 2",
                );
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
            Some(Res::Variant(ti, vi)) => self.variant_value(ti, vi, &[], &[], p.span, p.span),
            // Resolution already reported this.
            None => self.lit(ExprKind::Error, TyId::ERROR, p.span),
        }
    }

    fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        arg_names: &[Option<ast::Ident>],
        span: Span,
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
                    Some(Res::Type(ti)) => {
                        self.associated_call_named(ti, &name.name, *fspan, args, span)
                    }
                    Some(Res::Variant(ti, vi)) => {
                        self.variant_value(ti, vi, args, arg_names, *fspan, span)
                    }
                    _ => self.method_call(base, name, args, span),
                };
            }
        }

        let ast::Expr::Path(p) = callee else {
            self.not_yet(callee.span(), "calling an arbitrary expression", "Phase 2");
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };

        match self.resolved.lookup_use(p.span) {
            Some(Res::Fn(id)) => {
                let sig_params = self.sigs[id as usize].params.clone();
                let ret = self.sigs[id as usize].ret;
                let decl_span = self.sigs[id as usize].name_span;

                if args.len() != sig_params.len() {
                    self.arity_error(
                        &p.text(),
                        args.len(),
                        sig_params.len(),
                        span,
                        Some(decl_span),
                    );
                }

                let mut hargs = Vec::with_capacity(args.len());
                for (i, a) in args.iter().enumerate() {
                    let want = sig_params.get(i).copied();
                    let e = self.expr(a, want);
                    if let Some(w) = want {
                        self.expect_ty(e.ty, w, e.span, Some(decl_span));
                    }
                    hargs.push(e);
                }

                hir::Expr {
                    kind: ExprKind::Call { callee: hir::FnId(id), args: hargs },
                    ty: ret,
                    span,
                }
            }

            Some(Res::Builtin(b)) => self.builtin_call(b, args, span),

            Some(Res::Local(id)) => {
                let ty = self.locals[id as usize].ty;
                self.diags.push(
                    Diagnostic::error(codes::E0205, format!("`{}` is not a function", p.text()))
                        .with_primary(p.span, format!("this is {}", self.types.with_article(ty)))
                        .with_secondary(self.locals[id as usize].span, "declared here"),
                );
                self.lit(ExprKind::Error, TyId::ERROR, span)
            }

            Some(Res::Type(ti)) => self.associated_call(ti, p, args, span),

            Some(Res::Variant(ti, vi)) => {
                self.variant_value(ti, vi, args, arg_names, p.span, span)
            }

            None => self.lit(ExprKind::Error, TyId::ERROR, span),
        }
    }

    fn builtin_call(&mut self, b: BuiltinFn, args: &[ast::Expr], span: Span) -> hir::Expr {
        match b {
            BuiltinFn::ErrorsNew => {
                if args.len() != 1 {
                    self.arity_error("errors.new", args.len(), 1, span, None);
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                }
                let m = self.expr(&args[0], Some(TyId::STR));
                self.expect_ty(m.ty, TyId::STR, m.span, None);
                hir::Expr {
                    kind: ExprKind::ErrorNew { message: Box::new(m) },
                    ty: TyId::ERR,
                    span,
                }
            }

            BuiltinFn::IoPrint => {
                if args.len() != 1 {
                    self.arity_error("io.print", args.len(), 1, span, None);
                }
                let mut hargs = Vec::new();
                for a in args {
                    let e = self.expr(a, None);
                    if !self.types.is_printable(e.ty) && !self.types.is_poisoned(e.ty) {
                        let article = self.types.with_article(e.ty);
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!(
                                    "`io.print` cannot print a `{}`",
                                    self.types.name(e.ty)
                                ),
                            )
                            .with_primary(e.span, format!("this is {}", article))
                            .with_note(
                                "`io.print` accepts int, float, bool, and str; the `Display` \
                                 trait replaces this in Phase 6",
                            ),
                        );
                    }
                    hargs.push(e);
                }
                hir::Expr {
                    kind: ExprKind::CallBuiltin { builtin: Builtin::IoPrint, args: hargs },
                    ty: TyId::UNIT,
                    span,
                }
            }
        }
    }

    /// `receiver.method(args)`.
    fn method_call(
        &mut self,
        base: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let receiver = self.expr(base, None);
        if self.types.is_poisoned(receiver.ty) {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        }

        if receiver.ty == TyId::ERR {
            if name.name != "message" {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`error` has no method `{}`", name.name),
                    )
                    .with_primary(name.span, "no such method")
                    .with_note("`error` has: message"),
                );
                return self.lit(ExprKind::Error, TyId::ERROR, span);
            }
            if !args.is_empty() {
                self.arity_error("message", args.len(), 0, span, None);
            }
            return hir::Expr {
                kind: ExprKind::ErrorMessage { base: Box::new(receiver) },
                ty: TyId::STR,
                span,
            };
        }

        if self.types.slice_elem(receiver.ty).is_some() {
            return self
                .slice_method(base, receiver, name, args, span)
                .unwrap_or_else(|| hir::Expr {
                    kind: ExprKind::Error,
                    ty: TyId::ERROR,
                    span,
                });
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

        let sig_params = self.sigs[fn_index as usize].params.clone();
        let ret = self.sigs[fn_index as usize].ret;
        let decl_span = self.sigs[fn_index as usize].name_span;

        if args.len() != sig_params.len() {
            self.arity_error(&name.name, args.len(), sig_params.len(), span, Some(decl_span));
        }

        // The receiver becomes the first argument, which is exactly how `self`
        // is stored: local 0.
        let mut hargs = vec![receiver];
        for (i, a) in args.iter().enumerate() {
            let want = sig_params.get(i).copied();
            let e = self.expr(a, want);
            if let Some(w) = want {
                self.expect_ty(e.ty, w, e.span, Some(decl_span));
            }
            hargs.push(e);
        }

        hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(fn_index), args: hargs },
            ty: ret,
            span,
        }
    }

    /// `Rect.square(2.0)` — an associated function, called through the type.
    fn associated_call(
        &mut self,
        ti: u32,
        p: &ast::Path,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let name = p.last().name.clone();
        self.associated_call_named(ti, &name, p.span, args, span)
    }

    fn associated_call_named(
        &mut self,
        ti: u32,
        method_name: &str,
        path_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let type_name = self.resolved.type_decl(ti).name.clone();
        let method_name = method_name.to_string();
        let p_span = path_span;

        let Some(fn_index) = self.resolved.method_on(ti, &method_name) else {
            self.diags.push(
                Diagnostic::error(
                    codes::E0205,
                    format!("`{}` has no associated function `{}`", type_name, method_name),
                )
                .with_primary(p_span, "no such function"),
            );
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

        let sig_params = self.sigs[fn_index as usize].params.clone();
        let ret = self.sigs[fn_index as usize].ret;
        let decl_span = self.sigs[fn_index as usize].name_span;

        if args.len() != sig_params.len() {
            let full = format!("{}.{}", type_name, method_name);
            self.arity_error(&full, args.len(), sig_params.len(), span, Some(decl_span));
        }

        let mut hargs = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let want = sig_params.get(i).copied();
            let e = self.expr(a, want);
            if let Some(w) = want {
                self.expect_ty(e.ty, w, e.span, Some(decl_span));
            }
            hargs.push(e);
        }

        hir::Expr {
            kind: ExprKind::Call { callee: hir::FnId(fn_index), args: hargs },
            ty: ret,
            span,
        }
    }

    /// The `resolved.types` index for a nominal type, if it has one.
    fn type_index_of(&self, ty: TyId) -> Option<u32> {
        let target = match *self.types.kind(ty) {
            TyKind::Struct(s) => TypeTarget::Struct(s),
            TyKind::Enum(e) => TypeTarget::Enum(e),
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
    fn let_pair(
        &mut self,
        l: &ast::LetStmt,
        elems: &[ast::BindElem],
        span: Span,
    ) -> Option<(hir::Stmt, Flow)> {
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

        let Some(init) = &l.init else {
            self.diags.push(
                Diagnostic::error(codes::E0204, "a tuple binding needs an initialiser")
                    .with_primary(span, "nothing to destructure"),
            );
            return None;
        };

        let call = self.expr(init, None);
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

    /// `check err` — propagate if the error is not nil.
    ///
    /// Defined as exactly `if err != nil { return _, err }`. It occupies its own
    /// line and is greppable, which preserves Go's central virtue: you can scan
    /// the left margin of a function and see every place it can fail.
    fn check_stmt(
        &mut self,
        expr: &ast::Expr,
        span: Span,
        sig: &Signature,
    ) -> Option<(hir::Stmt, Flow)> {
        if !sig.fallible {
            self.diags.push(
                Diagnostic::error(codes::E0303, "`check` outside a fallible function")
                    .with_primary(span, "this would return an error")
                    .with_secondary(sig.name_span, "declared here")
                    .with_note(
                        "`check` returns the error to the caller, so the enclosing function \
                         must declare `-> (T, error)`",
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
                then: hir::Block {
                    stmts: vec![hir::Stmt::Return {
                        value: Some(hir::Expr {
                            kind: ExprKind::PairNew {
                                value: Box::new(hir::Expr {
                                    kind: ExprKind::Nil,
                                    ty: TyId::ERROR,
                                    span,
                                }),
                                error: Box::new(self.reread_error(expr, span)),
                            },
                            ty: ret,
                            span,
                        }),
                        span,
                    }],
                },
                else_: None,
                span,
            },
            Flow::Falls,
        ))
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
    fn mark_checked(&mut self, expr: &ast::Expr) {
        let ast::Expr::Path(p) = expr else { return };
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

    /// Report every error binding that was never inspected.
    ///
    /// Run once at the end of the body, where all paths have merged, so a
    /// single error is reported once rather than per branch.
    fn report_unchecked_errors(&mut self) {
        let mut pending: Vec<(String, Span)> = Vec::new();
        for (i, state) in self.taint.iter().enumerate() {
            if *state == Taint::Unchecked && !self.locals[i].synthetic {
                pending.push((self.locals[i].name.clone(), self.locals[i].span));
            }
        }
        for (name, span) in pending {
            self.diags.push(
                Diagnostic::error(codes::E0302, format!("`{}` is never checked", name))
                    .with_primary(span, "this error goes out of scope uninspected")
                    .with_note(
                        "silently dropping errors is the single most common source of \
                         production failures in languages that permit it",
                    )
                    .with_note(
                        "to propagate, write `check` on its own line; to handle it here, \
                         test `err != nil`",
                    ),
            );
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
            match elem_ty {
                None if !self.types.is_poisoned(v.ty) => elem_ty = Some(v.ty),
                Some(want) => self.expect_ty(v.ty, want, v.span, None),
                None => {}
            }
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
        let Some(elem) = self.types.slice_elem(seq.ty) else {
            let found = self.types.with_article(seq.ty);
            self.diags.push(
                Diagnostic::error(codes::E0200, "only a slice can be index-assigned")
                    .with_primary(seq.span, format!("this is {}", found)),
            );
            return None;
        };

        // A slice is a copy-on-write value, so writing into it changes the
        // binding, which must therefore be mutable.
        self.require_mutable_slice_binding(base, "assigned into")?;

        let i = self.expr(index, Some(TyId::INT));
        self.expect_ty(i.ty, TyId::INT, i.span, None);

        let value = self.expr(&a.value, Some(elem));
        let value = match a.op.to_binary() {
            None => {
                self.expect_ty(value.ty, elem, value.span, None);
                value
            }
            Some(binop) => {
                let current = hir::Expr {
                    kind: ExprKind::Index {
                        base: Box::new(self.expr(base, None)),
                        index: Box::new(self.expr(index, Some(TyId::INT))),
                    },
                    ty: elem,
                    span,
                };
                self.binary(binop, current, value, a.span)
            }
        };

        Some((
            hir::Stmt::SetIndex { base: seq, index: i, value, span: a.span },
            Flow::Falls,
        ))
    }

    /// Mutating a slice changes the binding that holds it, because slices have
    /// value semantics. Report when that binding is immutable.
    fn require_mutable_slice_binding(&mut self, base: &ast::Expr, what: &str) -> Option<u32> {
        let ast::Expr::Path(p) = base else {
            self.not_yet(
                base.span(),
                "mutating a slice that is not a plain binding",
                "assign it to a `var` first",
            );
            return None;
        };
        let Some(Res::Local(id)) = self.resolved.lookup_use(p.span) else {
            return None;
        };
        if !self.locals[id as usize].mutable {
            let name = self.locals[id as usize].name.clone();
            let decl = self.locals[id as usize].span;
            let mut d = Diagnostic::error(
                codes::E0114,
                format!("`{}` cannot be {}", name, what),
            )
            .with_primary(p.span, "this binding is immutable")
            .with_secondary(decl, "declared with `let` here")
            .with_note(
                "a slice is a copy-on-write value, so changing its contents changes the \
                 binding; declare it `var`",
            );
            if let Some(kw) = self.let_keyword_span(decl) {
                d = d.with_fix(Fix::replace("make the binding mutable", kw, "var"));
            }
            self.diags.push(d);
            return None;
        }
        Some(id)
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
                let id = self.require_mutable_slice_binding(base, "pushed to")?;
                let v = self.expr(&args[0], Some(elem));
                self.expect_ty(v.ty, elem, v.span, None);
                // `push` is a statement, not an expression; the checker returns
                // unit and MIR emits the mutation.
                Some(hir::Expr {
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
                                kind: ExprKind::Block(hir::Block {
                                    stmts: vec![hir::Stmt::SlicePush {
                                        local: hir::LocalId(id),
                                        value: v,
                                        span,
                                    }],
                                }),
                                ty: TyId::UNIT,
                                span,
                            },
                            span,
                        }],
                    },
                    ty: TyId::UNIT,
                    span,
                })
            }

            _ => {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0205,
                        format!("`{}` has no method `{}`", self.types.name(seq.ty), name.name),
                    )
                    .with_primary(name.span, "no such method")
                    .with_note("a slice has: len, get, push"),
                );
                Some(self.lit(ExprKind::Error, TyId::ERROR, span))
            }
        }
    }

    // ---- enums ------------------------------------------------------------

    /// `Circle(radius: 1.0)`, `Number(3.0)`, or a unit variant like `Point`.
    fn variant_value(
        &mut self,
        ti: u32,
        vi: u32,
        args: &[ast::Expr],
        arg_names: &[Option<ast::Ident>],
        path_span: Span,
        span: Span,
    ) -> hir::Expr {
        let Some(TypeTarget::Enum(eid)) = self.type_ids[ti as usize] else {
            return self.lit(ExprKind::Error, TyId::ERROR, span);
        };
        let enum_ty = self.types.enum_ty(eid);

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
            self.expect_ty(e.ty, want, e.span, Some(decl_span));
            slots[index] = Some(e);
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
        let scrutinee = self.expr(&m.scrutinee, None);
        let scrut_ty = scrutinee.ty;

        if m.arms.is_empty() {
            self.diags.push(
                Diagnostic::error(codes::E0210, "a `match` needs at least one arm")
                    .with_primary(m.span, "no arms")
                    .with_note("an empty match can never produce a value"),
            );
            return self.lit(ExprKind::Error, TyId::ERROR, m.span);
        }

        let entry_init = self.init.clone();
        let mut arms = Vec::with_capacity(m.arms.len());
        let mut result_ty: Option<TyId> = None;
        let mut arm_spans: Vec<(Span, TyId)> = Vec::new();

        // Arms are checked in order so a binding can be narrowed. Once an
        // earlier arm has matched `nil`, a later binding pattern cannot receive
        // one, so it binds the unwrapped type — which is what
        // SPECIFICATION.md section 3.3 shows.
        let mut nil_covered = false;

        for arm in &m.arms {
            self.init = entry_init.clone();
            let bind_ty = match *self.types.kind(scrut_ty) {
                TyKind::Optional(inner) if nil_covered => inner,
                _ => scrut_ty,
            };
            let pattern = self.pattern_with(&arm.pattern, scrut_ty, bind_ty);
            if arm.guard.is_none() && covers_nil(&arm.pattern) {
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

            let body = match &arm.body {
                ast::MatchBody::Expr(e) => self.expr(e, expected.or(result_ty)),
                ast::MatchBody::Block(b) => self.match_block(b, expected.or(result_ty)),
            };

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

        // A binding introduced by one arm's pattern is not in scope after the
        // match, so the entry state is what survives.
        self.init = entry_init;

        self.check_exhaustive(m, &arms, scrut_ty);

        let ty = result_ty.unwrap_or(TyId::UNIT);
        hir::Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            ty,
            span: m.span,
        }
    }

    /// A block arm. A block that ends in an expression yields it; otherwise the
    /// arm produces unit.
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

    /// The enclosing function's signature, so a nested block still checks
    /// `return` against the right type.
    fn current_signature(&self) -> Signature {
        let s = &self.sigs[self.fn_index];
        Signature {
            params: s.params.clone(),
            ret: s.ret,
            fallible: s.fallible,
            name_span: s.name_span,
            self_ty: s.self_ty,
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
        match p {
            ast::Pattern::Wildcard(_) => hir::Pattern::Wildcard,

            ast::Pattern::Binding(name) => match self.resolved.lookup_binding(name.span) {
                Some(local) => {
                    self.locals[local as usize].ty = bind_ty;
                    self.init[local as usize] = Init::Assigned;
                    hir::Pattern::Binding(hir::LocalId(local))
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
                let lit = self.expr(e, Some(scrut));
                self.expect_ty(lit.ty, scrut, lit.span, None);
                match lit.kind {
                    ExprKind::Int(v) => hir::Pattern::Int(v),
                    ExprKind::Float(v) => hir::Pattern::Float(v),
                    ExprKind::Str(s) => hir::Pattern::Str(s),
                    ExprKind::Bool(b) => hir::Pattern::Bool(b),
                    ExprKind::Unary { op: hir::UnOp::NegInt, operand } => match operand.kind {
                        ExprKind::Int(v) => hir::Pattern::Int(-v),
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
                match (&a.kind, &b.kind) {
                    (ExprKind::Int(x), ExprKind::Int(y)) => {
                        if x > y {
                            self.diags.push(
                                Diagnostic::warning(codes::E0210, "this range is empty")
                                    .with_primary(*span, format!("{}..{} matches nothing", x, y)),
                            );
                        }
                        hir::Pattern::IntRange {
                            start: *x,
                            end: *y,
                            inclusive: *inclusive,
                        }
                    }
                    _ => {
                        self.not_yet(*span, "non-integer range patterns", "Phase 2");
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
                            self.not_yet(*span, "struct call patterns", "use `Name{ … }`");
                            hir::Pattern::Wildcard
                        }
                        _ => hir::Pattern::Wildcard,
                    },
                    _ => hir::Pattern::Wildcard,
                }
            }

            ast::Pattern::Struct { path, fields, span, .. } => {
                let Some(Res::Type(ti)) = self.resolved.lookup_use(path.span) else {
                    return hir::Pattern::Wildcard;
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
                    let sub = match &f.pattern {
                        Some(p) => self.pattern(p, fty),
                        // `Point{ x }` binds `x` to the field's value.
                        None => match self.resolved.lookup_binding(f.name.span) {
                            Some(local) => {
                                self.locals[local as usize].ty = fty;
                                self.init[local as usize] = Init::Assigned;
                                hir::Pattern::Binding(hir::LocalId(local))
                            }
                            None => hir::Pattern::Wildcard,
                        },
                    };
                    out.push((index as u32, sub));
                }
                hir::Pattern::Struct { struct_id: sid, fields: out }
            }

            ast::Pattern::Or { alts, .. } => {
                hir::Pattern::Or(alts.iter().map(|a| self.pattern(a, scrut)).collect())
            }

            ast::Pattern::Tuple { span, .. } => {
                self.not_yet(*span, "tuple patterns", "tuples arrive later in Phase 2");
                hir::Pattern::Wildcard
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
                        }
                    }
                }
                out
            }
        };

        hir::Pattern::Variant { enum_id: eid, variant: vi, fields: sub }
    }

    fn check_exhaustive(&mut self, m: &ast::MatchExpr, arms: &[hir::MatchArm], scrut: TyId) {
        if self.types.is_poisoned(scrut) {
            return;
        }
        // A guarded arm may fail at run time, so it cannot make a match
        // exhaustive and is excluded from the coverage set.
        let unguarded: Vec<&hir::Pattern> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .map(|a| &a.pattern)
            .collect();

        let missing = exhaustive::missing_patterns(scrut, &unguarded, self.types);
        if missing.is_empty() {
            return;
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
    }

    // ---- structs ----------------------------------------------------------

    /// `Point{ x: 1.0, y: 2.0 }`.
    ///
    /// Every field must be given unless `..base` supplies the rest. There are
    /// no zero values in Kite, which removes Go's most common production bug:
    /// a forgotten field silently becoming `0`, `""`, or `nil`.
    fn struct_literal(&mut self, lit: &ast::StructLit) -> hir::Expr {
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

        let struct_ty = self.types.struct_ty(sid);
        let field_count = self.types.struct_def(sid).fields.len();

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
            Some((index, ty)) => hir::Expr {
                kind: ExprKind::FieldGet { base: Box::new(obj), index: index as u32 },
                ty,
                span,
            },
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
                if self.resolved.method_on(0, &name.name).is_some() {
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

        let value = self.expr(&a.value, Some(ty));
        let value = match a.op.to_binary() {
            None => {
                self.expect_ty(value.ty, ty, value.span, Some(decl_span));
                value
            }
            Some(binop) => {
                let current = hir::Expr {
                    kind: ExprKind::FieldGet {
                        base: Box::new(self.expr(base, None)),
                        index: index as u32,
                    },
                    ty,
                    span,
                };
                self.binary(binop, current, value, a.span)
            }
        };

        Some((
            hir::Stmt::SetField {
                base: obj,
                index: index as u32,
                value,
                span: a.span,
            },
            Flow::Falls,
        ))
    }

    fn if_expr(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        else_: &ast::ElseBranch,
        span: Span,
    ) -> hir::Expr {
        // An inline `if` narrows an optional exactly as the statement form
        // does, which is why no `?.` or `??` operator is needed.
        let narrowing = self.nil_test(cond);
        let c = self.condition(cond);

        let narrowed = self.apply_narrowing(narrowing, true);
        let t = self.block_value(then);
        self.undo_narrowing(narrowed);

        let e = match else_ {
            ast::ElseBranch::Block(b) => {
                let narrowed = self.apply_narrowing(narrowing, false);
                let v = self.block_value(b);
                self.undo_narrowing(narrowed);
                v
            }
            ast::ElseBranch::If(nested) => {
                let Some(inner_else) = nested.else_.as_deref() else {
                    return self.lit(ExprKind::Error, TyId::ERROR, span);
                };
                self.if_expr(&nested.cond, &nested.then, inner_else, nested.span)
            }
        };

        let ty = if t.ty == TyId::NEVER {
            e.ty
        } else if e.ty == TyId::NEVER {
            t.ty
        } else {
            if !self.types.satisfies(e.ty, t.ty) && !self.types.is_poisoned(t.ty) && !self.types.is_poisoned(e.ty) {
                self.diags.push(
                    Diagnostic::error(codes::E0200, "`if` branches have different types")
                        .with_primary(e.span, format!("this branch is {}", self.types.with_article(e.ty)))
                        .with_secondary(t.span, format!("this branch is {}", self.types.with_article(t.ty)))
                        .with_note("every branch of a value `if` must produce the same type"),
                );
            }
            t.ty
        };

        hir::Expr {
            kind: ExprKind::If { cond: Box::new(c), then: Box::new(t), else_: Box::new(e) },
            ty,
            span,
        }
    }

    /// A block used for its value must be a single expression. Kite has no
    /// implicit tail expression in statement blocks.
    fn block_value(&mut self, b: &ast::Block) -> hir::Expr {
        match b.stmts.as_slice() {
            [ast::Stmt::Expr(e)] => self.expr(e, None),
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

        if l.ty != r.ty {
            self.mismatched_operands(op, &l, &r, span);
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        }

        let t = l.ty;
        let resolved = match (op, t) {
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

            // Aggregates compare structurally, per the specification.
            (B::Eq, _) if self.types.is_equatable(t) => Some((H::EqValue, TyId::BOOL)),
            (B::Ne, _) if self.types.is_equatable(t) => Some((H::NeValue, TyId::BOOL)),

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
            if op.is_comparison() && !self.types.is_ordered(t) {
                d = d.with_note(format!("`{}` is not ordered", self.types.name(t)));
            }
            if matches!(op, B::Eq | B::Ne) && !self.types.is_equatable(t) {
                d = d.with_note(
                    "equality is structural, so every field must itself be equatable; \
                     functions and trait objects are not",
                );
            }
            self.diags.push(d);
            return hir::Expr { kind: ExprKind::Error, ty: TyId::ERROR, span };
        };

        // Float equality is a footgun the specification calls out.
        if matches!(hop, H::EqFloat | H::NeFloat) {
            self.diags.push(
                Diagnostic::warning(codes::E0201, "comparing floats for exact equality")
                    .with_primary(span, "floating-point equality is rarely what you want")
                    .with_note("use `math.approx_eq` to compare with a tolerance"),
            );
        }

        hir::Expr {
            kind: ExprKind::Binary { op: hop, lhs: Box::new(l), rhs: Box::new(r) },
            ty,
            span,
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

    fn text(&self, span: Span) -> &'a str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Decode a string literal's contents. Phase 1 handles escapes; string
    /// interpolation arrives with `Display` in Phase 2.
    fn string_value(&mut self, span: Span) -> String {
        let raw = self.text(span);
        let inner = if let Some(s) = raw.strip_prefix("\"\"\"") {
            s.strip_suffix("\"\"\"").unwrap_or(s).trim_start_matches('\n')
        } else {
            let s = raw.strip_prefix('"').unwrap_or(raw);
            s.strip_suffix('"').unwrap_or(s)
        };

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
                        None => self.diags.push(
                            Diagnostic::error(codes::E0003, "invalid unicode escape")
                                .with_primary(span, format!("`\\u{{{}}}` is not a character", hex)),
                        ),
                    }
                }
                Some('(') => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            "string interpolation is not available yet",
                        )
                        .with_primary(span, "`\\(...)` needs the `Display` trait")
                        .with_note("traits arrive in Phase 2; see docs/06-roadmap.md"),
                    );
                    return out;
                }
                Some(other) => {
                    self.diags.push(
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

    fn expect_ty(&mut self, found: TyId, expected: TyId, span: Span, because: Option<Span>) {
        if self.types.satisfies(found, expected) {
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
        if self.types.is_numeric(found) && self.types.is_numeric(expected) {
            d = d.with_note(format!(
                "Kite performs no implicit numeric conversion; write `... as {}`",
                self.types.name(expected)
            ));
        }
        self.diags.push(d);
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
        let before = &self.src[..name_span.start as usize];
        let trimmed = before.trim_end();
        if trimmed.ends_with("let") {
            let end = trimmed.len() as u32;
            Some(Span::new(name_span.file, end - 3, end))
        } else {
            None
        }
    }

    fn not_yet(&mut self, span: Span, what: &str, when: &str) {
        self.diags.push(
            Diagnostic::error(codes::E0200, format!("{} is not implemented yet", what))
                .with_primary(span, "not supported by this compiler version")
                .with_note(when.to_string()),
        );
    }
}

/// Whether a surface pattern matches `nil`.
fn covers_nil(p: &ast::Pattern) -> bool {
    match p {
        ast::Pattern::Nil(_) | ast::Pattern::Wildcard(_) | ast::Pattern::Binding(_) => true,
        ast::Pattern::Or { alts, .. } => alts.iter().any(covers_nil),
        _ => false,
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
fn check_impls(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    type_ids: &[Option<TypeTarget>],
    types: &Types,
    diags: &mut DiagBag,
) {
    // (trait index, type index) -> the span that first claimed it. Exactly one
    // implementation per pair is what makes trait resolution decidable.
    let mut claimed: std::collections::HashMap<(u32, u32), Span> =
        std::collections::HashMap::new();

    for item in &file.items {
        let ast::Item::Impl(imp) = item else { continue };
        let Some(tp) = &imp.trait_path else { continue };

        let (Some(ti), Some(target)) = (
            resolved.type_by_name(tp.name()),
            resolved.type_by_name(imp.self_ty.name()),
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
            let Some((_, decl)) = def.method(&m.name.name) else {
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
                    .with_secondary(decl.span, "declared here"),
                );
            }

            if decl.params.len() != m.params.len() {
                diags.push(
                    Diagnostic::error(
                        codes::E0113,
                        format!(
                            "`{}` takes {} parameter{}, but the trait declares {}",
                            m.name.name,
                            m.params.len(),
                            if m.params.len() == 1 { "" } else { "s" },
                            decl.params.len()
                        ),
                    )
                    .with_primary(m.sig_span, "signature does not match")
                    .with_secondary(decl.span, "declared here"),
                );
            }
        }
    }
}

/// The arena type for a declared name.
fn named_ty(target: Option<TypeTarget>, types: &mut Types) -> TyId {
    match target {
        Some(TypeTarget::Struct(s)) => types.struct_ty(s),
        Some(TypeTarget::Enum(e)) => types.enum_ty(e),
        Some(TypeTarget::Trait(t)) => types.dyn_ty(t),
        None => TyId::ERROR,
    }
}

/// Resolve a surface type, consulting the module's declared types before
/// falling back to the primitives.
fn resolve_named_ty(
    t: &ast::Type,
    resolved: &ResolveMap,
    type_ids: &[Option<TypeTarget>],
    types: &mut Types,
    diags: &mut DiagBag,
) -> TyId {
    match t {
        ast::Type::Path(p) if p.is_simple() => {
            if let Some(prim) = Types::primitive_from_name(p.name()) {
                return prim;
            }
            match resolved.type_by_name(p.name()) {
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
                    other => named_ty(other, types),
                },
                None => resolve_ty(t, types, diags),
            }
        }
        ast::Type::Slice { elem, .. } => {
            let e = resolve_named_ty(elem, resolved, type_ids, types, diags);
            types.slice_of(e)
        }
        ast::Type::Map { key, value, .. } => {
            let k = resolve_named_ty(key, resolved, type_ids, types, diags);
            let v = resolve_named_ty(value, resolved, type_ids, types, diags);
            types.map_of(k, v)
        }
        ast::Type::Optional { inner, .. } => {
            let i = resolve_named_ty(inner, resolved, type_ids, types, diags);
            types.optional_of(i)
        }
        ast::Type::Tuple { elems, .. } => {
            let es: Vec<TyId> = elems
                .iter()
                .map(|e| resolve_named_ty(e, resolved, type_ids, types, diags))
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
                .map(|p| resolve_named_ty(p, resolved, type_ids, types, diags))
                .collect();
            let r = match ret {
                Some(r) => resolve_named_ty(r, resolved, type_ids, types, diags),
                None => TyId::UNIT,
            };
            types.fn_of(ps, r)
        }
        ast::Type::Dyn { path, span } => match resolved.type_by_name(path.name()) {
            Some(i) => match type_ids[i as usize] {
                Some(TypeTarget::Trait(tr)) => types.dyn_ty(tr),
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

        ast::Type::Path(p) => {
            diags.push(
                Diagnostic::error(codes::E0204, "generic types are not available yet")
                    .with_primary(p.span, "type arguments need generics")
                    .with_note("generics arrive later in Phase 2; see docs/06-roadmap.md"),
            );
            TyId::ERROR
        }
        ast::Type::Dyn { span, .. } => {
            diags.push(
                Diagnostic::error(codes::E0204, "`dyn` is not available yet")
                    .with_primary(*span, "trait objects need traits")
                    .with_note("traits arrive later in Phase 2; see docs/06-roadmap.md"),
            );
            TyId::ERROR
        }
        ast::Type::Error(_) => TyId::ERROR,
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

fn parse_int(text: &str) -> Option<i64> {
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

fn parse_float(text: &str) -> Option<f64> {
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
