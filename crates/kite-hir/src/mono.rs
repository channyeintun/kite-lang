//! Monomorphisation: a generic function becomes one copy per set of type
//! arguments actually used.
//!
//! Kite specialises rather than boxing because the whole point of a type
//! parameter here is that the concrete type *is* known at the call site — code
//! that wanted runtime polymorphism would have written `dyn Trait`, which is
//! already a different and cheaper thing.
//!
//! Specialising also means neither backend ever sees a `Param`. MIR lowering,
//! the bytecode VM and the WebAssembly backend all work on concrete types only,
//! and none of them needs to know that generics exist.
//!
//! This runs on HIR because HIR still has the shape the checker produced.
//! Substituting here is one pass over expression trees, rather than a
//! substitution threaded through every step of lowering.

use crate::{BinOp, Block, EnumId, Expr, ExprKind, FnId, Function, Local, Pattern, Program, Stmt,
            StructId, TraitId, TyId, TyKind, TypeTag, Types, VTable, VTableEntry};
use std::collections::HashMap;

/// A generic function that instantiates itself with a larger type on each call
/// never terminates. The cap is far above any real program and low enough that
/// a runaway stops in well under a second.
///
/// It counts specialisations made, not functions walked: a program of five
/// thousand ordinary functions and one generic one is not a runaway, and used
/// to be treated as one — the walk stopped partway, leaving calls into
/// functions that had moved and generic calls never specialised, and nothing
/// said so.
const MAX_INSTANTIATIONS: usize = 4096;

/// A generic function that asked for copies of itself without end, which
/// monomorphisation cannot finish.
#[derive(Clone, Debug)]
pub struct Unbounded {
    /// The template, by its source name.
    pub template: String,
    pub span: kite_span::Span,
}

/// Specialise every generic function for the argument sets its callers use, and
/// drop the templates.
///
/// Fails when a generic function recurses at an ever larger type — `depth([x],
/// n - 1)` inside `depth<T>` — which has no finite set of copies. The program
/// is left half-rewritten then, and must not be lowered.
pub fn monomorphise(program: &mut Program) -> Result<(), Unbounded> {
    // A trait a `dyn` cannot hold is reached only through bounds, and every
    // call through a bound is a direct call by the end of this pass. Its
    // table would be dead weight at best; at worst a backend would type a
    // dispatcher for a method that mentions `Self`, which has no one type.
    let dispatchable: Vec<TraitId> = program
        .vtables
        .iter()
        .map(|v| v.trait_id)
        .filter(|t| program.types.is_object_safe(*t))
        .collect();
    if program.fns.iter().all(|f| f.generic_count == 0) {
        program.vtables.retain(|v| dispatchable.contains(&v.trait_id));
        return Ok(());
    }
    let Program { types, fns, entry, vtables, externs: _ } = program;

    // Non-generic functions keep their bodies and are renumbered; templates are
    // dropped and replaced by their instantiations.
    let mut moved: HashMap<u32, u32> = HashMap::new();
    let mut out: Vec<Function> = Vec::new();
    for (i, f) in fns.iter().enumerate() {
        if f.generic_count == 0 {
            moved.insert(i as u32, out.len() as u32);
            out.push(f.clone());
        }
    }

    let mut made: HashMap<(u32, Vec<TyId>), u32> = HashMap::new();
    let mut pending: Vec<usize> = (0..out.len()).collect();
    let mut unbounded: Option<Unbounded> = None;
    let mut rows: Vec<(TraitId, VTableEntry)> = Vec::new();

    while let Some(index) = pending.pop() {
        if let Some(u) = unbounded {
            return Err(u);
        }
        // Take the body so the walk does not borrow `out` while `out` grows.
        let mut body = std::mem::take(&mut out[index].body);
        {
            let mut m = Mono {
                fns,
                types,
                moved: &moved,
                made: &mut made,
                out: &mut out,
                pending: &mut pending,
                unbounded: &mut unbounded,
                vtables,
                rows: &mut rows,
            };
            m.block(&mut body);
        }
        out[index].body = body;
    }
    if let Some(u) = unbounded {
        return Err(u);
    }

    if let Some(e) = entry {
        if let Some(new) = moved.get(&e.0) {
            *e = FnId(*new);
        }
    }
    // A row the checker wrote names the declared methods. Those of a generic
    // type's `impl` are templates, and are gone: the type is dispatched to
    // through the rows made above for each specialisation of it that became
    // a `dyn`. Every other row names moved originals.
    vtables.retain(|v| dispatchable.contains(&v.trait_id));
    for v in vtables.iter_mut() {
        v.entries
            .retain(|row| row.methods.iter().all(|m| fns[m.index()].generic_count == 0));
        for row in &mut v.entries {
            for m in &mut row.methods {
                if let Some(new) = moved.get(&m.0) {
                    *m = FnId(*new);
                }
            }
        }
    }
    for (trait_id, row) in rows {
        if let Some(v) = vtables.iter_mut().find(|v| v.trait_id == trait_id) {
            v.entries.push(row);
            v.entries.sort_by_key(|e| e.tag);
        }
    }

    *fns = out;
    Ok(())
}

struct Mono<'a> {
    /// The original functions, templates included.
    fns: &'a [Function],
    types: &'a mut Types,
    moved: &'a HashMap<u32, u32>,
    made: &'a mut HashMap<(u32, Vec<TyId>), u32>,
    out: &'a mut Vec<Function>,
    pending: &'a mut Vec<usize>,
    /// Set by the first specialisation refused; the walk stops there.
    unbounded: &'a mut Option<Unbounded>,
    /// The tables the checker built, over the declared functions.
    vtables: &'a [VTable],
    /// Rows for specialisations of generic types that became a `dyn`, over
    /// the functions made here.
    rows: &'a mut Vec<(TraitId, VTableEntry)>,
}

impl Mono<'_> {
    /// The specialisation for a template and its type arguments, creating it if
    /// this is the first call site to ask.
    fn instantiate(&mut self, template: u32, targs: &[TyId]) -> u32 {
        let key = (template, targs.to_vec());
        if let Some(&existing) = self.made.get(&key) {
            return existing;
        }
        // A runaway shows itself two ways: arguments nesting deeper with each
        // copy, which is polymorphic recursion and is caught within a few
        // dozen levels, or simply too many copies. Either way the template is
        // named and nothing more is made; the index handed back is never
        // lowered, because the caller stops at the error.
        if self.made.len() >= MAX_INSTANTIATIONS || self.types.too_large(targs) {
            if self.unbounded.is_none() {
                let source = &self.fns[template as usize];
                *self.unbounded =
                    Some(Unbounded { template: source.name.clone(), span: source.span });
            }
            return template;
        }
        let index = self.out.len() as u32;
        // Claim the slot before the body is built, so a recursive call to the
        // same instantiation finds it instead of making a second one.
        self.made.insert(key, index);

        let source = &self.fns[template as usize];
        let mut copy = Function {
            name: specialised_name(&source.name, targs, self.types),
            // A specialisation is not exportable: `first<int>` and `first<str>`
            // would both want the name `first`, and a module may not export
            // one name twice.
            is_free: false,
            is_pub: source.is_pub,
            is_async: source.is_async,
            param_count: source.param_count,
            locals: source
                .locals
                .iter()
                .map(|l| Local { ty: subst(l.ty, targs, self.types), ..l.clone() })
                .collect(),
            ret: subst(source.ret, targs, self.types),
            body: source.body.clone(),
            span: source.span,
            // The copy is concrete; there is nothing left to specialise.
            generic_count: 0,
        };
        substitute_block(&mut copy.body, targs, self.types);
        self.out.push(copy);
        self.pending.push(index as usize);
        index
    }

    fn block(&mut self, b: &mut Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        for e in stmt_exprs(s) {
            self.expr(e);
        }
        for b in stmt_blocks(s) {
            self.block(b);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        // A call through a bound, now its receiver's type is known, is a call
        // to that type's method — and becomes one here, before the forms
        // below renumber or specialise it like any other.
        if let ExprKind::CallVirtual { trait_id, method, args, targs } = &mut e.kind {
            if let Some((callee, all)) = self.devirtualise(*trait_id, *method, args, targs) {
                let args = std::mem::take(args);
                e.kind = ExprKind::Call { callee, args, targs: all };
            }
        }
        if let ExprKind::ToDyn { value, trait_id } = &e.kind {
            self.dispatch_row(*trait_id, value.ty);
        }
        // Both forms name a function by index, so both need the same treatment:
        // renumbered when it moved, specialised when it is a template.
        let target = match &mut e.kind {
            ExprKind::Call { callee, targs, .. } => Some((callee, targs)),
            ExprKind::ClosureNew { func, targs, .. } => Some((func, targs)),
            _ => None,
        };
        if let Some((callee, targs)) = target {
            if targs.is_empty() {
                if let Some(new) = self.moved.get(&callee.0) {
                    *callee = FnId(*new);
                }
            } else {
                let args = std::mem::take(targs);
                *callee = FnId(self.instantiate(callee.0, &args));
            }
        }
        for c in expr_children(&mut e.kind) {
            self.expr(c);
        }
        for b in expr_blocks(&mut e.kind) {
            self.block(b);
        }
    }
}

impl Mono<'_> {
    /// The run-time identity of a nominal type, its declaration's, and the
    /// arguments it was specialised with.
    fn identity(&self, ty: TyId) -> Option<(TypeTag, TypeTag, Vec<TyId>)> {
        match *self.types.kind(ty) {
            TyKind::Struct(s) => {
                let (template, args) = self.types.struct_origin_of(s).unwrap_or((s, Vec::new()));
                Some((TypeTag::Struct(s), TypeTag::Struct(template), args))
            }
            TyKind::Enum(x) => {
                let (template, args) = self.types.enum_origin_of(x).unwrap_or((x, Vec::new()));
                Some((TypeTag::Enum(x), TypeTag::Enum(template), args))
            }
            _ => None,
        }
    }

    /// The declared function a trait's method runs for a type's declaration.
    fn declared_method(&self, trait_id: TraitId, template: TypeTag, method: u32) -> Option<FnId> {
        let table = self.vtables.iter().find(|v| v.trait_id == trait_id)?;
        let row = table.entries.iter().find(|r| r.tag == template)?;
        row.methods.get(method as usize).copied()
    }

    /// The direct call a call through a bound becomes, once the receiver's
    /// type is concrete: the implementing function, with the receiver's own
    /// type arguments ahead of the method's.
    fn devirtualise(
        &self,
        trait_id: TraitId,
        method: u32,
        args: &[Expr],
        targs: &[TyId],
    ) -> Option<(FnId, Vec<TyId>)> {
        let receiver = args.first()?;
        let (_, template, own) = self.identity(receiver.ty)?;
        let callee = self.declared_method(trait_id, template, method)?;
        // A method's parameters are its block's, which the receiver's type
        // supplies, then its own, which the call solved.
        let block = self.fns[callee.index()].generic_count.checked_sub(targs.len())?;
        if own.len() < block {
            return None;
        }
        let mut all: Vec<TyId> = own[..block].to_vec();
        all.extend_from_slice(targs);
        Some((callee, all))
    }

    /// Make sure a specialisation of a generic type has a row in a trait's
    /// table once a value of it becomes that trait's `dyn`.
    ///
    /// The checker's rows are per declaration and name the methods as
    /// written — templates, for a generic type — while a value carries the
    /// tag of its specialisation. Rows are made only for what actually
    /// becomes a `dyn`, which is also what the checker proved satisfies the
    /// `impl`'s bounds.
    fn dispatch_row(&mut self, trait_id: TraitId, ty: TyId) {
        let Some((tag, template, args)) = self.identity(ty) else { return };
        if tag == template || self.rows.iter().any(|(t, r)| *t == trait_id && r.tag == tag) {
            return;
        }
        let Some(table) = self.vtables.iter().find(|v| v.trait_id == trait_id) else { return };
        let Some(row) = table.entries.iter().find(|r| r.tag == template) else { return };
        let declared = row.methods.clone();
        let mut methods = Vec::with_capacity(declared.len());
        for m in declared {
            let count = self.fns[m.index()].generic_count;
            let made = if count == 0 {
                self.moved.get(&m.0).copied()
            } else if args.len() >= count {
                Some(self.instantiate(m.0, &args[..count]))
            } else {
                None
            };
            let Some(made) = made else { return };
            methods.push(FnId(made));
        }
        self.rows.push((trait_id, VTableEntry { tag, methods }));
    }
}

/// A readable name for a specialisation, so a MIR or bytecode dump says which
/// one it is looking at.
fn specialised_name(base: &str, targs: &[TyId], types: &Types) -> String {
    let args: Vec<String> = targs.iter().map(|t| types.name(*t)).collect();
    format!("{}<{}>", base, args.join(", "))
}

/// Replace every `Param` with the type argument at its index.
///
/// The arena owns this: a specialisation's own arguments may mention a
/// parameter — `Tree<T>` inside `f<T>` — and rebuilding it needs the table
/// recording what each specialisation was made from.
pub fn subst(ty: TyId, targs: &[TyId], types: &mut Types) -> TyId {
    types.substitute(ty, targs)
}

fn substitute_block(b: &mut Block, targs: &[TyId], types: &mut Types) {
    for s in &mut b.stmts {
        for e in stmt_exprs(s) {
            substitute_expr(e, targs, types);
        }
        for inner in stmt_blocks(s) {
            substitute_block(inner, targs, types);
        }
    }
}

fn substitute_expr(e: &mut Expr, targs: &[TyId], types: &mut Types) {
    e.ty = subst(e.ty, targs, types);
    // A constructor names its definition directly, not only through the
    // expression's type. `Box{value: v}` inside `Box<T>`'s own methods builds a
    // `Box<T>`, and the copy made for `Box<int>` has to build a `Box<int>` — the
    // substituted type is exactly which one.
    match (&mut e.kind, types.kind(e.ty).clone()) {
        (ExprKind::StructNew { struct_id, .. }, TyKind::Struct(s)) => *struct_id = s,
        (ExprKind::EnumNew { enum_id, .. }, TyKind::Enum(x)) => *enum_id = x,
        _ => {}
    }
    match &mut e.kind {
        // A nested generic call's own type arguments may mention this
        // function's parameters — `f<T>` calling `g<[T]>` — so they substitute
        // too, before the call is instantiated.
        ExprKind::Call { targs: inner, .. }
        | ExprKind::ClosureNew { targs: inner, .. }
        | ExprKind::CallVirtual { targs: inner, .. } => {
            for t in inner.iter_mut() {
                *t = subst(*t, targs, types);
            }
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms.iter_mut() {
                substitute_pattern(&mut arm.pattern, targs, types);
            }
        }
        _ => {}
    }
    for c in expr_children(&mut e.kind) {
        substitute_expr(c, targs, types);
    }
    for b in expr_blocks(&mut e.kind) {
        substitute_block(b, targs, types);
    }
    // `Option<Option<T>>` is `Option<T>`, so a `T` wrapped into an optional
    // in a template needs no wrapping in a copy where `T` is already one —
    // and wrapping it anyway boxes a value that is already boxed, which the
    // Wasm backend's types do not admit. Unwrapping it is the same no-op
    // from the other side.
    if let ExprKind::Wrap { value } | ExprKind::Unwrap { value } = &mut e.kind {
        if value.ty == e.ty {
            let inner = std::mem::replace(
                &mut **value,
                Expr { kind: ExprKind::Error, ty: e.ty, span: e.span },
            );
            *e = inner;
        }
    }
}

/// The specialisation of a struct id under a substitution.
fn subst_struct(id: StructId, targs: &[TyId], types: &mut Types) -> StructId {
    let Some((template, args)) = types.struct_origin_of(id) else { return id };
    let args: Vec<TyId> = args.iter().map(|a| subst(*a, targs, types)).collect();
    types.instantiate_struct(template, &args)
}

fn subst_enum(id: EnumId, targs: &[TyId], types: &mut Types) -> EnumId {
    let Some((template, args)) = types.enum_origin_of(id) else { return id };
    let args: Vec<TyId> = args.iter().map(|a| subst(*a, targs, types)).collect();
    types.instantiate_enum(template, &args)
}

fn substitute_pattern(p: &mut Pattern, targs: &[TyId], types: &mut Types) {
    // A pattern names its definition too, and for the same reason.
    match p {
        Pattern::Struct { struct_id, .. } => *struct_id = subst_struct(*struct_id, targs, types),
        Pattern::Variant { enum_id, .. } => *enum_id = subst_enum(*enum_id, targs, types),
        _ => {}
    }
    match p {
        Pattern::Tuple { ty, elems } => {
            *ty = subst(*ty, targs, types);
            for e in elems {
                substitute_pattern(e, targs, types);
            }
        }
        Pattern::Variant { fields, .. } | Pattern::Or(fields) => {
            for f in fields {
                substitute_pattern(f, targs, types);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (_, f) in fields {
                substitute_pattern(f, targs, types);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Structural walks
//
// Written out rather than derived, and with no catch-all: adding a node to the
// HIR fails to compile here, which is the only reliable way to keep a walk in
// step with the tree it walks.
// ---------------------------------------------------------------------------

fn stmt_exprs(s: &mut Stmt) -> Vec<&mut Expr> {
    match s {
        Stmt::Let { init, .. } => init.iter_mut().collect(),
        Stmt::Assign { value, .. } | Stmt::SlicePush { value, .. } => vec![value],
        Stmt::SetField { base, value, .. } => vec![base, value],
        Stmt::SetIndex { base, index, value, .. } => vec![base, index, value],
        Stmt::MapSet { key, value, .. } => vec![key, value],
        Stmt::MapRemove { key, .. } => vec![key],
        Stmt::ForSlice { slice, .. } => vec![slice],
        Stmt::Expr(e) => vec![e],
        Stmt::Return { value, .. } => value.iter_mut().collect(),
        Stmt::If { cond, .. } | Stmt::While { cond, .. } => vec![cond],
        Stmt::ForRange { start, end, .. } => vec![start, end],
        Stmt::Loop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Block(_) => vec![],
    }
}

fn stmt_blocks(s: &mut Stmt) -> Vec<&mut Block> {
    match s {
        Stmt::If { then, else_, .. } => {
            let mut v = vec![then];
            v.extend(else_.iter_mut());
            v
        }
        Stmt::ForSlice { body, .. }
        | Stmt::ForRange { body, .. }
        | Stmt::While { body, .. }
        | Stmt::Loop { body, .. }
        | Stmt::Block(body) => vec![body],
        Stmt::Let { .. }
        | Stmt::Assign { .. }
        | Stmt::SetField { .. }
        | Stmt::SetIndex { .. }
        | Stmt::SlicePush { .. }
        | Stmt::MapSet { .. }
        | Stmt::MapRemove { .. }
        | Stmt::Expr(_)
        | Stmt::Return { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. } => vec![],
    }
}

/// Turn structural equality on a primitive into the primitive's own
/// comparison.
///
/// `==` inside a generic function is checked as `EqValue`, because the operand
/// type is a parameter and nothing is known about it. Once specialisation has
/// replaced the parameter, the type *is* known — and a backend that generates
/// a comparison function per aggregate has nothing to generate for an `int`.
/// The bytecode VM would happily compare two tagged values and never notice
/// the difference, which is exactly the kind of divergence the differential
/// suite exists to catch.
pub fn specialise_equality(program: &mut Program) {
    let mut types = std::mem::take(&mut program.types);
    for f in &mut program.fns {
        let mut block = std::mem::take(&mut f.body);
        specialise_block(&mut block, &mut types);
        f.body = block;
    }
    program.types = types;
}

fn specialise_block(block: &mut Block, types: &mut Types) {
    for stmt in &mut block.stmts {
        for inner in stmt_blocks(stmt) {
            specialise_block(inner, types);
        }
        for expr in stmt_exprs(stmt) {
            specialise_expr(expr, types);
        }
    }
}

fn specialise_expr(expr: &mut Expr, types: &mut Types) {
    if let ExprKind::Binary { op, lhs, .. } = &mut expr.kind {
        if matches!(op, BinOp::EqValue | BinOp::NeValue) {
            let equal = matches!(op, BinOp::EqValue);
            let specific = match types.kind(lhs.ty) {
                TyKind::Int => Some(if equal { BinOp::EqInt } else { BinOp::NeInt }),
                TyKind::Float => Some(if equal { BinOp::EqFloat } else { BinOp::NeFloat }),
                TyKind::Bool => Some(if equal { BinOp::EqBool } else { BinOp::NeBool }),
                TyKind::Str => Some(if equal { BinOp::EqStr } else { BinOp::NeStr }),
                _ => None,
            };
            if let Some(specific) = specific {
                *op = specific;
            }
        }
    }
    for child in expr_children(&mut expr.kind) {
        specialise_expr(child, types);
    }
    for block in expr_blocks(&mut expr.kind) {
        specialise_block(block, types);
    }
}

fn expr_children(k: &mut ExprKind) -> Vec<&mut Expr> {
    match k {
        ExprKind::Call { args, .. }
        | ExprKind::CallVirtual { args, .. }
        | ExprKind::CallBuiltin { args, .. }
        | ExprKind::StructNew { fields: args, .. }
        | ExprKind::EnumNew { fields: args, .. }
        | ExprKind::TupleNew { elems: args }
        | ExprKind::MapNew { entries: args }
        | ExprKind::SliceNew { elems: args }
        | ExprKind::CallExtern { args, .. }
        | ExprKind::StrOp { args, .. } => args.iter_mut().collect(),
        ExprKind::ClosureNew { captures, .. } => captures.iter_mut().collect(),
        ExprKind::CallClosure { callee, args } => {
            let mut v = vec![&mut **callee];
            v.extend(args.iter_mut());
            v
        }
        ExprKind::ToDyn { value, .. }
        | ExprKind::Cast { value, .. }
        | ExprKind::ToStr { value }
        | ExprKind::Unary { operand: value, .. }
        | ExprKind::IsNil { value }
        | ExprKind::Wrap { value }
        | ExprKind::Await { value }
        | ExprKind::Unwrap { value } => vec![value],
        ExprKind::FieldGet { base, .. }
        | ExprKind::PairValue { base }
        | ExprKind::PairError { base }
        | ExprKind::ErrorMessage { base }
        | ExprKind::ErrorCause { base }
        | ExprKind::ErrorTag { base }
        | ExprKind::ErrorAs { base, .. }
        | ExprKind::MapLen { base }
        | ExprKind::MapKeys { base }
        | ExprKind::MapValues { base }
        | ExprKind::SliceLen { base } => vec![base],
        ExprKind::ErrorNew { message, value, tag, cause } => vec![message, value, tag, cause],
        ExprKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::PairNew { value, error } => vec![value, error],
        ExprKind::MapGet { base, key } => vec![base, key],
        ExprKind::Index { base, index } | ExprKind::SliceGet { base, index } => vec![base, index],
        ExprKind::SliceRange { base, start, end } => vec![base, start, end],
        ExprKind::If { cond, then, else_ } => vec![cond, then, else_],
        ExprKind::Match { scrutinee, arms } => {
            let mut v = vec![&mut **scrutinee];
            for a in arms.iter_mut() {
                v.extend(a.guard.iter_mut());
                v.push(&mut a.body);
            }
            v
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Local(_)
        | ExprKind::Nil
        | ExprKind::Yield
        | ExprKind::Block(_)
        | ExprKind::Error => vec![],
    }
}

fn expr_blocks(k: &mut ExprKind) -> Vec<&mut Block> {
    match k {
        ExprKind::Block(b) => vec![b],
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Local renumbering
//
// Lambda lifting moves a closure body into a function of its own, where the
// locals it uses are numbered differently. The walks above already know the
// shape of the tree, so the renumber reuses them rather than repeating it.
// ---------------------------------------------------------------------------

/// Rewrite every local reference in a block through `map`.
///
/// A local the map does not mention is left alone. That never happens for a
/// lifted body — every local it can reach is either a capture, a parameter, or
/// one of its own — but leaving it alone is the behaviour that fails visibly
/// rather than silently pointing at the wrong slot.
pub fn renumber_locals(b: &mut Block, map: &HashMap<u32, u32>) {
    for s in &mut b.stmts {
        renumber_stmt(s, map);
    }
}

fn remap(id: &mut crate::LocalId, map: &HashMap<u32, u32>) {
    if let Some(&new) = map.get(&id.0) {
        *id = crate::LocalId(new);
    }
}

fn renumber_stmt(s: &mut Stmt, map: &HashMap<u32, u32>) {
    match s {
        Stmt::Let { local, .. }
        | Stmt::Assign { local, .. }
        | Stmt::SlicePush { local, .. }
        | Stmt::MapSet { local, .. }
        | Stmt::MapRemove { local, .. }
        | Stmt::ForSlice { var: local, .. }
        | Stmt::ForRange { var: local, .. } => remap(local, map),
        Stmt::SetField { .. }
        | Stmt::SetIndex { .. }
        | Stmt::Expr(_)
        | Stmt::Return { .. }
        | Stmt::If { .. }
        | Stmt::While { .. }
        | Stmt::Loop { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Block(_) => {}
    }
    for e in stmt_exprs(s) {
        renumber_expr(e, map);
    }
    for b in stmt_blocks(s) {
        renumber_locals(b, map);
    }
}

fn renumber_expr(e: &mut Expr, map: &HashMap<u32, u32>) {
    match &mut e.kind {
        ExprKind::Local(id) => remap(id, map),
        ExprKind::Match { arms, .. } => {
            for arm in arms.iter_mut() {
                renumber_pattern(&mut arm.pattern, map);
            }
        }
        _ => {}
    }
    for c in expr_children(&mut e.kind) {
        renumber_expr(c, map);
    }
    for b in expr_blocks(&mut e.kind) {
        renumber_locals(b, map);
    }
}

fn renumber_pattern(p: &mut Pattern, map: &HashMap<u32, u32>) {
    match p {
        Pattern::Binding { local, .. } => remap(local, map),
        Pattern::Variant { fields, .. } | Pattern::Or(fields) | Pattern::Tuple { elems: fields, .. } => {
            for f in fields {
                renumber_pattern(f, map);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (_, f) in fields {
                renumber_pattern(f, map);
            }
        }
        Pattern::Wildcard
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Str(_)
        | Pattern::Bool(_)
        | Pattern::IntRange { .. }
        | Pattern::Nil => {}
    }
}

// ---------------------------------------------------------------------------
// Reachability
// ---------------------------------------------------------------------------

/// Drop functions nothing can reach, and renumber what is left.
///
/// The prelude is compiled into every program, so without this a `hello world`
/// would carry every list helper and every numeric helper it never mentions.
/// Reachability is exact rather than heuristic: a call names its target by
/// index, a closure names its lifted body, and a trait object can reach any
/// method in its vtable. There is nothing else that can enter a function.
pub fn prune(program: &mut Program) {
    prune_keeping(program, |_| false);
}

/// The same, keeping every function `keep` answers true for as a way in too.
///
/// For `kitec test`, which calls the program's `test_…` functions by name —
/// including a private one, which nothing else calls and which pruning would
/// otherwise have removed before the runner went looking for it.
pub fn prune_keeping(program: &mut Program, keep: impl Fn(&Function) -> bool) {
    let Program { fns, entry, vtables, .. } = program;

    let mut roots: Vec<u32> = Vec::new();
    if let Some(e) = entry {
        roots.push(e.0);
    }
    // Every `pub` free function the *program itself* declares is a way in, and
    // that is true whether or not it also has a `main`. The backend exports
    // exactly these, so pruning one would mean pruning something the outside
    // world can call — and the outside world here is a real caller: a page
    // looks for `init`, `view` and `update` and drives the program through
    // them.
    //
    // Getting this backwards was a silent failure rather than a loud one. The
    // set of exports used to be *whatever survived pruning*, so a program with
    // both an application and a `main` kept only what `main` reached: an
    // example whose `main` built its own model never exported `init`, the page
    // found no application, and ran `main` instead — drawing one still frame
    // and looking for all the world like a broken animation.
    //
    // A library's functions are qualified — `json.parse`, `prelude.map` — and
    // a dot cannot appear in an identifier, so the unqualified name is exactly
    // "declared here". That is what keeps a `hello world` from carrying the
    // prelude it never mentions.
    for (i, f) in fns.iter().enumerate() {
        if (f.is_pub && f.is_free && !f.name.contains('.')) || keep(f) {
            roots.push(i as u32);
        }
    }
    // A value can become a trait object anywhere, and from there any of its
    // trait's methods can run.
    for v in vtables.iter() {
        for row in &v.entries {
            for m in &row.methods {
                roots.push(m.0);
            }
        }
    }

    let mut reachable = vec![false; fns.len()];
    let mut queue = roots;
    while let Some(i) = queue.pop() {
        let Some(seen) = reachable.get_mut(i as usize) else { continue };
        if *seen {
            continue;
        }
        *seen = true;
        collect_callees(&fns[i as usize].body, &mut queue);
    }

    if reachable.iter().all(|r| *r) {
        return;
    }

    let mut moved: HashMap<u32, u32> = HashMap::new();
    let mut kept: Vec<Function> = Vec::new();
    for (i, f) in fns.drain(..).enumerate() {
        if reachable[i] {
            moved.insert(i as u32, kept.len() as u32);
            kept.push(f);
        }
    }
    for f in &mut kept {
        renumber_calls(&mut f.body, &moved);
    }
    if let Some(e) = entry {
        if let Some(new) = moved.get(&e.0) {
            *e = FnId(*new);
        }
    }
    for v in vtables.iter_mut() {
        for row in &mut v.entries {
            for m in &mut row.methods {
                if let Some(new) = moved.get(&m.0) {
                    *m = FnId(*new);
                }
            }
        }
    }
    *fns = kept;
}

fn collect_callees(b: &Block, out: &mut Vec<u32>) {
    // The walks take `&mut`, and this only reads; cloning a body to reuse them
    // would cost more than the second walk.
    let mut b = b.clone();
    for s in &mut b.stmts {
        for e in stmt_exprs(s) {
            collect_expr_callees(e, out);
        }
        for inner in stmt_blocks(s) {
            collect_callees(inner, out);
        }
    }
}

fn collect_expr_callees(e: &mut Expr, out: &mut Vec<u32>) {
    match &e.kind {
        ExprKind::Call { callee, .. } => out.push(callee.0),
        ExprKind::ClosureNew { func, .. } => out.push(func.0),
        _ => {}
    }
    for c in expr_children(&mut e.kind) {
        collect_expr_callees(c, out);
    }
    for b in expr_blocks(&mut e.kind) {
        collect_callees(b, out);
    }
}

fn renumber_calls(b: &mut Block, map: &HashMap<u32, u32>) {
    for s in &mut b.stmts {
        for e in stmt_exprs(s) {
            renumber_call_expr(e, map);
        }
        for inner in stmt_blocks(s) {
            renumber_calls(inner, map);
        }
    }
}

fn renumber_call_expr(e: &mut Expr, map: &HashMap<u32, u32>) {
    match &mut e.kind {
        ExprKind::Call { callee, .. } => {
            if let Some(new) = map.get(&callee.0) {
                *callee = FnId(*new);
            }
        }
        ExprKind::ClosureNew { func, .. } => {
            if let Some(new) = map.get(&func.0) {
                *func = FnId(*new);
            }
        }
        _ => {}
    }
    for c in expr_children(&mut e.kind) {
        renumber_call_expr(c, map);
    }
    for b in expr_blocks(&mut e.kind) {
        renumber_calls(b, map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FnId, TyId, TyKind};

    fn template(name: &str, generic_count: usize, ret: TyId) -> Function {
        Function {
            name: name.into(),
            generic_count,
            is_free: true,
            is_pub: false,
            is_async: false,
            param_count: 0,
            locals: Vec::new(),
            ret,
            body: Block::default(),
            span: kite_span::Span::new(kite_span::FileId(0), 0, 0),
        }
    }

    fn call(callee: u32, targs: Vec<TyId>) -> Expr {
        Expr {
            kind: ExprKind::Call { callee: FnId(callee), args: Vec::new(), targs },
            ty: TyId::UNIT,
            span: kite_span::Span::new(kite_span::FileId(0), 0, 0),
        }
    }

    /// Two call sites with different type arguments produce two functions; a
    /// third repeating one of them reuses it.
    #[test]
    fn one_copy_per_distinct_argument_set() {
        let mut p = Program::default();
        let param = p.types.param_ty(0, "T");
        p.fns.push(template("id", 1, param));
        let mut main = template("main", 0, TyId::UNIT);
        main.body.stmts = vec![
            Stmt::Expr(call(0, vec![TyId::INT])),
            Stmt::Expr(call(0, vec![TyId::STR])),
            Stmt::Expr(call(0, vec![TyId::INT])),
        ];
        p.fns.push(main);
        p.entry = Some(FnId(1));

        monomorphise(&mut p).unwrap();

        // `main` plus two specialisations; the template itself is gone.
        assert_eq!(p.fns.len(), 3);
        assert_eq!(p.fns[0].name, "main");
        assert_eq!(p.entry, Some(FnId(0)));
        let names: Vec<&str> = p.fns[1..].iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"id<int>"), "got {:?}", names);
        assert!(names.contains(&"id<str>"), "got {:?}", names);

        // Each copy's return type is its own argument, not the parameter.
        for f in &p.fns[1..] {
            assert!(!matches!(p.types.kind(f.ret), TyKind::Param { .. }));
            assert_eq!(f.generic_count, 0);
        }
    }

    /// A program with no generic function is left exactly as it was — including
    /// its function numbering, which nothing else should have to think about.
    #[test]
    fn a_program_without_generics_is_untouched() {
        let mut p = Program::default();
        p.fns.push(template("a", 0, TyId::UNIT));
        p.fns.push(template("b", 0, TyId::INT));
        p.entry = Some(FnId(1));

        monomorphise(&mut p).unwrap();

        assert_eq!(p.fns.len(), 2);
        assert_eq!(p.fns[0].name, "a");
        assert_eq!(p.entry, Some(FnId(1)));
    }

    /// Only what a program can reach survives. The prelude is compiled into
    /// every program, so this is what keeps a `hello world` small.
    #[test]
    fn unreachable_functions_are_dropped() {
        let mut p = Program::default();
        p.fns.push(template("used", 0, TyId::UNIT));
        p.fns.push(template("unused", 0, TyId::UNIT));
        let mut main = template("main", 0, TyId::UNIT);
        main.body.stmts = vec![Stmt::Expr(call(0, Vec::new()))];
        p.fns.push(main);
        p.entry = Some(FnId(2));

        prune(&mut p);

        let names: Vec<&str> = p.fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["used", "main"]);
        assert_eq!(p.entry, Some(FnId(1)));
        // The surviving call was renumbered with everything else.
        let Stmt::Expr(e) = &p.fns[1].body.stmts[0] else { panic!("expected a call") };
        let ExprKind::Call { callee, .. } = &e.kind else { panic!("expected a call") };
        assert_eq!(*callee, FnId(0));
    }

    /// A trait object can reach any method in its vtable, so those are roots
    /// even when nothing calls them by name.
    #[test]
    fn vtable_methods_are_roots() {
        use crate::{TypeTag, VTable, VTableEntry};
        let mut p = Program::default();
        p.fns.push(template("area", 0, TyId::INT));
        p.fns.push(template("main", 0, TyId::UNIT));
        p.entry = Some(FnId(1));
        p.vtables.push(VTable {
            trait_id: crate::TraitId(0),
            entries: vec![VTableEntry {
                tag: TypeTag::Struct(crate::StructId(0)),
                methods: vec![FnId(0)],
            }],
        });

        prune(&mut p);

        assert_eq!(p.fns.len(), 2, "a vtable method is reachable");
        assert_eq!(p.vtables[0].entries[0].methods[0], FnId(0));
    }

    /// Many ordinary functions and one generic one is not a runaway. The budget
    /// once counted every function walked, so past four thousand of them the
    /// walk stopped, leaving calls into functions that had moved.
    #[test]
    fn a_large_program_is_specialised_completely() {
        let mut p = Program::default();
        let param = p.types.param_ty(0, "T");
        p.fns.push(template("id", 1, param));
        for i in 0..5000 {
            let mut f = template(&format!("f{}", i), 0, TyId::UNIT);
            f.body.stmts = vec![Stmt::Expr(call(0, vec![TyId::INT]))];
            p.fns.push(f);
        }
        p.entry = Some(FnId(5000));

        monomorphise(&mut p).unwrap();

        assert_eq!(p.fns.len(), 5001);
        assert_eq!(p.entry, Some(FnId(4999)));
        let copy = p.fns.iter().position(|f| f.name == "id<int>").expect("a copy") as u32;
        for f in &p.fns[..5000] {
            let Stmt::Expr(e) = &f.body.stmts[0] else { panic!("expected a call") };
            let ExprKind::Call { callee, targs, .. } = &e.kind else { panic!("expected a call") };
            assert_eq!(callee.0, copy, "{} still calls the template", f.name);
            assert!(targs.is_empty());
        }
    }

    /// `f<T>` calling `f<[T]>` needs a copy at every depth. That is refused,
    /// by name, rather than cut off partway without a word.
    #[test]
    fn polymorphic_recursion_is_refused() {
        let mut p = Program::default();
        let param = p.types.param_ty(0, "T");
        let slice_of_t = p.types.slice_of(param);
        let mut depth = template("depth", 1, TyId::UNIT);
        depth.body.stmts = vec![Stmt::Expr(call(0, vec![slice_of_t]))];
        p.fns.push(depth);
        let mut main = template("main", 0, TyId::UNIT);
        main.body.stmts = vec![Stmt::Expr(call(0, vec![TyId::INT]))];
        p.fns.push(main);
        p.entry = Some(FnId(1));

        let err = monomorphise(&mut p).expect_err("an unbounded instantiation");
        assert_eq!(err.template, "depth");
    }

    /// Substitution rebuilds composite types around the parameter rather than
    /// replacing only a bare `T`.
    #[test]
    fn substitution_reaches_inside_composites() {
        let mut types = Types::new();
        let t = types.param_ty(0, "T");
        let slice_of_t = types.slice_of(t);
        let opt = types.optional_of(slice_of_t);

        let concrete = subst(opt, &[TyId::STR], &mut types);

        let TyKind::Optional(inner) = *types.kind(concrete) else {
            panic!("expected an optional, got {}", types.name(concrete))
        };
        let TyKind::Slice(elem) = *types.kind(inner) else {
            panic!("expected a slice, got {}", types.name(inner))
        };
        assert_eq!(elem, TyId::STR);
    }
}
