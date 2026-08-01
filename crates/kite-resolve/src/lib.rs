//! Name resolution.
//!
//! Binds every identifier to exactly one definition before type checking runs,
//! so the checker never has to ask "which `x` is this?".
//!
//! Scoping rules from the specification:
//!
//! * Shadowing in a *nested* scope is permitted.
//! * Shadowing in the *same* scope is an error — it is almost always a typo.
//! * Imports are always qualified at the use site, so `config.load` always says
//!   where `load` came from. There is no wildcard import.

use kite_ast::*;
use kite_diag::{codes, DiagBag, Diagnostic};
use kite_span::Span;
use std::collections::HashMap;

/// What a name refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Res {
    /// Index into the enclosing function's local table.
    Local(u32),
    /// Index into [`ResolveMap::fns`].
    Fn(u32),
    Builtin(BuiltinFn),
}

/// Compiler-provided functions. Replaced by the real standard library in
/// Phase 6; until then they are how a Phase 1 program produces output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuiltinFn {
    IoPrint,
}

impl BuiltinFn {
    pub fn from_path(path: &str) -> Option<BuiltinFn> {
        match path {
            "io.print" => Some(BuiltinFn::IoPrint),
            _ => None,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            BuiltinFn::IoPrint => "io.print",
        }
    }

    pub fn arity(self) -> usize {
        match self {
            BuiltinFn::IoPrint => 1,
        }
    }
}

/// A function's identity, known before any body is resolved so calls may go
/// in either direction.
#[derive(Debug)]
pub struct FnSig {
    pub name: String,
    pub param_count: usize,
    pub decl_index: usize,
    pub span: Span,
}

/// A local slot within one function.
#[derive(Debug)]
pub struct LocalInfo {
    pub name: String,
    pub mutable: bool,
    pub span: Span,
    pub synthetic: bool,
}

#[derive(Debug, Default)]
pub struct ResolveMap {
    pub fns: Vec<FnSig>,
    /// Per function, in the same order as `fns`.
    pub locals: Vec<Vec<LocalInfo>>,
    /// Every resolved name, keyed by the span of its use. Spans are unique per
    /// source position, which makes them a serviceable node identity until
    /// Phase 2 introduces real node ids.
    pub uses: HashMap<Span, Res>,
    /// Binding occurrences, keyed by the span of the introduced name.
    pub bindings: HashMap<Span, u32>,
}

impl ResolveMap {
    pub fn lookup_use(&self, span: Span) -> Option<Res> {
        self.uses.get(&span).copied()
    }

    pub fn lookup_binding(&self, span: Span) -> Option<u32> {
        self.bindings.get(&span).copied()
    }

    pub fn fn_by_name(&self, name: &str) -> Option<u32> {
        self.fns.iter().position(|f| f.name == name).map(|i| i as u32)
    }
}

pub fn resolve(file: &SourceFile, diags: &mut DiagBag) -> ResolveMap {
    let mut map = ResolveMap::default();

    // Pass 1: collect signatures, so a call may precede its declaration.
    let mut seen: HashMap<&str, Span> = HashMap::new();
    for (i, item) in file.items.iter().enumerate() {
        let Item::Fn(f) = item else { continue };
        if let Some(&prev) = seen.get(f.name.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    codes::E0112,
                    format!("`{}` is defined more than once", f.name.name),
                )
                .with_primary(f.name.span, "redefined here")
                .with_secondary(prev, "first defined here")
                .with_note("Kite has no function overloading: one name, one signature"),
            );
            continue;
        }
        seen.insert(&f.name.name, f.name.span);
        map.fns.push(FnSig {
            name: f.name.name.clone(),
            param_count: f.params.len(),
            decl_index: i,
            span: f.name.span,
        });
    }

    // Pass 2: resolve each body.
    for sig_index in 0..map.fns.len() {
        let decl_index = map.fns[sig_index].decl_index;
        let Item::Fn(f) = &file.items[decl_index] else {
            unreachable!("signature index points at a function")
        };
        let locals = {
            let mut r = FnResolver {
                map: &mut map,
                diags,
                locals: Vec::new(),
                scopes: vec![HashMap::new()],
                loop_depth: 0,
                labels: Vec::new(),
            };
            r.resolve_fn(f);
            r.locals
        };
        map.locals.push(locals);
    }

    map
}

struct FnResolver<'a> {
    map: &'a mut ResolveMap,
    diags: &'a mut DiagBag,
    locals: Vec<LocalInfo>,
    scopes: Vec<HashMap<String, u32>>,
    loop_depth: u32,
    labels: Vec<String>,
}

impl<'a> FnResolver<'a> {
    fn resolve_fn(&mut self, f: &FnDecl) {
        for p in &f.params {
            self.declare(&p.name, p.is_var, false);
        }
        self.block(&f.body);
    }

    // ---- scopes -----------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &Ident, mutable: bool, synthetic: bool) -> u32 {
        if let Some(&prev_id) = self.scopes.last().unwrap().get(&name.name) {
            let prev = self.locals[prev_id as usize].span;
            self.diags.push(
                Diagnostic::error(
                    codes::E0112,
                    format!("`{}` is already declared in this scope", name.name),
                )
                .with_primary(name.span, "redeclared here")
                .with_secondary(prev, "first declared here")
                .with_note(
                    "shadowing is permitted in a nested scope, but in the same scope it is \
                     almost always a typo",
                ),
            );
        }
        let id = self.locals.len() as u32;
        self.locals.push(LocalInfo {
            name: name.name.clone(),
            mutable,
            span: name.span,
            synthetic,
        });
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.name.clone(), id);
        self.map.bindings.insert(name.span, id);
        id
    }

    fn lookup(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        None
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self, b: &Block) {
        self.push_scope();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.pop_scope();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l) => {
                // The initialiser is resolved *before* the binding is
                // introduced, so `let x = x` refers to an outer `x` rather
                // than to itself.
                if let Some(init) = &l.init {
                    self.expr(init);
                }
                self.binding(&l.binding, false);
            }
            Stmt::Var(v) => {
                self.expr(&v.init);
                self.declare(&v.name, true, false);
            }
            Stmt::Assign(a) => {
                self.expr(&a.value);
                self.assign_target(&a.target);
            }
            Stmt::Return(r) => match &r.value {
                Some(ReturnValue::Single(e)) => self.expr(e),
                Some(ReturnValue::Pair { value, error, .. }) => {
                    self.expr(value);
                    self.expr(error);
                }
                Some(ReturnValue::Fail { error, .. }) => self.expr(error),
                None => {}
            },
            Stmt::Check { expr, .. } | Stmt::Defer { expr, .. } => self.expr(expr),
            Stmt::If(i) => self.if_stmt(i),
            Stmt::For(f) => self.for_stmt(f),
            Stmt::Break { label, span } => self.loop_jump(label.as_ref(), *span, "break"),
            Stmt::Continue { label, span } => self.loop_jump(label.as_ref(), *span, "continue"),
            Stmt::Expr(e) => self.expr(e),
            Stmt::Error(_) => {}
        }
    }

    fn binding(&mut self, b: &Binding, mutable: bool) {
        match b {
            Binding::Name(n) => {
                self.declare(n, mutable, false);
            }
            Binding::Tuple { elems, .. } => {
                for e in elems {
                    if let BindElem::Name(n) = e {
                        self.declare(n, mutable, false);
                    }
                }
            }
        }
    }

    fn if_stmt(&mut self, i: &IfStmt) {
        self.expr(&i.cond);
        self.block(&i.then);
        match i.else_.as_deref() {
            Some(ElseBranch::Block(b)) => self.block(b),
            Some(ElseBranch::If(nested)) => self.if_stmt(nested),
            None => {}
        }
    }

    fn for_stmt(&mut self, f: &ForStmt) {
        if let Some(l) = &f.label {
            self.labels.push(l.name.clone());
        }
        self.loop_depth += 1;
        self.push_scope();

        match &f.header {
            ForHeader::In { binding, iter } => {
                self.expr(iter);
                self.binding(binding, false);
            }
            ForHeader::While(c) => self.expr(c),
            ForHeader::Loop => {}
        }

        // The body shares the header's scope, so the loop variable is visible.
        for s in &f.body.stmts {
            self.stmt(s);
        }

        self.pop_scope();
        self.loop_depth -= 1;
        if f.label.is_some() {
            self.labels.pop();
        }
    }

    fn loop_jump(&mut self, label: Option<&Ident>, span: Span, what: &str) {
        if self.loop_depth == 0 {
            self.diags.push(
                Diagnostic::error(codes::E0115, format!("`{}` outside a loop", what))
                    .with_primary(span, format!("no loop for this `{}` to leave", what)),
            );
            return;
        }
        if let Some(l) = label {
            if !self.labels.contains(&l.name) {
                self.diags.push(
                    Diagnostic::error(codes::E0111, format!("unknown loop label `{}`", l.name))
                        .with_primary(l.span, "no enclosing loop has this label")
                        .with_note(if self.labels.is_empty() {
                            "no enclosing loop is labelled".to_string()
                        } else {
                            format!("labels in scope: {}", self.labels.join(", "))
                        }),
                );
            }
        }
    }

    /// The left of an assignment. Resolution is the same as a read; whether the
    /// binding is mutable is the type checker's business, because that is where
    /// the E0114 message and its `var` suggestion belong.
    fn assign_target(&mut self, e: &Expr) {
        self.expr(e);
    }

    // ---- expressions ------------------------------------------------------

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Str(_)
            | Expr::Char(_)
            | Expr::Bool { .. }
            | Expr::Nil(_)
            | Expr::SelfExpr(_)
            | Expr::Error(_) => {}

            Expr::Path(p) => self.path(p),

            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Field { base, .. } => self.expr(base),
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            Expr::If { cond, then, else_, .. } => {
                self.expr(cond);
                self.block(then);
                match else_.as_ref() {
                    ElseBranch::Block(b) => self.block(b),
                    ElseBranch::If(i) => self.if_stmt(i),
                }
            }
            Expr::Cast { expr, .. } | Expr::Await { expr, .. } => self.expr(expr),
            Expr::Paren { inner, .. } => self.expr(inner),
            Expr::Tuple { elems, .. } | Expr::Slice { elems, .. } => {
                for x in elems {
                    self.expr(x);
                }
            }
            Expr::Closure { params, body, .. } => {
                self.push_scope();
                for p in params {
                    self.declare(&p.name, false, false);
                }
                match body.as_ref() {
                    ClosureBody::Expr(e) => self.expr(e),
                    ClosureBody::Block(b) => self.block(b),
                }
                self.pop_scope();
            }
        }
    }

    fn path(&mut self, p: &Path) {
        let text = p.text();

        // A dotted path may name a builtin, e.g. `io.print`.
        if !p.is_simple() {
            if let Some(b) = BuiltinFn::from_path(&text) {
                self.map.uses.insert(p.span, Res::Builtin(b));
                return;
            }
            self.diags.push(
                Diagnostic::error(codes::E0111, format!("cannot find `{}`", text))
                    .with_primary(p.span, "not found in this scope")
                    .with_note("Phase 1 provides only `io.print`; modules arrive in Phase 6"),
            );
            return;
        }

        let name = &p.last().name;

        if let Some(id) = self.lookup(name) {
            self.map.uses.insert(p.span, Res::Local(id));
            return;
        }
        if let Some(id) = self.map.fn_by_name(name) {
            self.map.uses.insert(p.span, Res::Fn(id));
            return;
        }

        let mut d = Diagnostic::error(codes::E0111, format!("cannot find `{}`", name))
            .with_primary(p.span, "not found in this scope");
        if let Some(sugg) = self.suggest(name) {
            d = d.with_note(format!("a similar name is in scope: `{}`", sugg));
        }
        self.diags.push(d);
    }

    /// Nearest name by edit distance, when it is close enough to be a likely
    /// typo rather than a coincidence.
    fn suggest(&self, name: &str) -> Option<String> {
        let mut best: Option<(usize, &str)> = None;
        let candidates = self
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(|k| k.as_str()))
            .chain(self.map.fns.iter().map(|f| f.name.as_str()));

        for cand in candidates {
            let d = edit_distance(name, cand);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, cand));
            }
        }
        let (dist, cand) = best?;
        let threshold = (name.len() / 3).max(1);
        (dist <= threshold).then(|| cand.to_string())
    }
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

#[cfg(test)]
mod tests;
