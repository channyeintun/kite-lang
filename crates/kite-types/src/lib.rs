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
use kite_hir::{Builtin, ExprKind, TyId, Types};
use kite_resolve::{BuiltinFn, Res, ResolveMap};
use kite_span::Span;

pub fn check(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    src: &str,
    diags: &mut DiagBag,
) -> hir::Program {
    let mut types = Types::new();
    let mut fns = Vec::new();

    // Signatures first, so calls can be checked in either direction.
    let mut sigs = Vec::new();
    for sig in &resolved.fns {
        let ast::Item::Fn(f) = &file.items[sig.decl_index] else {
            unreachable!("signature index points at a function")
        };
        let params: Vec<TyId> = f
            .params
            .iter()
            .map(|p| resolve_ty(&p.ty, &mut types, diags))
            .collect();
        let ret = match &f.ret {
            None => TyId::UNIT,
            Some(r) => resolve_ty(r.value_type(), &mut types, diags),
        };
        sigs.push(Signature {
            params,
            ret,
            fallible: f.ret.as_ref().is_some_and(|r| r.is_fallible()),
            name_span: f.name.span,
        });
    }

    for (i, sig) in resolved.fns.iter().enumerate() {
        let ast::Item::Fn(f) = &file.items[sig.decl_index] else {
            unreachable!()
        };
        let mut checker = Checker {
            resolved,
            sigs: &sigs,
            types: &mut types,
            src,
            diags,
            fn_index: i,
            locals: Vec::new(),
            init: Vec::new(),
            loop_depth: 0,
        };
        let func = checker.check_fn(f, &sigs[i]);
        fns.push(func);
    }

    hir::Program {
        types,
        fns,
        entry: resolved.fn_by_name("main").map(hir::FnId),
    }
}

struct Signature {
    params: Vec<TyId>,
    ret: TyId,
    fallible: bool,
    name_span: Span,
}

struct Checker<'a> {
    resolved: &'a ResolveMap,
    sigs: &'a [Signature],
    /// The interned type arena, built up as declarations are checked.
    types: &'a mut Types,
    src: &'a str,
    diags: &'a mut DiagBag,
    fn_index: usize,
    locals: Vec<hir::Local>,
    /// Definite-assignment state, parallel to `locals`.
    init: Vec<Init>,
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

    fn check_fn(&mut self, f: &ast::FnDecl, sig: &Signature) -> hir::Function {
        let infos = &self.resolved.locals[self.fn_index];

        // Every local gets a slot up front; parameter types come from the
        // signature and the rest are filled in as their `let` is checked.
        self.locals = infos
            .iter()
            .enumerate()
            .map(|(i, info)| hir::Local {
                name: info.name.clone(),
                ty: sig.params.get(i).copied().unwrap_or(TyId::ERROR),
                mutable: info.mutable,
                span: info.span,
                synthetic: info.synthetic,
            })
            .collect();

        // Parameters always hold a value; everything else starts unassigned
        // and is marked as its `let` or `var` is checked.
        self.init = (0..self.locals.len())
            .map(|i| {
                if i < f.params.len() {
                    Init::Assigned
                } else {
                    Init::Unassigned
                }
            })
            .collect();

        let (body, flow) = self.block(&f.body, sig);

        if sig.ret != TyId::UNIT && flow == Flow::Falls {
            self.diags.push(
                Diagnostic::error(codes::E0203, "not every path returns a value")
                    .with_primary(
                        Span::empty_at(f.body.span.file, f.body.span.end.saturating_sub(1)),
                        "control reaches the end of the function here",
                    )
                    .with_secondary(
                        f.ret.as_ref().map(|r| r.span()).unwrap_or(sig.name_span),
                        format!("`{}` declared here", self.types.name(sig.ret)),
                    ),
            );
        }

        hir::Function {
            name: f.name.name.clone(),
            is_pub: f.is_pub,
            is_async: f.is_async,
            param_count: f.params.len(),
            locals: std::mem::take(&mut self.locals),
            ret: sig.ret,
            body,
            span: f.span,
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

            // Phase 3 introduces the taint analysis these depend on.
            ast::Stmt::Check { span, .. } => {
                self.not_yet(*span, "`check`", "error handling arrives in Phase 3");
                None
            }
            ast::Stmt::Defer { span, .. } => {
                self.not_yet(*span, "`defer`", "scope-exit release arrives in Phase 3");
                None
            }
            ast::Stmt::Error(_) => None,
        }
    }

    fn let_stmt(&mut self, l: &ast::LetStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        let ast::Binding::Name(name) = &l.binding else {
            self.not_yet(
                l.binding.span(),
                "tuple bindings",
                "`let (v, err) = f()` arrives in Phase 3",
            );
            return None;
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
        let ast::Expr::Path(p) = &a.target else {
            self.not_yet(
                a.target.span(),
                "assignment to fields and indices",
                "structs and slices arrive in Phase 2",
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
            Some(ast::ReturnValue::Pair { span, .. }) | Some(ast::ReturnValue::Fail { span, .. }) => {
                if !sig.fallible {
                    self.diags.push(
                        Diagnostic::error(
                            codes::E0200,
                            "returning a pair from a function that is not fallible",
                        )
                        .with_primary(*span, "two values returned here")
                        .with_secondary(sig.name_span, "declare `-> (T, error)` to return a pair"),
                    );
                } else {
                    self.not_yet(*span, "fallible returns", "error handling arrives in Phase 3");
                }
                None
            }
        }
    }

    fn if_stmt(&mut self, i: &ast::IfStmt, sig: &Signature) -> Option<(hir::Stmt, Flow)> {
        let cond = self.condition(&i.cond);

        // Each branch is checked from the same entry state, and the states are
        // merged at the join. A branch that diverges contributes nothing to the
        // join, because control never arrives from it.
        let entry_init = self.init.clone();

        let (then, then_flow) = self.block(&i.then, sig);
        let then_init = std::mem::replace(&mut self.init, entry_init.clone());

        let (else_, else_flow) = match i.else_.as_deref() {
            None => (None, Flow::Falls),
            Some(ast::ElseBranch::Block(b)) => {
                let (blk, f) = self.block(b, sig);
                (Some(blk), f)
            }
            Some(ast::ElseBranch::If(nested)) => {
                let (stmt, f) = self.if_stmt(nested, sig)?;
                (Some(hir::Block { stmts: vec![stmt] }), f)
            }
        };
        let else_init = std::mem::take(&mut self.init);

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
                    self.not_yet(
                        iter.span(),
                        "iterating anything but a range",
                        "the Iterate trait arrives in Phase 2",
                    );
                    self.loop_depth -= 1;
                    return None;
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

            ast::Expr::Call { callee, args, span } => self.call(callee, args, *span),

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
            ast::Expr::Nil(span) => {
                self.not_yet(*span, "`nil`", "optionals arrive in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::SelfExpr(span) => {
                self.not_yet(*span, "`self`", "methods arrive in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Field { span, .. } => {
                self.not_yet(*span, "field access", "structs arrive in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
            ast::Expr::Index { span, .. } => {
                self.not_yet(*span, "indexing", "slices and maps arrive in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
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
            ast::Expr::Slice { span, .. } => {
                self.not_yet(*span, "slice literals", "arrives in Phase 2");
                self.lit(ExprKind::Error, TyId::ERROR, *span)
            }
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
                // Naming a function without calling it needs function types.
                self.not_yet(
                    p.span,
                    "using a function as a value",
                    "function types arrive in Phase 2",
                );
                self.lit(ExprKind::Error, TyId::ERROR, p.span)
            }
            // Resolution already reported this.
            None => self.lit(ExprKind::Error, TyId::ERROR, p.span),
        }
    }

    fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr], span: Span) -> hir::Expr {
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

            Some(Res::Builtin(BuiltinFn::IoPrint)) => {
                if args.len() != 1 {
                    self.arity_error("io.print", args.len(), 1, span, None);
                }
                let mut hargs = Vec::new();
                for a in args {
                    let e = self.expr(a, None);
                    if !self.types.is_printable(e.ty) && !self.types.is_poisoned(e.ty) {
                        self.diags.push(
                            Diagnostic::error(
                                codes::E0200,
                                format!("`io.print` cannot print a `{}`", self.types.name(e.ty)),
                            )
                            .with_primary(e.span, format!("this is {}", self.types.with_article(e.ty)))
                            .with_note("`io.print` accepts int, float, bool, and str"),
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

            Some(Res::Local(id)) => {
                let ty = self.locals[id as usize].ty;
                self.diags.push(
                    Diagnostic::error(codes::E0205, format!("`{}` is not a function", p.text()))
                        .with_primary(p.span, format!("this is {}", self.types.with_article(ty)))
                        .with_secondary(self.locals[id as usize].span, "declared here"),
                );
                self.lit(ExprKind::Error, TyId::ERROR, span)
            }

            None => self.lit(ExprKind::Error, TyId::ERROR, span),
        }
    }

    fn if_expr(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        else_: &ast::ElseBranch,
        span: Span,
    ) -> hir::Expr {
        let c = self.condition(cond);
        let t = self.block_value(then);
        let e = match else_ {
            ast::ElseBranch::Block(b) => self.block_value(b),
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

fn short_circuit(op: ast::BinaryOp) -> Option<hir::BinOp> {
    match op {
        ast::BinaryOp::And => Some(hir::BinOp::And),
        ast::BinaryOp::Or => Some(hir::BinOp::Or),
        _ => None,
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
