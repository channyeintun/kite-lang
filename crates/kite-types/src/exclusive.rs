//! Exclusivity: one object, two argument names, at least one of them written.
//!
//! Kite has no borrow checker and needs none — every target collects, so no
//! program here can reach freed memory. What a collector does not prevent is a
//! struct arriving at a call twice under two `var` parameters:
//!
//! ```kite
//! fn transfer(var from: Account, var to: Account, amount: int) {
//!     from.balance = from.balance - amount
//!     to.balance   = to.balance   + amount
//! }
//!
//! transfer(a, a, 50)      // sets balance to 50, then back to 100
//! ```
//!
//! Both parameters name one object, each write lands on the other's field, and
//! the program reports nothing. Structs are GC references and are always passed
//! by reference ([§1.3](../../../SPECIFICATION.md)), so this is reachable from
//! ordinary code without a pointer anywhere in sight.
//!
//! The rule is one line: **while an object is being written through one
//! argument, no other argument of the same call may name it.** It needs no
//! ownership, no lifetimes and no annotations. An argument that can alias is a
//! path rooted at a local — `a`, `o.inner`, `xs[2]` — and two paths name the
//! same object when one is a prefix of the other.
//!
//! What it deliberately does not see: aliasing established through the heap.
//! Two fields assigned the same reference in another function look distinct
//! here, and `f(p.left, p.right)` is accepted even where both were built from
//! one `Account`. Closing that hole is alias analysis, which is ownership and
//! lifetimes — the rest of a borrow checker, and the part that costs a
//! language its simplicity. A collector underneath is what makes the incomplete
//! version worth having: the cases this misses stay memory-safe, they are
//! merely surprising.
//!
//! Two things that would be rules in Rust are not rules here, because Kite's
//! value semantics already settle them:
//!
//! - **Slices and maps are copy-on-write values.** A `var [T]` parameter is the
//!   callee's own copy; a push inside is invisible to the caller, so two slice
//!   arguments cannot interfere.
//! - **`for x in xs` iterates a snapshot.** Growing `xs` in the body is defined
//!   and terminating, so iteration needs no loan on what it walks.
//!
//! A call through `dyn Trait` is checked as well: the implementation is not
//! known here, so a parameter counts as written when any row of the trait's
//! vtable declares it `var`.

use kite_diag::{codes, DiagBag, Diagnostic};
use kite_hir::{self as hir, ExprKind, LocalId, Stmt, TyId, TyKind, Types};
use kite_span::Span;

/// Check every call in the program. Runs after type checking, on HIR, before
/// monomorphisation — so a generic function is checked once rather than once
/// per instantiation.
pub fn check(program: &hir::Program, diags: &mut DiagBag) {
    for func in &program.fns {
        let mut cx = Checker { program, func, diags };
        cx.block(&func.body);
    }
}

struct Checker<'a> {
    program: &'a hir::Program,
    func: &'a hir::Function,
    diags: &'a mut DiagBag,
}

/// One parameter of whatever the call reaches. Flattened out of `hir::Local`
/// because a virtual call has no single callee to borrow it from.
struct Param {
    name: String,
    ty: TyId,
    mutable: bool,
}

// ---------------------------------------------------------------------------
// Places
// ---------------------------------------------------------------------------

/// A path to an object, rooted at a local. This is the whole of what the pass
/// knows how to name; anything else — a call result, a struct literal — is a
/// fresh object no other argument can be holding.
struct Place {
    root: LocalId,
    path: Vec<Step>,
    /// The path as written, for the diagnostic. Kept alongside rather than
    /// recovered later, because field names need the base's type and this is
    /// the one point where it is in hand.
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Field(u32),
    /// `xs[i]`. A literal index compares exactly. Anything else could be any
    /// element, so it is taken to overlap every index into the same slice —
    /// `swap(xs[i], xs[j])` is rejected, which is right: it is a bug when the
    /// two are equal, and the compiler cannot tell that they are not.
    Index(Option<i64>),
}

/// The place an expression names, if it names one at all.
fn place_of(expr: &hir::Expr, types: &Types, func: &hir::Function) -> Option<Place> {
    match &expr.kind {
        ExprKind::Local(id) => Some(Place {
            root: *id,
            path: Vec::new(),
            text: func.local(*id).name.clone(),
        }),
        ExprKind::FieldGet { base, index } => {
            let mut place = place_of(base, types, func)?;
            place.text.push('.');
            place.text.push_str(&field_name(base.ty, *index, types));
            place.path.push(Step::Field(*index));
            Some(place)
        }
        ExprKind::Index { base, index } => {
            let mut place = place_of(base, types, func)?;
            let constant = match index.kind {
                ExprKind::Int(n) => Some(n),
                _ => None,
            };
            place.text.push('[');
            match constant {
                Some(n) => place.text.push_str(&n.to_string()),
                None => place.text.push_str(".."),
            }
            place.text.push(']');
            place.path.push(Step::Index(constant));
            Some(place)
        }
        _ => None,
    }
}

fn field_name(base: TyId, index: u32, types: &Types) -> String {
    match types.kind(base) {
        TyKind::Struct(id) => types
            .struct_def(*id)
            .fields
            .get(index as usize)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| index.to_string()),
        _ => index.to_string(),
    }
}

/// Whether two places can name the same object: same root, and neither path
/// takes a step the other definitely does not.
///
/// The loop runs to the shorter of the two, so a prefix relation counts —
/// `o` and `o.inner` overlap, because writing through `o` writes `o.inner`.
fn overlaps(a: &Place, b: &Place) -> bool {
    if a.root != b.root {
        return false;
    }
    for (x, y) in a.path.iter().zip(&b.path) {
        let distinct = match (x, y) {
            (Step::Field(i), Step::Field(j)) => i != j,
            (Step::Index(Some(i)), Step::Index(Some(j))) => i != j,
            // An unknown index could be either. A field and an index cannot
            // both apply to one type, so a mismatch here means the paths
            // diverged already.
            (Step::Index(_), Step::Index(_)) => false,
            _ => true,
        };
        if distinct {
            return false;
        }
    }
    true
}

/// Types whose mutation is visible through another name: those passed by
/// reference. Slices, maps and tuples are copy-on-write values, and the
/// primitives are copied outright, so none of them can carry a write across.
///
/// A generic parameter is left out on purpose. The pass runs before
/// monomorphisation, where `T` is not yet known to be a struct, and reporting
/// per-instantiation would mean the same source line diagnosed several times.
fn is_reference(ty: TyId, types: &Types) -> bool {
    matches!(types.kind(ty), TyKind::Struct(_) | TyKind::Dyn(_))
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

impl Checker<'_> {
    fn block(&mut self, block: &hir::Block) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { init, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
            }
            Stmt::Assign { value, .. } => self.expr(value),
            Stmt::SetField { base, value, .. } => {
                self.expr(base);
                self.expr(value);
            }
            Stmt::SetIndex { base, index, value, .. } => {
                self.expr(base);
                self.expr(index);
                self.expr(value);
            }
            Stmt::SlicePush { value, .. } => self.expr(value),
            Stmt::MapSet { key, value, .. } => {
                self.expr(key);
                self.expr(value);
            }
            Stmt::ForSlice { slice, body, .. } => {
                self.expr(slice);
                self.block(body);
            }
            Stmt::ForRange { start, end, body, .. } => {
                self.expr(start);
                self.expr(end);
                self.block(body);
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::Loop { body, .. } => self.block(body),
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.expr(e);
                }
            }
            Stmt::If { cond, then, else_, .. } => {
                self.expr(cond);
                self.block(then);
                if let Some(b) = else_ {
                    self.block(b);
                }
            }
            Stmt::Block(b) => self.block(b),
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }

    fn expr(&mut self, expr: &hir::Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args, .. } => self.call(*callee, args, expr.span),
            ExprKind::CallVirtual { trait_id, method, args } => {
                self.call_virtual(*trait_id, *method, args, expr.span)
            }
            _ => {}
        }

        // Every child, whatever the node: a call can appear anywhere an
        // expression can, including inside another call's arguments.
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Local(_)
            | ExprKind::Nil
            | ExprKind::Yield
            | ExprKind::Error => {}
            ExprKind::Call { args, .. }
            | ExprKind::CallVirtual { args, .. }
            | ExprKind::CallBuiltin { args, .. }
            | ExprKind::CallExtern { args, .. }
            | ExprKind::StrOp { args, .. }
            | ExprKind::ClosureNew { captures: args, .. }
            | ExprKind::StructNew { fields: args, .. }
            | ExprKind::EnumNew { fields: args, .. }
            | ExprKind::TupleNew { elems: args }
            | ExprKind::MapNew { entries: args }
            | ExprKind::SliceNew { elems: args } => {
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::CallClosure { callee, args } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::ToDyn { value, .. }
            | ExprKind::ToStr { value }
            | ExprKind::Cast { value, .. }
            | ExprKind::Await { value }
            | ExprKind::IsNil { value }
            | ExprKind::Wrap { value }
            | ExprKind::Unwrap { value }
            | ExprKind::ErrorNew { message: value } => self.expr(value),
            ExprKind::FieldGet { base, .. }
            | ExprKind::PairValue { base }
            | ExprKind::PairError { base }
            | ExprKind::ErrorMessage { base }
            | ExprKind::MapLen { base }
            | ExprKind::MapKeys { base }
            | ExprKind::MapValues { base }
            | ExprKind::SliceLen { base } => self.expr(base),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond);
                self.expr(then);
                self.expr(else_);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                }
            }
            ExprKind::PairNew { value, error } => {
                self.expr(value);
                self.expr(error);
            }
            ExprKind::MapGet { base, key } => {
                self.expr(base);
                self.expr(key);
            }
            ExprKind::Index { base, index } | ExprKind::SliceGet { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Block(b) => self.block(b),
        }
    }

    /// A virtual call reaches whichever implementation the receiver turns out
    /// to have, so the parameters to check are every implementation's at once:
    /// a parameter counts as written if *any* row of the vtable declares it
    /// `var`. Names for the message come from the first row, which is in
    /// `TypeTag` order and therefore the same on every compilation.
    fn call_virtual(
        &mut self,
        trait_id: hir::TraitId,
        method: u32,
        args: &[hir::Expr],
        call_span: Span,
    ) {
        let Some(vtable) = self.program.vtables.iter().find(|v| v.trait_id == trait_id) else {
            return;
        };
        let targets: Vec<&hir::Function> = vtable
            .entries
            .iter()
            .filter_map(|e| e.methods.get(method as usize))
            .filter_map(|f| self.program.fns.get(f.index()))
            .filter(|f| f.param_count == args.len())
            .collect();
        let Some(first) = targets.first() else {
            return;
        };

        let params: Vec<Param> = first
            .params()
            .iter()
            .enumerate()
            .map(|(i, p)| Param {
                name: p.name.clone(),
                ty: p.ty,
                mutable: targets.iter().any(|t| t.params()[i].mutable),
            })
            .collect();
        self.check_args(&params, args, call_span);
    }

    fn call(&mut self, callee: hir::FnId, args: &[hir::Expr], call_span: Span) {
        let Some(target) = self.program.fns.get(callee.index()) else {
            return;
        };
        // A lifted closure carries its captures as leading parameters, and a
        // call the checker already rejected may have the wrong count. Neither
        // lines up with the arguments written here.
        if target.param_count != args.len() {
            return;
        }
        let params: Vec<Param> = target
            .params()
            .iter()
            .map(|p| Param { name: p.name.clone(), ty: p.ty, mutable: p.mutable })
            .collect();
        self.check_args(&params, args, call_span);
    }

    /// One call: collect the arguments that name something, then compare them
    /// pairwise. Cost is quadratic in the number of reference-typed arguments,
    /// which in practice is two or three.
    fn check_args(&mut self, params: &[Param], args: &[hir::Expr], call_span: Span) {
        let types = &self.program.types;
        let mut named: Vec<(Place, &Param, Span)> = Vec::new();
        for (arg, param) in args.iter().zip(params) {
            if !is_reference(param.ty, types) {
                continue;
            }
            if let Some(place) = place_of(arg, types, self.func) {
                named.push((place, param, arg.span));
            }
        }

        for i in 0..named.len() {
            for j in (i + 1)..named.len() {
                let (a, pa, span_a) = &named[i];
                let (b, pb, span_b) = &named[j];
                if !(pa.mutable || pb.mutable) || !overlaps(a, b) {
                    continue;
                }
                self.report(a, pa, *span_a, b, pb, *span_b, call_span);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &mut self,
        a: &Place,
        pa: &Param,
        span_a: Span,
        b: &Place,
        pb: &Param,
        span_b: Span,
        call_span: Span,
    ) {
        // Which one is the write decides how the sentence reads. When both
        // are, the first is blamed and the labels say so.
        let both = pa.mutable && pb.mutable;
        let (written, other, span_written, span_other) = if pa.mutable {
            (pa, pb, span_a, span_b)
        } else {
            (pb, pa, span_b, span_a)
        };

        let message = if a.text == b.text {
            format!("`{}` is passed twice to this call, and parameter `{}` writes it", a.text, written.name)
        } else if both {
            format!("`{}` and `{}` are the same object, and both are written", a.text, b.text)
        } else {
            format!("`{}` and `{}` are the same object, and parameter `{}` writes it", a.text, b.text, written.name)
        };

        // The prefix case is the one worth spelling out: one argument is a
        // field of the other, so a write through the outer lands inside the
        // inner and the two are not separate things the way they look.
        let primary = if a.text == b.text {
            format!("the same object again, as parameter `{}`", other.name)
        } else if a.path.len() == b.path.len() {
            format!("also names it, as parameter `{}`", other.name)
        } else {
            let ((long, _), (short, short_param)) = if a.path.len() > b.path.len() {
                ((a, pa), (b, pb))
            } else {
                ((b, pb), (a, pa))
            };
            if short_param.mutable {
                format!("`{}` is inside `{}`, which parameter `{}` writes", long.text, short.text, short_param.name)
            } else {
                format!("`{}` is inside `{}`, so parameter `{}` sees every write", long.text, short.text, short_param.name)
            }
        };

        let secondary = if both {
            format!("parameter `{}` writes it here, and so does `{}`", written.name, other.name)
        } else {
            format!("parameter `{}` is `var`, so it has exclusive access here", written.name)
        };

        let note = "a `var` parameter is a reference the callee writes through. \
                    Two names for one object mean each write lands where the other \
                    expects its own value, and neither the compiler nor the reader \
                    can see it. Pass distinct objects, or take one `var` parameter \
                    and return the second result.";

        let diag = Diagnostic::error(codes::E0800, message)
            .with_primary(span_other, primary)
            .with_secondary(span_written, secondary)
            .with_secondary(call_span, "in this call")
            .with_note(note);
        self.diags.push(diag);
    }
}
