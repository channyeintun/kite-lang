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
//!
//! Method calls are *not* resolved here. `x.area()` needs `x`'s type, so it is
//! the checker's business; the resolver only records that `x` is in scope.

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
    /// Index into [`ResolveMap::types`].
    Type(u32),
    /// An enum variant: the type index, then the variant's position.
    Variant(u32, u32),
}

/// Compiler-provided functions. Replaced by the real standard library in
/// Phase 6; until then they are how a program produces output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuiltinFn {
    IoPrint,
    /// `errors.new(message)` — builds an error value. Errors are ordinary
    /// values in Kite, never exceptions.
    ErrorsNew,
    /// `draw.rect(x, y, w, h, colour)` — a filled rectangle in device pixels.
    ///
    /// The drawing boundary is two calls wide on purpose. Everything a layout
    /// produces is rectangles and runs of text, and a boundary that stayed
    /// that narrow can be met by a DOM renderer and a canvas renderer alike —
    /// which is the only way the two can be made to agree.
    DrawRect,
    /// `draw.text(x, y, body, colour)` — a run of text with its baseline-left
    /// origin at `(x, y)`.
    DrawText,
    /// `text.width(body)` — how wide a run of text will be, in the same units
    /// drawing uses.
    ///
    /// Measurement is a host call because only the host knows the font. Two
    /// hosts can legitimately disagree — that is what different fonts *are* —
    /// so a layout is a function of where it runs, and pretending otherwise is
    /// how text ends up clipped.
    TextWidth,
    /// `text.height()` — the line height of the host's font: ascent plus
    /// descent plus leading, which is what a line of text actually occupies.
    TextHeight,
    /// `draw.clip(x, y, w, h)` — confine drawing to a rectangle until
    /// `draw.unclip()`.
    ///
    /// This is the one thing a layout produces that is neither a rectangle nor
    /// a run of text. Clipping cannot be built out of fills: a half-visible
    /// line of text has to be cut by the renderer, and painting a rectangle
    /// over it would erase whatever it was scrolling past.
    DrawClip,
    DrawUnclip,
    /// `task.yield()` — give the scheduler the chance to run something else.
    /// The one suspension primitive: `sleep`, `race` and `timeout` are Kite
    /// written on top of it.
    TaskYield,
    /// `task.wake_at(ms)` — ask not to be polled again before a deadline.
    TaskWakeAt,
    /// `task.park()` — ask not to be polled again until some other task
    /// finishes. What a combinator waiting on "whichever finishes first" needs,
    /// and the only way to wait without spinning.
    TaskPark,
    /// `task.wait_host()` — ask not to be polled again until the host has had
    /// a chance to do something. What a task waiting on a fetch needs: the
    /// answer is not coming from another task, and spinning would keep the
    /// event loop from ever delivering it.
    TaskWaitHost,
    /// `task.finished(t)` / `task.get(t)` — inspect a task without suspending.
    /// A combinator that must not block on a particular task needs these; a
    /// program that simply wants the value writes `await`.
    TaskFinished,
    TaskGet,
    /// `time.now()` — milliseconds since the program started.
    TimeNow,
}

impl BuiltinFn {
    pub fn from_path(path: &str) -> Option<BuiltinFn> {
        match path {
            "io.print" => Some(BuiltinFn::IoPrint),
            "errors.new" => Some(BuiltinFn::ErrorsNew),
            "draw.rect" => Some(BuiltinFn::DrawRect),
            "draw.text" => Some(BuiltinFn::DrawText),
            "text.width" => Some(BuiltinFn::TextWidth),
            "text.height" => Some(BuiltinFn::TextHeight),
            "draw.clip" => Some(BuiltinFn::DrawClip),
            "draw.unclip" => Some(BuiltinFn::DrawUnclip),
            "task.yield" => Some(BuiltinFn::TaskYield),
            "task.wake_at" => Some(BuiltinFn::TaskWakeAt),
            "task.park" => Some(BuiltinFn::TaskPark),
            "task.wait_host" => Some(BuiltinFn::TaskWaitHost),
            "task.finished" => Some(BuiltinFn::TaskFinished),
            "task.get" => Some(BuiltinFn::TaskGet),
            "time.now" => Some(BuiltinFn::TimeNow),
            _ => None,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            BuiltinFn::IoPrint => "io.print",
            BuiltinFn::ErrorsNew => "errors.new",
            BuiltinFn::DrawRect => "draw.rect",
            BuiltinFn::DrawText => "draw.text",
            BuiltinFn::TextWidth => "text.width",
            BuiltinFn::TextHeight => "text.height",
            BuiltinFn::DrawClip => "draw.clip",
            BuiltinFn::DrawUnclip => "draw.unclip",
            BuiltinFn::TaskYield => "task.yield",
            BuiltinFn::TaskWakeAt => "task.wake_at",
            BuiltinFn::TaskPark => "task.park",
            BuiltinFn::TaskWaitHost => "task.wait_host",
            BuiltinFn::TaskFinished => "task.finished",
            BuiltinFn::TaskGet => "task.get",
            BuiltinFn::TimeNow => "time.now",
        }
    }

    pub fn arity(self) -> usize {
        match self {
            BuiltinFn::IoPrint | BuiltinFn::ErrorsNew => 1,
            BuiltinFn::DrawRect | BuiltinFn::DrawText => 5,
            BuiltinFn::TextWidth => 1,
            BuiltinFn::TextHeight => 0,
            BuiltinFn::DrawClip => 4,
            BuiltinFn::DrawUnclip => 0,
            BuiltinFn::TaskYield
            | BuiltinFn::TimeNow
            | BuiltinFn::TaskPark
            | BuiltinFn::TaskWaitHost => 0,
            BuiltinFn::TaskWakeAt | BuiltinFn::TaskFinished | BuiltinFn::TaskGet => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Struct,
    Enum,
    Trait,
    Alias,
}

impl TypeKind {
    pub fn describe(self) -> &'static str {
        match self {
            TypeKind::Struct => "struct",
            TypeKind::Enum => "enum",
            TypeKind::Trait => "trait",
            TypeKind::Alias => "type alias",
        }
    }
}

/// A declared type, known before any body is resolved.
#[derive(Debug)]
pub struct TypeDecl {
    pub name: String,
    pub kind: TypeKind,
    /// Index into `SourceFile::items`.
    pub decl_index: usize,
    pub span: Span,
    pub is_pub: bool,
}

/// Which module each merged item came from, and how the use sites spell them.
///
/// A module is a directory ([spec §13.1]). Every file in it contributes to one
/// namespace, and an importer always writes the module name: `config.load`
/// says where `load` came from, which is why there is no wildcard import.
///
/// Modules are merged into one item list before resolution, with each declared
/// name rewritten to its qualified form — `load` in module `config` is
/// declared as `config.load`. A dot cannot appear in an identifier, so the
/// qualified name is unforgeable *and* is exactly what a user writes. Lookups
/// inside a module try its own prefix first, which is what lets sibling files
/// call each other unqualified.
#[derive(Debug, Default)]
pub struct Modules {
    /// Module name per item index. Empty for the root module, which is the
    /// program's own file plus the prelude.
    pub of_item: Vec<String>,
    /// `use std/json as j` — the spelling at the use site, and the module it
    /// names.
    pub aliases: HashMap<String, String>,
}

impl Modules {
    pub fn of(&self, item: usize) -> &str {
        self.of_item.get(item).map(|s| s.as_str()).unwrap_or("")
    }

    /// Rewrite an alias at the head of a dotted name: `j.decode` becomes
    /// `json.decode` when the file wrote `use std/json as j`.
    pub fn canonical(&self, name: &str) -> String {
        let Some((head, rest)) = name.split_once('.') else {
            return name.to_string();
        };
        match self.aliases.get(head) {
            Some(module) => format!("{}.{}", module, rest),
            None => name.to_string(),
        }
    }
}

/// A function's identity, known before any body is resolved so calls may go in
/// either direction.
#[derive(Debug)]
pub struct FnSig {
    pub name: String,
    pub param_count: usize,
    pub decl_index: usize,
    pub span: Span,
    /// For a method, the type it is declared on and whether it takes `self`.
    pub owner: Option<MethodOwner>,
    pub is_pub: bool,
    /// Declared `extern`: the body is the host's, and the compiler generates a
    /// stub that forwards to it.
    pub is_extern: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MethodOwner {
    /// Index into [`ResolveMap::types`].
    pub type_index: u32,
    /// Index into `SourceFile::items` for the block holding the body — an
    /// `impl`, or the `trait` itself when this is an inherited default.
    pub impl_index: usize,
    /// Position within that block's method list.
    pub method_index: usize,
    pub takes_self: bool,
    /// The trait being implemented, if this is a trait impl.
    pub trait_index: Option<u32>,
    /// True when the body comes from a trait's default method rather than
    /// from the `impl` block. The body is shared; only the receiver differs.
    pub is_default: bool,
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
    pub types: Vec<TypeDecl>,
    /// Which module each item came from.
    pub modules: Modules,
    /// Per function, in the same order as `fns`.
    pub locals: Vec<Vec<LocalInfo>>,
    /// Every resolved name, keyed by the span of its use. Spans are unique per
    /// source position, which makes them a serviceable node identity until a
    /// later phase introduces real node ids.
    pub uses: HashMap<Span, Res>,
    /// Binding occurrences, keyed by the span of the introduced name.
    pub bindings: HashMap<Span, u32>,
    /// Unqualified variant names, so `match shape { Circle(r) => … }` works
    /// without writing `Shape.Circle`. Ambiguous names are removed and must be
    /// qualified.
    variant_index: HashMap<String, (u32, u32)>,
    /// Variant names per enum, so a qualified `Shape.Circle` resolves against
    /// that enum rather than against the unqualified index — which drops any
    /// name two enums share, and would otherwise make the qualified form fail
    /// exactly where it is most needed.
    variants_of: HashMap<u32, HashMap<String, u32>>,
    ambiguous_variants: Vec<String>,
}

impl ResolveMap {
    /// The index of a variant on one enum.
    pub fn variant_of(&self, type_index: u32, name: &str) -> Option<u32> {
        self.variants_of.get(&type_index)?.get(name).copied()
    }

    pub fn lookup_use(&self, span: Span) -> Option<Res> {
        self.uses.get(&span).copied()
    }

    pub fn lookup_binding(&self, span: Span) -> Option<u32> {
        self.bindings.get(&span).copied()
    }

    pub fn fn_by_name(&self, name: &str) -> Option<u32> {
        self.fn_by_name_in("", name)
    }

    pub fn type_by_name(&self, name: &str) -> Option<u32> {
        self.type_by_name_in("", name)
    }

    /// A function visible from inside `module`.
    ///
    /// The module's own declarations win, so a file in `math` calling `abs`
    /// gets `math.abs` rather than a name of that spelling elsewhere. Failing
    /// that the name is looked up as written, which is how the prelude and a
    /// qualified `task.all` both resolve.
    pub fn fn_by_name_in(&self, module: &str, name: &str) -> Option<u32> {
        let name = self.modules.canonical(name);
        self.find_fn(&qualify(module, &name))
            .or_else(|| self.find_fn(&name))
    }

    fn find_fn(&self, name: &str) -> Option<u32> {
        self.fns
            .iter()
            .position(|f| f.owner.is_none() && f.name == name)
            .map(|i| i as u32)
    }

    pub fn type_by_name_in(&self, module: &str, name: &str) -> Option<u32> {
        let name = self.modules.canonical(name);
        self.find_type(&qualify(module, &name))
            .or_else(|| self.find_type(&name))
    }

    fn find_type(&self, name: &str) -> Option<u32> {
        self.types.iter().position(|t| t.name == name).map(|i| i as u32)
    }

    /// The module an item was declared in.
    pub fn module_of_item(&self, decl_index: usize) -> &str {
        self.modules.of(decl_index)
    }

    /// The module a function was declared in.
    pub fn module_of_fn(&self, index: u32) -> &str {
        self.modules.of(self.fns[index as usize].decl_index)
    }

    pub fn type_decl(&self, index: u32) -> &TypeDecl {
        &self.types[index as usize]
    }

    /// Methods declared on a type, in declaration order.
    pub fn methods_of(&self, type_index: u32) -> Vec<u32> {
        self.fns
            .iter()
            .enumerate()
            .filter(|(_, f)| f.owner.is_some_and(|o| o.type_index == type_index))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Whether a type has an `impl` block for a trait. This is what makes a
    /// value usable as a `dyn Trait`.
    pub fn implements(&self, type_index: u32, trait_index: u32) -> bool {
        self.fns.iter().any(|f| {
            f.owner
                .is_some_and(|o| o.type_index == type_index && o.trait_index == Some(trait_index))
        })
    }

    /// Every type implementing a trait, in declaration order. This is the set a
    /// virtual call dispatches over, so its order is the vtable's order.
    pub fn impls_of(&self, trait_index: u32) -> Vec<u32> {
        let mut seen = Vec::new();
        for f in &self.fns {
            if let Some(o) = f.owner {
                if o.trait_index == Some(trait_index) && !seen.contains(&o.type_index) {
                    seen.push(o.type_index);
                }
            }
        }
        seen.sort_unstable();
        seen
    }

    /// The function that runs when a trait's method is called on a concrete
    /// type. A default method resolves to the trait's own body, which is why
    /// `is_default` methods are recorded per implementing type.
    pub fn trait_method(&self, type_index: u32, trait_index: u32, name: &str) -> Option<u32> {
        self.fns
            .iter()
            .position(|f| {
                f.name == name
                    && f.owner.is_some_and(|o| {
                        o.type_index == type_index && o.trait_index == Some(trait_index)
                    })
            })
            .map(|i| i as u32)
    }

    pub fn method_on(&self, type_index: u32, name: &str) -> Option<u32> {
        self.fns
            .iter()
            .position(|f| {
                f.name == name && f.owner.is_some_and(|o| o.type_index == type_index)
            })
            .map(|i| i as u32)
    }
}

/// The qualified form of a name declared in `module`.
fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", module, name)
    }
}

pub fn resolve(file: &SourceFile, diags: &mut DiagBag) -> ResolveMap {
    resolve_modules(file, Modules::default(), diags)
}

/// Resolve a merged item list whose declarations are already qualified by
/// module. The driver does the merging, because which modules a program needs
/// is a question about `use` declarations and the file system, not about names.
pub fn resolve_modules(file: &SourceFile, modules: Modules, diags: &mut DiagBag) -> ResolveMap {
    let mut map = ResolveMap { modules, ..ResolveMap::default() };

    collect_types(file, &mut map, diags);
    index_variants(file, &mut map);
    collect_functions(file, &mut map, diags);
    resolve_bodies(file, &mut map, diags);

    map
}

/// Pass 1: every type name, so declarations may refer to each other in any
/// order and mutual recursion works.
fn collect_types(file: &SourceFile, map: &mut ResolveMap, diags: &mut DiagBag) {
    let mut seen: HashMap<&str, Span> = HashMap::new();

    for (i, item) in file.items.iter().enumerate() {
        let (name, kind, is_pub) = match item {
            Item::Struct(s) => (&s.name, TypeKind::Struct, s.is_pub),
            Item::Enum(e) => (&e.name, TypeKind::Enum, e.is_pub),
            Item::Trait(t) => (&t.name, TypeKind::Trait, t.is_pub),
            Item::TypeAlias(a) => (&a.name, TypeKind::Alias, a.is_pub),
            _ => continue,
        };

        if let Some(&prev) = seen.get(name.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    codes::E0112,
                    format!("`{}` is defined more than once", name.name),
                )
                .with_primary(name.span, "redefined here")
                .with_secondary(prev, "first defined here"),
            );
            continue;
        }
        seen.insert(&name.name, name.span);
        map.types.push(TypeDecl {
            name: name.name.clone(),
            kind,
            decl_index: i,
            span: name.span,
            is_pub,
        });
    }
}

/// Build the unqualified variant index. A name carried by two enums is
/// ambiguous and must be written qualified.
fn index_variants(file: &SourceFile, map: &mut ResolveMap) {
    let mut ambiguous = Vec::new();
    for (type_index, decl) in map.types.iter().enumerate() {
        if decl.kind != TypeKind::Enum {
            continue;
        }
        let Item::Enum(e) = &file.items[decl.decl_index] else {
            continue;
        };
        for (vi, v) in e.variants.iter().enumerate() {
            let key = v.name.name.clone();
            map.variants_of
                .entry(type_index as u32)
                .or_default()
                .insert(key.clone(), vi as u32);
            // A name two enums share is ambiguous and must be written
            // qualified, so the first one in does not win.
            match map.variant_index.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => ambiguous.push(key),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert((type_index as u32, vi as u32));
                }
            }
        }
    }
    for name in &ambiguous {
        map.variant_index.remove(name);
    }
    map.ambiguous_variants = ambiguous;
}

/// Pass 2: free functions and methods.
fn collect_functions(file: &SourceFile, map: &mut ResolveMap, diags: &mut DiagBag) {
    let mut seen: HashMap<&str, Span> = HashMap::new();

    for (i, item) in file.items.iter().enumerate() {
        match item {
            Item::Fn(f) => {
                if let Some(&prev) = seen.get(f.name.name.as_str()) {
                    diags.push(
                        Diagnostic::error(
                            codes::E0112,
                            format!("`{}` is defined more than once", f.name.name),
                        )
                        .with_primary(f.name.span, "redefined here")
                        .with_secondary(prev, "first defined here")
                        .with_note(
                            "Kite has no function overloading: one name, one signature",
                        ),
                    );
                    continue;
                }
                seen.insert(&f.name.name, f.name.span);
                map.fns.push(FnSig {
                    name: f.name.name.clone(),
                    param_count: f.params.len(),
                    decl_index: i,
                    span: f.name.span,
                    owner: None,
                    is_pub: f.is_pub,
                    is_extern: false,
                });
            }

            Item::Extern(e) => {
                if let Some(&prev) = seen.get(e.name.name.as_str()) {
                    diags.push(
                        Diagnostic::error(
                            codes::E0112,
                            format!("`{}` is defined more than once", e.name.name),
                        )
                        .with_primary(e.name.span, "redefined here")
                        .with_secondary(prev, "first defined here"),
                    );
                    continue;
                }
                seen.insert(&e.name.name, e.name.span);
                map.fns.push(FnSig {
                    name: e.name.name.clone(),
                    param_count: e.params.len(),
                    decl_index: i,
                    span: e.name.span,
                    owner: None,
                    is_pub: e.is_pub,
                    is_extern: true,
                });
            }

            Item::Impl(imp) => {
                let module = map.modules.of(i).to_string();
                let target = imp.self_ty.text();
                let _ = &seen;
                let Some(type_index) = map.type_by_name_in(&module, &target) else {
                    diags.push(
                        Diagnostic::error(codes::E0204, format!("unknown type `{}`", target))
                            .with_primary(imp.self_ty.span, "no such type in this module")
                            .with_note("an `impl` block needs a type declared in this module"),
                    );
                    continue;
                };

                let trait_index = match &imp.trait_path {
                    None => None,
                    Some(tp) => match map.type_by_name_in(&module, &tp.text()) {
                        Some(ti) if map.types[ti as usize].kind == TypeKind::Trait => Some(ti),
                        Some(ti) => {
                            diags.push(
                                Diagnostic::error(
                                    codes::E0204,
                                    format!("`{}` is not a trait", tp.name()),
                                )
                                .with_primary(tp.span, format!(
                                    "this is a {}",
                                    map.types[ti as usize].kind.describe()
                                )),
                            );
                            continue;
                        }
                        None => {
                            diags.push(
                                Diagnostic::error(
                                    codes::E0204,
                                    format!("unknown trait `{}`", tp.name()),
                                )
                                .with_primary(tp.span, "no such trait"),
                            );
                            continue;
                        }
                    },
                };

                for (mi, m) in imp.methods.iter().enumerate() {
                    map.fns.push(FnSig {
                        name: m.name.name.clone(),
                        param_count: m.params.len(),
                        decl_index: i,
                        span: m.name.span,
                        owner: Some(MethodOwner {
                            type_index,
                            impl_index: i,
                            method_index: mi,
                            takes_self: m.self_param.is_some(),
                            trait_index,
                            is_default: false,
                        }),
                        is_pub: m.is_pub,
                        is_extern: false,
                    });
                }

                // A trait's default method becomes a method on the
                // implementing type unless the impl overrode it. The body is
                // the trait's; only the receiver differs.
                if let Some(ti) = trait_index {
                    let trait_item = map.types[ti as usize].decl_index;
                    if let Item::Trait(tr) = &file.items[trait_item] {
                        for (mi, m) in tr.methods.iter().enumerate() {
                            if m.body.is_none() {
                                continue;
                            }
                            let overridden =
                                imp.methods.iter().any(|x| x.name.name == m.name.name);
                            if overridden {
                                continue;
                            }
                            map.fns.push(FnSig {
                                name: m.name.name.clone(),
                                param_count: m.params.len(),
                                decl_index: trait_item,
                                span: m.name.span,
                                owner: Some(MethodOwner {
                                    type_index,
                                    impl_index: trait_item,
                                    method_index: mi,
                                    takes_self: m.self_param.is_some(),
                                    trait_index: Some(ti),
                                    is_default: true,
                                }),
                                is_pub: m.is_pub,
                                is_extern: false,
                            });
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

/// Pass 3: resolve every function and method body.
fn resolve_bodies(file: &SourceFile, map: &mut ResolveMap, diags: &mut DiagBag) {
    for sig_index in 0..map.fns.len() {
        let decl_index = map.fns[sig_index].decl_index;
        let owner = map.fns[sig_index].owner;

        // A body is resolved in the module that declares it, so a file in
        // `math` reaching for `abs` finds its own module's before anything
        // else. That is the whole of what a module scope is here.
        let module = map.modules.of(decl_index).to_string();
        let locals = match owner {
            None => match &file.items[decl_index] {
                Item::Fn(f) => {
                    let mut r = FnResolver::new(map, diags, module);
                    r.resolve_fn(&f.params, Some(&f.body), false);
                    r.locals
                }
                // A host function has no body here; its parameters still get
                // slots, because the generated stub forwards them.
                Item::Extern(e) => {
                    let mut r = FnResolver::new(map, diags, module);
                    r.resolve_fn(&e.params, None, false);
                    r.locals
                }
                _ => unreachable!("a free function signature points at a function"),
            },
            Some(o) => {
                let methods = match &file.items[o.impl_index] {
                    Item::Impl(imp) => &imp.methods,
                    Item::Trait(tr) => &tr.methods,
                    _ => unreachable!("a method signature points at an impl or trait"),
                };
                let m = &methods[o.method_index];
                let mut r = FnResolver::new(map, diags, module);
                r.resolve_fn(&m.params, m.body.as_ref(), o.takes_self);
                r.locals
            }
        };
        map.locals.push(locals);
    }
}

struct FnResolver<'a> {
    map: &'a mut ResolveMap,
    diags: &'a mut DiagBag,
    locals: Vec<LocalInfo>,
    scopes: Vec<HashMap<String, u32>>,
    loop_depth: u32,
    labels: Vec<String>,
    /// The module this body was declared in. Its own names win over everything
    /// but locals.
    module: String,
}

impl<'a> FnResolver<'a> {
    fn new(map: &'a mut ResolveMap, diags: &'a mut DiagBag, module: String) -> Self {
        FnResolver {
            map,
            diags,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            loop_depth: 0,
            labels: Vec::new(),
            module,
        }
    }

    /// A type visible from the module being resolved.
    fn find_type(&self, name: &str) -> Option<u32> {
        self.map.type_by_name_in(&self.module, name)
    }

    /// A free function visible from the module being resolved.
    fn find_fn(&self, name: &str) -> Option<u32> {
        self.map.fn_by_name_in(&self.module, name)
    }

    /// Reject reaching into another module for something it did not mark
    /// `pub`. Two levels of visibility, exactly as the specification says.
    fn check_visible(&mut self, res: Res, span: Span, name: &str) {
        let (module, is_pub, decl, what) = match res {
            Res::Fn(i) => {
                let f = &self.map.fns[i as usize];
                (self.map.modules.of(f.decl_index), f.is_pub, f.span, "function")
            }
            Res::Type(i) | Res::Variant(i, _) => {
                let t = &self.map.types[i as usize];
                (self.map.modules.of(t.decl_index), t.is_pub, t.span, t.kind.describe())
            }
            _ => return,
        };
        // The root module holds the program and the prelude, which are in
        // scope everywhere by construction.
        if module.is_empty() || module == self.module || is_pub {
            return;
        }
        let module = module.to_string();
        self.diags.push(
            Diagnostic::error(
                codes::E0401,
                format!("`{}` is private to module `{}`", name, module),
            )
            .with_primary(span, "not visible here")
            .with_secondary(decl, format!("this {} is not marked `pub`", what))
            .with_note(
                "unmarked declarations are visible only within their own module;                  write `pub` to export one",
            ),
        );
    }

    fn resolve_fn(&mut self, params: &[Param], body: Option<&Block>, takes_self: bool) {
        if takes_self {
            // `self` occupies local 0, so a method's parameters start at 1.
            self.locals.push(LocalInfo {
                name: "self".to_string(),
                mutable: false,
                span: params.first().map(|p| p.span).unwrap_or_else(|| {
                    body.map(|b| b.span).unwrap_or(Span::new(kite_span::FileId(0), 0, 0))
                }),
                synthetic: false,
            });
            self.scopes.last_mut().unwrap().insert("self".to_string(), 0);
        }
        for p in params {
            self.declare(&p.name, p.is_var, false);
        }
        if let Some(b) = body {
            self.block(b);
        }
    }

    // ---- scopes -----------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &Ident, mutable: bool, synthetic: bool) -> u32 {
        self.declare_maybe_shadowing(name, mutable, synthetic, false)
    }

    /// `allow_shadow` is set only for the error slot of a tuple binding.
    ///
    /// The specification makes this the one exception to the same-scope
    /// shadowing rule, because it is what lets a function chain several
    /// fallible calls:
    ///
    /// ```kite
    /// let (bytes, err) = fs.read(path)
    /// check err
    /// let (text, err) = str.from_utf8(bytes)
    /// check err
    /// ```
    ///
    /// It is safe because each `err` gets its own local slot, so the taint
    /// analysis still reports the earlier one if it was never checked.
    fn declare_maybe_shadowing(
        &mut self,
        name: &Ident,
        mutable: bool,
        synthetic: bool,
        allow_shadow: bool,
    ) -> u32 {
        if let Some(&prev_id) = self.scopes.last().unwrap().get(&name.name) {
            if allow_shadow {
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
                return id;
            }
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
                // The initialiser is resolved *before* the binding enters
                // scope, so `let x = x` refers to an outer `x`.
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
                self.expr(&a.target);
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
            Stmt::Match(m) => self.match_expr(m),
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
                for (i, e) in elems.iter().enumerate() {
                    if let BindElem::Name(n) = e {
                        // The error slot may shadow an earlier one in the same
                        // scope; see `declare_maybe_shadowing`.
                        let is_error_slot = elems.len() == 2 && i == 1;
                        self.declare_maybe_shadowing(n, mutable, false, is_error_slot);
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

    // ---- match and patterns ----------------------------------------------

    fn match_expr(&mut self, m: &MatchExpr) {
        self.expr(&m.scrutinee);
        for arm in &m.arms {
            // Each arm gets its own scope: pattern bindings are visible in the
            // guard and the body, and nowhere else.
            self.push_scope();
            self.pattern(&arm.pattern);
            if let Some(g) = &arm.guard {
                self.expr(g);
            }
            match &arm.body {
                MatchBody::Expr(e) => self.expr(e),
                MatchBody::Block(b) => {
                    for s in &b.stmts {
                        self.stmt(s);
                    }
                }
            }
            self.pop_scope();
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Wildcard(_) | Pattern::Nil(_) | Pattern::Error(_) => {}
            Pattern::Literal(e) => self.expr(e),
            Pattern::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }

            // A bare name is a binding unless it names a unit variant.
            Pattern::Binding(name) => {
                if let Some(&(ty, vi)) = self.map.variant_index.get(&name.name) {
                    self.map.uses.insert(name.span, Res::Variant(ty, vi));
                } else {
                    self.declare(name, false, false);
                }
            }

            Pattern::Variant { path, args, .. } => {
                self.resolve_pattern_path(path);
                match args {
                    PatternArgs::Positional(ps) => {
                        for x in ps {
                            self.pattern(x);
                        }
                    }
                    PatternArgs::Named(ps) => {
                        for (_, x) in ps {
                            self.pattern(x);
                        }
                    }
                }
            }

            Pattern::Struct { path, fields, .. } => {
                self.resolve_pattern_path(path);
                for f in fields {
                    match &f.pattern {
                        Some(x) => self.pattern(x),
                        // `Point{ x }` binds `x`.
                        None => {
                            self.declare(&f.name, false, false);
                        }
                    }
                }
            }

            Pattern::Tuple { elems, .. } => {
                for x in elems {
                    self.pattern(x);
                }
            }

            // Every alternative must bind the same names; the checker verifies
            // that once it knows the types.
            Pattern::Or { alts, .. } => {
                for x in alts {
                    self.pattern(x);
                }
            }
        }
    }

    /// Resolve the head of a variant or struct pattern.
    fn resolve_pattern_path(&mut self, path: &TypePath) {
        let name = path.name();

        if path.segments.len() >= 2 {
            // `Shape.Circle`, or `ui.Align.Start` — everything but the last
            // segment names the type, which may itself be module-qualified.
            let owner = segments_text(&path.segments[..path.segments.len() - 1]);
            if let Some(ti) = self.find_type(&owner) {
                self.check_visible(Res::Type(ti), path.span, &owner);
                self.map.uses.insert(path.span, Res::Type(ti));
                return;
            }
        }

        if let Some(&(ty, vi)) = self.map.variant_index.get(name) {
            self.map.uses.insert(path.span, Res::Variant(ty, vi));
            return;
        }
        if let Some(ti) = self.find_type(&path.text()) {
            self.check_visible(Res::Type(ti), path.span, name);
            self.map.uses.insert(path.span, Res::Type(ti));
            return;
        }

        let mut d = Diagnostic::error(codes::E0111, format!("cannot find `{}`", name))
            .with_primary(path.span, "no such type or variant");
        if self.map.ambiguous_variants.iter().any(|a| a == name) {
            d = d.with_note(format!(
                "`{}` is a variant of more than one enum; write it qualified, as `Enum.{}`",
                name, name
            ));
        }
        self.diags.push(d);
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

            // Each hole is an ordinary expression and resolves like one.
            Expr::Interpolated { parts, .. } => {
                for part in parts {
                    if let kite_ast::StrPart::Hole(e) = part {
                        self.expr(e);
                    }
                }
            }

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
            // `a.b` is syntactically a field access. It may still be a module
            // path (`io.print`), a qualified variant (`Shape.Circle`), or an
            // associated function (`Rect.square`) — but only when its head is
            // not a local. Locals win, so a field named `print` on a value
            // called `io` still works.
            Expr::Field { base, name, span } => {
                if let Some(res) = self.try_dotted(base, name) {
                    self.map.uses.insert(*span, res);
                    return;
                }
                self.expr(base)
            }
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
            Expr::Map { entries, .. } => {
                for entry in entries {
                    self.expr(&entry.key);
                    self.expr(&entry.value);
                }
            }
            Expr::StructLit(lit) => {
                self.resolve_type_name(&lit.path);
                if let Some(b) = &lit.base {
                    self.expr(b);
                }
                for f in &lit.fields {
                    self.expr(&f.value);
                }
            }
            Expr::Match(m) => self.match_expr(m),
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

    /// Try to read `base.name` as a static path rather than a field access.
    ///
    /// Returns `None` when the head names a local, which always wins — a field
    /// called `print` on a value called `io` still works.
    fn try_dotted(&mut self, base: &Expr, name: &Ident) -> Option<Res> {
        // The base may itself be dotted: `ui.Justify.SpaceBetween` is a module,
        // a type and a variant, and arrives as nested field accesses because
        // the parser does not decide what `a.b` means.
        let head = dotted_text(base)?;
        if self.lookup(head.split('.').next().unwrap_or(&head)).is_some() {
            return None;
        }

        let dotted = format!("{}.{}", head, name.name);
        if let Some(b) = BuiltinFn::from_path(&dotted) {
            return Some(Res::Builtin(b));
        }
        // `task.all` — a function in an imported module. Declared names are
        // qualified when a module is merged, so this is an ordinary lookup.
        if let Some(fi) = self.find_fn(&dotted) {
            self.check_visible(Res::Fn(fi), name.span, &dotted);
            return Some(Res::Fn(fi));
        }
        if let Some(ti) = self.find_type(&dotted) {
            self.check_visible(Res::Type(ti), name.span, &dotted);
            return Some(Res::Type(ti));
        }

        let ti = self.find_type(&head)?;
        self.check_visible(Res::Type(ti), name.span, &head);
        // `Shape.Circle` — looked up on `Shape`, not in the unqualified index,
        // which has dropped every name two enums share.
        if let Some(vi) = self.map.variant_of(ti, &name.name) {
            return Some(Res::Variant(ti, vi));
        }
        // `Rect.square` — an associated function, resolved against the type's
        // method table by the checker.
        Some(Res::Type(ti))
    }

    fn resolve_type_name(&mut self, path: &TypePath) {
        match self.find_type(&path.text()) {
            Some(ti) => {
                self.check_visible(Res::Type(ti), path.span, &path.text());
                self.map.uses.insert(path.span, Res::Type(ti));
            }
            None => {
                let mut d =
                    Diagnostic::error(codes::E0204, format!("unknown type `{}`", path.name()))
                        .with_primary(path.span, "no such type in this module");
                if let Some(near) = self.suggest_type(path.name()) {
                    d = d.with_note(format!("a similar type is in scope: `{}`", near));
                }
                self.diags.push(d);
            }
        }
    }

    fn path(&mut self, p: &Path) {
        let text = p.text();

        if !p.is_simple() {
            // `io.print`
            if let Some(b) = BuiltinFn::from_path(&text) {
                self.map.uses.insert(p.span, Res::Builtin(b));
                return;
            }
            // `task.all` — a function reached through its module.
            if let Some(fi) = self.find_fn(&text) {
                self.check_visible(Res::Fn(fi), p.span, &text);
                self.map.uses.insert(p.span, Res::Fn(fi));
                return;
            }
            // `Shape.Circle` — a qualified variant.
            let owner = segments_text(&p.segments[..p.segments.len() - 1]);
            let leaf = &p.last().name;
            if let Some(ti) = self.find_type(&owner) {
                self.check_visible(Res::Type(ti), p.span, &owner);
                if let Some(vi) = self.map.variant_of(ti, leaf) {
                    self.map.uses.insert(p.span, Res::Variant(ti, vi));
                    return;
                }
                // Otherwise it is an associated function such as `Rect.square`,
                // which the checker resolves against the type's method table.
                self.map.uses.insert(p.span, Res::Type(ti));
                return;
            }
            self.diags.push(
                Diagnostic::error(codes::E0111, format!("cannot find `{}`", text))
                    .with_primary(p.span, "not found in this scope"),
            );
            return;
        }

        let name = &p.last().name;

        if let Some(id) = self.lookup(name) {
            self.map.uses.insert(p.span, Res::Local(id));
            return;
        }
        if let Some(id) = self.find_fn(name) {
            self.map.uses.insert(p.span, Res::Fn(id));
            return;
        }
        if let Some(&(ty, vi)) = self.map.variant_index.get(name) {
            self.map.uses.insert(p.span, Res::Variant(ty, vi));
            return;
        }
        if let Some(ti) = self.find_type(name) {
            self.map.uses.insert(p.span, Res::Type(ti));
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
        let candidates: Vec<&str> = self
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(|k| k.as_str()))
            .chain(self.map.fns.iter().map(|f| f.name.as_str()))
            .chain(self.map.types.iter().map(|t| t.name.as_str()))
            .collect();
        nearest(name, &candidates)
    }

    fn suggest_type(&self, name: &str) -> Option<String> {
        let candidates: Vec<&str> = self.map.types.iter().map(|t| t.name.as_str()).collect();
        nearest(name, &candidates)
    }
}

/// A run of plain names, dotted. `None` for anything else, which is then an
/// ordinary field access.
fn dotted_text(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) => Some(p.text()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", dotted_text(base)?, name.name)),
        _ => None,
    }
}

/// The dotted text of a run of path segments.
fn segments_text(segments: &[Ident]) -> String {
    segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn nearest(name: &str, candidates: &[&str]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for cand in candidates {
        let d = edit_distance(name, cand);
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, cand));
        }
    }
    let (dist, cand) = best?;
    (dist <= (name.len() / 3).max(1)).then(|| cand.to_string())
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
