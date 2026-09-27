//! `@derive(…)` — the bodies the compiler writes.
//!
//! A derive is a **source-to-source** expansion, not a special case anywhere
//! later. The generated text is ordinary Kite: it is lexed, parsed, resolved,
//! checked and lowered like anything a person wrote, so both backends handle
//! it without knowing derivation exists, `kitec --emit hir` shows exactly what
//! ran, and a derived method is not privileged over a hand-written one.
//!
//! That choice costs something and buys something. It costs precision — the
//! expander works from what a field's type is *written* as rather than from
//! what it resolved to, because resolution has not happened yet. It buys the
//! property that matters more: there is no second implementation of anything.
//! A derived `Debug` and a hand-written one are the same kind of function.
//!
//! Four traits derive, and the ones that do not are as deliberate as the ones
//! that do:
//!
//! * **`Debug`** — a rendering for a programmer. Mechanical is *right* here,
//!   which is exactly what separates it from `Display`: `Display` is how a
//!   type wants to be seen by a user, and a machine cannot guess that.
//! * **`Hash`** — one integer, folded from the fields with FNV-1a.
//! * **`Encode`** / **`Decode`** — a value to and from `json.Json`. This is
//!   what the roadmap calls `json.decode<T>`; it is spelled `T.decode(doc)`
//!   because Kite has no turbofish, and the type is the thing you already have.
//!
//! **`Display` does not derive.** A mechanical answer would be wrong more
//! often than right, and a `Password` whose derived form printed its field is
//! the case where being wrong matters.
//!
//! **`Eq` does not derive.** `==` is already structural on every Kite value,
//! on both backends — a derived `Eq` would be a second spelling for what the
//! language does anyway, and a second spelling is a chance for two answers.

use kite_ast::{EnumDecl, Item, StructDecl, Type, TypePath, VariantPayload};
use kite_diag::{codes, DiagBag, Diagnostic};
use kite_span::Span;
use std::collections::{HashMap, HashSet};

/// What the expander produced: one file of Kite, and the module each of its
/// items belongs to.
///
/// The module matters. A generated `impl` is placed in the module of the type
/// it is for, so it reaches that type's private fields exactly as a
/// hand-written `impl` beside the declaration would — and so an unqualified
/// name in the generated body means what it means at the declaration.
pub struct Derived {
    pub source: String,
    pub modules: Vec<String>,
    /// Spellings the generated code uses that its module never wrote, keyed
    /// as [`crate::modules::Loader::aliases`] is: `(module, spelling)` to the
    /// module it names. The driver adds them before resolution.
    ///
    /// A derived `Encode` is written against `std/json`, and it used to be
    /// written as `json.…` — which resolved only in a module that happened to
    /// import `std/json` under exactly that name. `use std/json as j`, or no
    /// import at all in the module doing the deriving, and the generated code
    /// named a trait that was not there.
    pub aliases: Vec<((String, String), String)>,
}

/// The spelling derived code uses for `std/json` in a module that did not
/// write one. It is an ordinary identifier, because generated code is ordinary
/// Kite; it is one nobody writes, because a module that did would find it
/// taken.
const JSON_SPELLING: &str = "__json";

/// The traits the compiler can write a body for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Derivable {
    Debug,
    Hash,
    Encode,
    Decode,
}

impl Derivable {
    fn parse(name: &str) -> Option<Derivable> {
        Some(match name {
            "Debug" => Derivable::Debug,
            "Hash" => Derivable::Hash,
            "Encode" => Derivable::Encode,
            "Decode" => Derivable::Decode,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Derivable::Debug => "Debug",
            Derivable::Hash => "Hash",
            Derivable::Encode => "Encode",
            Derivable::Decode => "Decode",
        }
    }

    /// How the trait is written where an `impl` names it. `Encode` lives in
    /// `std/json`, because it mentions `Json` and a trait cannot be declared
    /// somewhere that does not know the type it returns.
    fn path(self) -> &'static str {
        match self {
            Derivable::Debug => "Debug",
            Derivable::Hash => "Hash",
            Derivable::Encode => "json.Encode",
            // `Decode` produces the type itself, which a trait method cannot
            // say without `Self` in return position. It is an associated
            // function on the type instead — `User.decode(doc)` — which is
            // what a caller writes anyway.
            Derivable::Decode => "",
        }
    }

    /// The method a field of this type must have for the walk to recurse.
    fn method(self) -> &'static str {
        match self {
            Derivable::Debug => "debug",
            Derivable::Hash => "hash",
            Derivable::Encode => "encode",
            Derivable::Decode => "decode",
        }
    }
}

/// One declared type, as the expander needs to see it.
struct Decl<'a> {
    /// Qualified — `models.User` — which is what the item list holds after
    /// modules were merged.
    qualified: String,
    /// What a person writes inside that module.
    bare: String,
    module: String,
    kind: Shape<'a>,
    derives: Vec<(Derivable, Span)>,
    generic: bool,
}

/// What a program already wrote by hand for its types, keyed by each type's
/// qualified name.
#[derive(Default)]
struct ByHand {
    /// Traits implemented in an `impl … for`, by their path with any alias at
    /// its head rewritten to the module it names: `j.Encode` is `json.Encode`.
    traits: HashMap<String, Vec<String>>,
    /// Methods and associated functions in an inherent `impl`, with where
    /// each was written.
    methods: HashMap<String, Vec<(String, Span)>>,
}

impl ByHand {
    /// Whether `ty` already has what deriving `trait_` would write — the
    /// trait, or an inherent function of the same name. The second is how
    /// `Decode` is implemented at all, and an inherent `debug` beside a
    /// derived `impl Debug` is the method a call reaches, so either way a
    /// derived body would be a second one that silently loses.
    fn covers(&self, ty: &str, trait_: Derivable) -> Option<Option<Span>> {
        let path = trait_.path();
        if !path.is_empty()
            && self.traits.get(ty).is_some_and(|t| t.iter().any(|t| t == path))
        {
            return Some(None);
        }
        self.methods
            .get(ty)
            .and_then(|m| m.iter().find(|(name, _)| name == trait_.method()))
            .map(|(_, at)| Some(*at))
    }
}

/// The name a type written as `written` in `module` was declared under.
///
/// A dotted name has a spelling at its head, which is the module's own —
/// `m.Point` under `use models as m` is `models.Point` — and an undotted one
/// is the module's own declaration, which in the entry file is unqualified.
fn declared_name(
    aliases: &HashMap<(String, String), String>,
    module: &str,
    written: &str,
) -> String {
    match written.split_once('.') {
        Some((head, rest)) => match aliases.get(&(module.to_string(), head.to_string())) {
            Some(target) => format!("{}.{}", target, rest),
            None => written.to_string(),
        },
        None if module.is_empty() => written.to_string(),
        None => format!("{}.{}", module, written),
    }
}

enum Shape<'a> {
    Struct(&'a StructDecl),
    Enum(&'a EnumDecl),
}

/// Expand every `@derive` in a merged item list.
///
/// Returns `None` when nothing derives anything, which is the common case and
/// is worth not paying for: a program with no derives gets no extra file, no
/// extra parse, and nothing in its source map.
pub fn expand(
    items: &[Item],
    item_modules: &[String],
    aliases: &HashMap<(String, String), String>,
    diags: &mut DiagBag,
) -> Option<Derived> {
    let mut decls: Vec<Decl> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let module = item_modules.get(i).cloned().unwrap_or_default();
        let (name, derives, kind, generic) = match item {
            Item::Struct(s) => (&s.name, &s.derives, Shape::Struct(s), !s.generics.is_empty()),
            Item::Enum(e) => (&e.name, &e.derives, Shape::Enum(e), !e.generics.is_empty()),
            _ => continue,
        };
        let bare = name.name.rsplit('.').next().unwrap_or(&name.name).to_string();
        let mut wanted = Vec::new();
        for d in derives {
            match Derivable::parse(&d.name) {
                Some(t) if wanted.iter().any(|(w, _)| *w == t) => diags.push(
                    Diagnostic::error(codes::E0701, format!("`{}` is derived twice", d.name))
                        .with_primary(d.span, "already asked for"),
                ),
                Some(t) => wanted.push((t, d.span)),
                None => diags.push(
                    Diagnostic::error(codes::E0701, format!("nothing derives `{}`", d.name))
                        .with_primary(d.span, "not a derivable trait")
                        .with_note("the compiler writes Debug, Hash, Encode and Decode")
                        .with_note(match d.name.as_str() {
                            "Display" => {
                                "`Display` is how a type wants to be seen by a person, and a \
                                 mechanical answer would be wrong more often than right — write \
                                 `impl Display` and say what it should look like"
                            }
                            "Eq" => {
                                "`==` is already structural on every Kite value, so there is \
                                 nothing for an `Eq` derive to add"
                            }
                            _ => "write the implementation by hand",
                        }),
                ),
            }
        }
        if wanted.is_empty() {
            continue;
        }
        decls.push(Decl {
            qualified: name.name.clone(),
            bare,
            module,
            kind,
            derives: wanted,
            generic,
        });
    }
    if decls.is_empty() {
        return None;
    }

    // What is already written by hand, for every type rather than only the
    // ones deriving something: a field whose type implements the trait by
    // hand is as walkable as one that derives it.
    //
    // A hand-written `impl Debug for X` beside a `@derive(Debug)` would be two
    // bodies for one method, and the error for that points at generated code.
    // It is better caught here, where the derive can be named.
    let mut by_hand = ByHand::default();
    for (i, item) in items.iter().enumerate() {
        let Item::Impl(imp) = item else { continue };
        let module = item_modules.get(i).map(String::as_str).unwrap_or_default();
        // An `impl` written unqualified means the type of *its own* module.
        // Two modules may each declare a `User`, and taking a hand-written
        // `impl` in one as covering the other's would let a derive recurse
        // into a method that is not there.
        let target = declared_name(aliases, module, &imp.self_ty.text());
        match &imp.trait_path {
            Some(tr) => {
                // Only the head of a trait's path is a spelling to rewrite:
                // `Debug` is the prelude's wherever it is written.
                let text = tr.text();
                let trait_name = if text.contains('.') {
                    declared_name(aliases, module, &text)
                } else {
                    text
                };
                by_hand.traits.entry(target).or_default().push(trait_name);
            }
            None => {
                let methods = by_hand.methods.entry(target).or_default();
                for m in &imp.methods {
                    methods.push((m.name.name.clone(), m.name.span));
                }
            }
        }
    }

    let known: HashMap<String, usize> =
        decls.iter().enumerate().map(|(i, d)| (d.qualified.clone(), i)).collect();
    // Every type in the program, so a field can be recognised as a type that
    // exists but does not derive — which is a much better diagnostic than one
    // about a missing method.
    let mut all_types: Vec<(String, String)> = Vec::new();
    // And every alias, so `id: Id` under `type Id = int` walks as the `int`
    // it is rather than stopping at a name that is not a struct or an enum.
    let mut type_aliases: HashMap<String, (String, &Type)> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let module = item_modules.get(i).cloned().unwrap_or_default();
        match item {
            Item::Struct(s) => all_types.push((s.name.name.clone(), module)),
            Item::Enum(e) => all_types.push((e.name.name.clone(), module)),
            Item::TypeAlias(a) if a.generics.is_empty() => {
                type_aliases.insert(a.name.name.clone(), (module, &a.ty));
            }
            _ => {}
        }
    }

    // How each module deriving `Encode` or `Decode` spells `std/json` in
    // what is generated for it: by a spelling of the compiler's own, never by
    // one the module wrote. The module's own was used when it had one, and a
    // module's spelling is whatever it chose — `use std/json as doc` made
    // every `doc.field(doc, …)` in a derived `decode` mean the parameter
    // `doc`, and the derive failed with errors inside `<derive>`. Every name
    // the generated code binds is chosen to miss the module's spellings too
    // ([`Writer::avoid`]), but a spelling of the compiler's own cannot meet
    // one of the module's by construction.
    let mut json_spellings: HashMap<String, String> = HashMap::new();
    let mut added: Vec<((String, String), String)> = Vec::new();
    for decl in &decls {
        let needs_json =
            decl.derives.iter().any(|(t, _)| matches!(t, Derivable::Encode | Derivable::Decode));
        if !needs_json || json_spellings.contains_key(&decl.module) {
            continue;
        }
        let mut n = 0;
        let spelling = loop {
            let candidate = match n {
                0 => JSON_SPELLING.to_string(),
                n => format!("{}{}", JSON_SPELLING, n),
            };
            if !aliases.contains_key(&(decl.module.clone(), candidate.clone())) {
                added.push(((decl.module.clone(), candidate.clone()), "json".to_string()));
                break candidate;
            }
            n += 1;
        };
        json_spellings.insert(decl.module.clone(), spelling);
    }

    let mut source = String::from(
        "// Generated by `@derive`. This file is written by the compiler, parsed\n\
         // like any other, and is what actually runs — there is no second path\n\
         // through the checker for a derived body.\n",
    );
    let mut modules = Vec::new();
    for index in 0..decls.len() {
        for (trait_, at) in decls[index].derives.clone() {
            let module = decls[index].module.clone();
            let json = json_spellings.get(&module).cloned().unwrap_or_else(|| "json".to_string());
            let avoid = names_in(&module, aliases, &all_types, &json);
            let mut w = Writer {
                decls: &decls,
                known: &known,
                all_types: &all_types,
                type_aliases: &type_aliases,
                aliases,
                by_hand: &by_hand,
                json,
                lookup: module.clone(),
                module,
                followed: 0,
                trait_,
                next: 0,
                avoid,
                diags,
                failed: false,
            };
            let text = w.item(&decls[index], at);
            if w.failed {
                continue;
            }
            source.push('\n');
            source.push_str(&text);
            modules.push(decls[index].module.clone());
        }
    }
    if modules.is_empty() {
        return None;
    }
    Some(Derived { source, modules, aliases: added })
}

/// Every name a module's generated code may need to mean what it means in
/// the module: its spellings for other modules, its own types, and its
/// spelling of `std/json`.
///
/// A local the generated code binds shadows whatever the module meant by that
/// name, and the generated code is full of locals — `doc`, `out`, `field_1`,
/// `src_2`. With `use shapes as doc`, a derived `decode` asked the parameter
/// `doc` for `doc.Circle.decode`, and the module's program failed to compile
/// with errors in a file nobody wrote. So no local takes any of these.
fn names_in(
    module: &str,
    aliases: &HashMap<(String, String), String>,
    all_types: &[(String, String)],
    json: &str,
) -> HashSet<String> {
    let mut names: HashSet<String> = aliases
        .keys()
        .filter(|(m, _)| m == module)
        .map(|(_, spelling)| spelling.clone())
        .collect();
    for (qualified, owner) in all_types {
        if owner == module {
            names.insert(qualified.rsplit('.').next().unwrap_or(qualified).to_string());
        }
    }
    names.insert(json.to_string());
    names
}

/// `mod.Type<A>` as written.
fn path_text(p: &TypePath) -> String {
    let base = p.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(".");
    if p.args.is_empty() {
        return base;
    }
    format!(
        "{}<{}>",
        base,
        p.args.iter().map(render_type).collect::<Vec<_>>().join(", ")
    )
}

/// A type as Kite source, from the parse rather than from the file — the
/// generated text has to say the same thing the declaration did, and the
/// declaration's own spelling is what resolves inside its module.
fn render_type(t: &Type) -> String {
    match t {
        Type::Path(p) => path_text(p),
        Type::Optional { inner, .. } => format!("Option<{}>", render_type(inner)),
        Type::Slice { elem, .. } => format!("[{}]", render_type(elem)),
        Type::Map { key, value, .. } => format!("{{{}: {}}}", render_type(key), render_type(value)),
        Type::Tuple { elems, .. } => {
            format!("({})", elems.iter().map(render_type).collect::<Vec<_>>().join(", "))
        }
        Type::Fn { params, ret, .. } => {
            let ps = params.iter().map(render_type).collect::<Vec<_>>().join(", ");
            match ret {
                Some(r) => format!("fn({}) -> {}", ps, render_type(r)),
                None => format!("fn({})", ps),
            }
        }
        Type::Dyn { path, .. } => format!("dyn {}", path_text(path)),
        Type::Error(_) => "?".to_string(),
    }
}

struct Writer<'a, 'd> {
    decls: &'a [Decl<'a>],
    known: &'a HashMap<String, usize>,
    all_types: &'a [(String, String)],
    /// Every non-generic `type` alias, by its qualified name, with the module
    /// it was declared in and what it stands for.
    type_aliases: &'a HashMap<String, (String, &'a Type)>,
    /// Every module's spellings, which is what turns a field's type as written
    /// into the declaration it names.
    aliases: &'a HashMap<(String, String), String>,
    by_hand: &'a ByHand,
    /// How this module's generated code spells `std/json`.
    json: String,
    /// The module whose spellings the type being walked was written with.
    /// The deriving type's own, except inside an alias declared somewhere
    /// else, whose target is written in *that* module's spellings.
    lookup: String,
    /// The module the generated code is placed in.
    module: String,
    /// How many aliases deep the walk is.
    followed: usize,
    trait_: Derivable,
    next: usize,
    /// Names the generated code must not bind: see [`names_in`].
    avoid: HashSet<String>,
    diags: &'d mut DiagBag,
    /// Set when a field could not be walked. The item is dropped rather than
    /// emitted half-written, so one bad field is one diagnostic and not a
    /// cascade of parse errors in generated text.
    failed: bool,
}

/// Statements accumulate as lines at a known indent; an expression is a string.
type Lines = Vec<String>;

impl<'a> Writer<'a, '_> {
    fn temp(&mut self, stem: &str) -> String {
        loop {
            self.next += 1;
            let name = format!("{}_{}", stem, self.next);
            if !self.avoid.contains(&name) {
                return name;
            }
        }
    }

    /// One of the generated code's fixed local names — `doc`, `out`, `h` —
    /// or, where the module already means something by it, the same with
    /// underscores until it does not.
    fn local(&self, name: &str) -> String {
        let mut name = name.to_string();
        while self.avoid.contains(&name) {
            name.push('_');
        }
        name
    }

    fn cannot(&mut self, at: Span, what: &str, why: &str) -> String {
        self.failed = true;
        self.diags.push(
            Diagnostic::error(
                codes::E0702,
                format!("`@derive({})` cannot write {}", self.trait_.name(), what),
            )
            .with_primary(at, "this is the field it stopped at")
            .with_note(why.to_string())
            .with_note("write the implementation by hand — a derive is a convenience, not the only way in"),
        );
        "\"\"".to_string()
    }

    fn item(&mut self, decl: &Decl, at: Span) -> String {
        if decl.generic {
            self.failed = true;
            self.diags.push(
                Diagnostic::error(
                    codes::E0702,
                    format!("`@derive({})` on a generic type is not written", self.trait_.name()),
                )
                .with_primary(at, "asked for here")
                .with_note(format!(
                    "`{}` has type parameters, and a derived body would need a bound on every \
                     one of them",
                    decl.bare
                ))
                // An `impl` is for every instantiation at once (E0208 refuses
                // one for a single one), so the way in is a bound.
                .with_note(format!(
                    "write the implementation by hand, for every `{}` at once and with a bound: \
                     `impl<T: {}> {} for {}<T>`",
                    decl.bare,
                    self.trait_.name(),
                    self.trait_.name(),
                    decl.bare
                )),
            );
            return String::new();
        }
        // Deriving what is already written by hand is an error rather than a
        // silent replacement (§10.4). It was only caught for a trait `impl`,
        // so `Decode` — an inherent function, with no trait to implement —
        // was never caught, and neither was an inherent `debug` beside a
        // derived `impl Debug`, which is the one a call reaches.
        if let Some(written) = self.by_hand.covers(&decl.qualified, self.trait_) {
            self.failed = true;
            let mut d = match written {
                None => Diagnostic::error(
                    codes::E0701,
                    format!("`{}` already implements `{}`", decl.bare, self.trait_.name()),
                )
                .with_primary(at, "derived here")
                .with_note("a derive writes the same method the hand-written `impl` does"),
                Some(method) => Diagnostic::error(
                    codes::E0701,
                    format!("`{}` already has a `{}`", decl.bare, self.trait_.method()),
                )
                .with_primary(at, "derived here")
                .with_secondary(method, "written by hand here")
                .with_note(format!(
                    "`@derive({})` writes `{}` too, and two of one name is one too many",
                    self.trait_.name(),
                    self.trait_.method()
                )),
            };
            d = d.with_note("remove one of the two");
            self.diags.push(d);
            return String::new();
        }

        match self.trait_ {
            Derivable::Debug => self.debug_item(decl),
            Derivable::Hash => self.hash_item(decl),
            Derivable::Encode => self.encode_item(decl),
            Derivable::Decode => self.decode_item(decl),
        }
    }

    // ---- Debug ------------------------------------------------------------

    fn debug_item(&mut self, decl: &Decl) -> String {
        let mut body: Lines = Vec::new();
        match &decl.kind {
            Shape::Struct(s) => {
                let mut parts = vec![quote(&format!("{}{{", decl.bare))];
                for (i, f) in s.fields.iter().enumerate() {
                    let bound = self.temp("field");
                    body.push(format!("    let {} = self.{}", bound, f.name.name));
                    let value = self.debug_of(&f.ty, &bound, 1, &mut body);
                    let lead = if i == 0 { " " } else { ", " };
                    parts.push(quote(&format!("{}{}: ", lead, f.name.name)));
                    parts.push(value);
                }
                parts.push(quote(" }"));
                body.push(format!("    return {}", parts.join(" + ")));
            }
            Shape::Enum(e) => {
                let out = self.local("out");
                body.push(format!("    var {} = \"\"", out));
                body.push("    match self {".to_string());
                for v in &e.variants {
                    let (head, binds) = self.variant_pattern(v);
                    body.push(format!("        {} => {{", head));
                    let mut parts = vec![quote(&v.name.name)];
                    if !binds.is_empty() {
                        parts.push(quote("("));
                        for (i, (bind, ty, label)) in binds.iter().enumerate() {
                            if i > 0 {
                                parts.push(quote(", "));
                            }
                            if let Some(label) = label {
                                parts.push(quote(&format!("{}: ", label)));
                            }
                            let mut inner: Lines = Vec::new();
                            let value = self.debug_of(ty, bind, 3, &mut inner);
                            body.extend(inner);
                            parts.push(value);
                        }
                        parts.push(quote(")"));
                    }
                    body.push(format!("            {} = {}", out, parts.join(" + ")));
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
                body.push(format!("    return {}", out));
            }
        }
        wrap_impl(&decl.bare, Some("Debug"), "fn debug(self) -> str", &body)
    }

    /// A value of `ty`, held in `expr`, rendered for a programmer.
    fn debug_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
        if let Some((module, target)) = self.through_alias(ty) {
            return self.in_alias(module, ty, |w| w.debug_of(target, expr, depth, out));
        }
        let pad = "    ".repeat(depth);
        match ty {
            Type::Path(p) => match path_text(p).as_str() {
                "int" | "float" | "bool" => format!("\"\\({})\"", expr),
                "str" => format!("debug_str({})", expr),
                other => self.recurse(other, expr, ty),
            },
            Type::Optional { inner, .. } => {
                let held = self.temp("some");
                let text = self.temp("text");
                out.push(format!("{}let {} = {}", pad, held, expr));
                out.push(format!("{}var {} = \"nil\"", pad, text));
                out.push(format!("{}if {} != nil {{", pad, held));
                let value = self.debug_of(inner, &held, depth + 1, out);
                out.push(format!("{}    {} = {}", pad, text, value));
                out.push(format!("{}}}", pad));
                text
            }
            Type::Slice { elem, .. } => {
                let text = self.temp("list");
                let first = self.temp("first");
                let item = self.temp("item");
                out.push(format!("{}var {} = \"[\"", pad, text));
                out.push(format!("{}var {} = true", pad, first));
                out.push(format!("{}for {} in {} {{", pad, item, expr));
                out.push(format!("{}    if !{} {{", pad, first));
                out.push(format!("{}        {} = {} + \", \"", pad, text, text));
                out.push(format!("{}    }}", pad));
                out.push(format!("{}    {} = false", pad, first));
                let value = self.debug_of(elem, &item, depth + 1, out);
                out.push(format!("{}    {} = {} + {}", pad, text, text, value));
                out.push(format!("{}}}", pad));
                out.push(format!("{}{} = {} + \"]\"", pad, text, text));
                text
            }
            Type::Map { key, value, .. } => {
                let text = self.temp("map");
                let first = self.temp("first");
                let k = self.temp("key");
                let v = self.temp("value");
                out.push(format!("{}var {} = \"{{\"", pad, text));
                out.push(format!("{}var {} = true", pad, first));
                out.push(format!("{}for ({}, {}) in {} {{", pad, k, v, expr));
                out.push(format!("{}    if !{} {{", pad, first));
                out.push(format!("{}        {} = {} + \", \"", pad, text, text));
                out.push(format!("{}    }}", pad));
                out.push(format!("{}    {} = false", pad, first));
                let kt = self.debug_of(key, &k, depth + 1, out);
                let vt = self.debug_of(value, &v, depth + 1, out);
                out.push(format!("{}    {} = {} + {} + \": \" + {}", pad, text, text, kt, vt));
                out.push(format!("{}}}", pad));
                out.push(format!("{}{} = {} + \"}}\"", pad, text, text));
                text
            }
            Type::Tuple { elems, .. } => {
                let mut parts = vec![quote("(")];
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        parts.push(quote(", "));
                    }
                    let value = self.debug_of(e, &format!("{}.{}", expr, i), depth, out);
                    parts.push(value);
                }
                parts.push(quote(")"));
                let text = self.temp("tuple");
                out.push(format!("{}let {} = {}", pad, text, parts.join(" + ")));
                text
            }
            Type::Fn { .. } => self.cannot(
                ty.span(),
                "a function",
                "a function value has nothing to show but its type",
            ),
            Type::Dyn { .. } => self.cannot(
                ty.span(),
                "a trait object",
                "a `dyn Trait` hides the concrete type, which is the thing a derived body walks",
            ),
            Type::Error(span) => {
                self.failed = true;
                let _ = span;
                "\"\"".to_string()
            }
        }
    }

    // ---- Hash -------------------------------------------------------------

    fn hash_item(&mut self, decl: &Decl) -> String {
        let mut body: Lines = Vec::new();
        let h = self.local("h");
        body.push(format!("    var {} = hash_seed()", h));
        match &decl.kind {
            Shape::Struct(s) => {
                for f in &s.fields {
                    let bound = self.temp("field");
                    body.push(format!("    let {} = self.{}", bound, f.name.name));
                    let value = self.hash_of(&f.ty, &bound, 1, &mut body);
                    body.push(format!("    {h} = hash_combine({h}, {})", value));
                }
            }
            Shape::Enum(e) => {
                body.push("    match self {".to_string());
                for (index, v) in e.variants.iter().enumerate() {
                    let (head, binds) = self.variant_pattern(v);
                    body.push(format!("        {} => {{", head));
                    // The variant's position, so two variants with the same
                    // payload do not hash alike.
                    body.push(format!("            {h} = hash_combine({h}, {})", index));
                    for (bind, ty, _) in &binds {
                        let mut inner: Lines = Vec::new();
                        let value = self.hash_of(ty, bind, 3, &mut inner);
                        body.extend(inner);
                        body.push(format!("            {h} = hash_combine({h}, {})", value));
                    }
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
            }
        }
        body.push(format!("    return {}", h));
        wrap_impl(&decl.bare, Some("Hash"), "fn hash(self) -> int", &body)
    }

    fn hash_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
        if let Some((module, target)) = self.through_alias(ty) {
            return self.in_alias(module, ty, |w| w.hash_of(target, expr, depth, out));
        }
        let pad = "    ".repeat(depth);
        match ty {
            Type::Path(p) => match path_text(p).as_str() {
                "int" => format!("hash_int({})", expr),
                // Through the rendered text, because that rendering is shared
                // with `io.print` and interpolation and is therefore the one
                // thing about a float both backends already agree on.
                "float" => format!("hash_float({})", expr),
                "bool" => format!("hash_bool({})", expr),
                "str" => format!("hash_str({})", expr),
                other => self.recurse(other, expr, ty),
            },
            Type::Optional { inner, .. } => {
                let held = self.temp("some");
                let acc = self.temp("h");
                out.push(format!("{}let {} = {}", pad, held, expr));
                out.push(format!("{}var {} = 0", pad, acc));
                out.push(format!("{}if {} != nil {{", pad, held));
                let value = self.hash_of(inner, &held, depth + 1, out);
                out.push(format!("{}    {} = hash_combine(1, {})", pad, acc, value));
                out.push(format!("{}}}", pad));
                acc
            }
            Type::Slice { elem, .. } => {
                let acc = self.temp("h");
                let item = self.temp("item");
                out.push(format!("{}var {} = hash_seed()", pad, acc));
                out.push(format!("{}for {} in {} {{", pad, item, expr));
                let value = self.hash_of(elem, &item, depth + 1, out);
                out.push(format!("{}    {} = hash_combine({}, {})", pad, acc, acc, value));
                out.push(format!("{}}}", pad));
                acc
            }
            Type::Map { key, value, .. } => {
                let acc = self.temp("h");
                let k = self.temp("key");
                let v = self.temp("value");
                // Insertion order is part of what a Kite map *is*, and two maps
                // that differ in it are not `==`, so folding in order is right
                // rather than merely convenient.
                out.push(format!("{}var {} = hash_seed()", pad, acc));
                out.push(format!("{}for ({}, {}) in {} {{", pad, k, v, expr));
                let kh = self.hash_of(key, &k, depth + 1, out);
                let vh = self.hash_of(value, &v, depth + 1, out);
                out.push(format!("{}    {} = hash_combine({}, {})", pad, acc, acc, kh));
                out.push(format!("{}    {} = hash_combine({}, {})", pad, acc, acc, vh));
                out.push(format!("{}}}", pad));
                acc
            }
            Type::Tuple { elems, .. } => {
                let acc = self.temp("h");
                out.push(format!("{}var {} = hash_seed()", pad, acc));
                for (i, e) in elems.iter().enumerate() {
                    let value = self.hash_of(e, &format!("{}.{}", expr, i), depth, out);
                    out.push(format!("{}{} = hash_combine({}, {})", pad, acc, acc, value));
                }
                acc
            }
            Type::Fn { .. } => self.cannot(
                ty.span(),
                "a function",
                "two closures that behave alike are still two different values",
            ),
            Type::Dyn { .. } => self.cannot(
                ty.span(),
                "a trait object",
                "a `dyn Trait` hides the concrete type, which is the thing a derived body walks",
            ),
            Type::Error(_) => {
                self.failed = true;
                "0".to_string()
            }
        }
    }

    // ---- Encode -----------------------------------------------------------

    fn encode_item(&mut self, decl: &Decl) -> String {
        let j = self.json.clone();
        let mut body: Lines = Vec::new();
        match &decl.kind {
            Shape::Struct(s) => {
                let fields = self.local("fields");
                body.push(format!("    var {fields}: {{ str: {j}.Json }} = {{ }}"));
                for f in &s.fields {
                    let bound = self.temp("field");
                    body.push(format!("    let {} = self.{}", bound, f.name.name));
                    let value = self.encode_of(&f.ty, &bound, 1, &mut body);
                    body.push(format!("    {fields}[{}] = {}", quote(&f.name.name), value));
                }
                body.push(format!("    return {j}.Json.Object({fields})"));
            }
            Shape::Enum(e) => {
                // Externally tagged: a unit variant is its own name as text,
                // and a payload is a one-key object. It round-trips, it reads,
                // and it is what every other language's JSON does — which
                // matters more here than elegance, because the other end of
                // this is not written in Kite.
                let out = self.local("out");
                let payload = self.local("payload");
                let wrapper = self.local("wrapper");
                body.push(format!("    var {out} = {j}.Json.Null"));
                body.push("    match self {".to_string());
                for v in &e.variants {
                    let (head, binds) = self.variant_pattern(v);
                    body.push(format!("        {} => {{", head));
                    if binds.is_empty() {
                        body.push(format!(
                            "            {out} = {j}.Json.Text({})",
                            quote(&v.name.name)
                        ));
                    } else if binds.iter().all(|(_, _, label)| label.is_some()) {
                        body.push(format!("            var {payload}: {{ str: {j}.Json }} = {{ }}"));
                        for (bind, ty, label) in &binds {
                            let mut inner: Lines = Vec::new();
                            let value = self.encode_of(ty, bind, 3, &mut inner);
                            body.extend(inner);
                            body.push(format!(
                                "            {payload}[{}] = {}",
                                quote(label.as_deref().unwrap_or("")),
                                value
                            ));
                        }
                        body.push(format!("            var {wrapper}: {{ str: {j}.Json }} = {{ }}"));
                        body.push(format!(
                            "            {wrapper}[{}] = {j}.Json.Object({payload})",
                            quote(&v.name.name)
                        ));
                        body.push(format!("            {out} = {j}.Json.Object({wrapper})"));
                    } else {
                        body.push(format!("            var {payload}: [{j}.Json] = []"));
                        for (bind, ty, _) in &binds {
                            let mut inner: Lines = Vec::new();
                            let value = self.encode_of(ty, bind, 3, &mut inner);
                            body.extend(inner);
                            body.push(format!("            {payload}.push({})", value));
                        }
                        body.push(format!("            var {wrapper}: {{ str: {j}.Json }} = {{ }}"));
                        body.push(format!(
                            "            {wrapper}[{}] = {j}.Json.Array({payload})",
                            quote(&v.name.name)
                        ));
                        body.push(format!("            {out} = {j}.Json.Object({wrapper})"));
                    }
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
                body.push(format!("    return {out}"));
            }
        }
        wrap_impl(
            &decl.bare,
            Some(&format!("{j}.Encode")),
            &format!("fn encode(self) -> {j}.Json"),
            &body,
        )
    }

    fn encode_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
        if let Some((module, target)) = self.through_alias(ty) {
            return self.in_alias(module, ty, |w| w.encode_of(target, expr, depth, out));
        }
        let j = self.json.clone();
        let pad = "    ".repeat(depth);
        match ty {
            Type::Path(p) => match path_text(p).as_str() {
                // JSON has one numeric type, and `json.stringify` writes a
                // whole number without a point, so an `int` survives the round
                // trip as an `int`.
                "int" => format!("{j}.Json.Number({} as float)", expr),
                "float" => format!("{j}.Json.Number({})", expr),
                "bool" => format!("{j}.Json.Bool({})", expr),
                "str" => format!("{j}.Json.Text({})", expr),
                other => self.recurse(other, expr, ty),
            },
            Type::Optional { inner, .. } => {
                let held = self.temp("some");
                let node = self.temp("node");
                out.push(format!("{}let {} = {}", pad, held, expr));
                out.push(format!("{}var {} = {j}.Json.Null", pad, node));
                out.push(format!("{}if {} != nil {{", pad, held));
                let value = self.encode_of(inner, &held, depth + 1, out);
                out.push(format!("{}    {} = {}", pad, node, value));
                out.push(format!("{}}}", pad));
                node
            }
            Type::Slice { elem, .. } => {
                let list = self.temp("items");
                let item = self.temp("item");
                out.push(format!("{}var {}: [{j}.Json] = []", pad, list));
                out.push(format!("{}for {} in {} {{", pad, item, expr));
                let value = self.encode_of(elem, &item, depth + 1, out);
                out.push(format!("{}    {}.push({})", pad, list, value));
                out.push(format!("{}}}", pad));
                format!("{j}.Json.Array({})", list)
            }
            Type::Map { key, value, .. } => {
                if !is_str(key) {
                    return self.cannot(
                        ty.span(),
                        "a map with a key that is not `str`",
                        "a JSON object's keys are strings, and inventing a spelling for anything \
                         else would be a convention the other end has to know",
                    );
                }
                let object = self.temp("object");
                let k = self.temp("key");
                let v = self.temp("value");
                out.push(format!("{}var {}: {{ str: {j}.Json }} = {{ }}", pad, object));
                out.push(format!("{}for ({}, {}) in {} {{", pad, k, v, expr));
                let encoded = self.encode_of(value, &v, depth + 1, out);
                out.push(format!("{}    {}[{}] = {}", pad, object, k, encoded));
                out.push(format!("{}}}", pad));
                format!("{j}.Json.Object({})", object)
            }
            Type::Tuple { .. } => self.cannot(
                ty.span(),
                "a tuple",
                "a tuple has no field names, so what it should become in JSON is a choice — use \
                 a struct, which has made that choice",
            ),
            Type::Fn { .. } => {
                self.cannot(ty.span(), "a function", "a function value is not data")
            }
            Type::Dyn { .. } => self.cannot(
                ty.span(),
                "a trait object",
                "a `dyn Trait` hides the concrete type, which is the thing a derived body walks",
            ),
            Type::Error(_) => {
                self.failed = true;
                format!("{j}.Json.Null")
            }
        }
    }

    // ---- Decode -----------------------------------------------------------

    fn decode_item(&mut self, decl: &Decl) -> String {
        let j = self.json.clone();
        let doc = self.local("doc");
        let mut body: Lines = Vec::new();
        match &decl.kind {
            Shape::Struct(s) => {
                let mut values = Vec::new();
                for f in &s.fields {
                    let where_ = format!("{}.{}", decl.bare, f.name.name);
                    let value = self.decode_of(
                        &f.ty,
                        &format!("{j}.field({doc}, {})", quote(&f.name.name)),
                        &where_,
                        1,
                        &mut body,
                    );
                    values.push(format!("{}: {}", f.name.name, value));
                }
                body.push(format!(
                    "    return {}{{ {} }}, nil",
                    decl.bare,
                    values.join(", ")
                ));
            }
            Shape::Enum(e) => {
                // A unit variant arrives as its own name.
                let tag = self.temp("tag");
                body.push(format!("    let {} = {j}.text({doc})", tag));
                body.push(format!("    if {} != nil {{", tag));
                for v in &e.variants {
                    if v.payload.is_empty() {
                        body.push(format!("        if {} == {} {{", tag, quote(&v.name.name)));
                        body.push(format!("            return {}.{}, nil", decl.bare, v.name.name));
                        body.push("        }".to_string());
                    }
                }
                body.push(format!(
                    "        return _, errors.new(\"{}: no variant named \\({})\")",
                    decl.bare, tag
                ));
                body.push("    }".to_string());
                for v in &e.variants {
                    if v.payload.is_empty() {
                        continue;
                    }
                    let held = self.temp("payload");
                    body.push(format!(
                        "    let {} = {j}.field({doc}, {})",
                        held,
                        quote(&v.name.name)
                    ));
                    body.push(format!("    if {} != nil {{", held));
                    let args = match &v.payload {
                        VariantPayload::Named(fields) => {
                            let mut args = Vec::new();
                            for f in fields {
                                let where_ = format!("{}.{}", v.name.name, f.name.name);
                                let value = self.decode_of(
                                    &f.ty,
                                    &format!("{j}.field({}, {})", held, quote(&f.name.name)),
                                    &where_,
                                    2,
                                    &mut body,
                                );
                                args.push(format!("{}: {}", f.name.name, value));
                            }
                            args
                        }
                        VariantPayload::Positional(tys) => {
                            let mut args = Vec::new();
                            for (i, t) in tys.iter().enumerate() {
                                let where_ = format!("{}({})", v.name.name, i);
                                let value = self.decode_of(
                                    t,
                                    &format!("{j}.at({}, {})", held, i),
                                    &where_,
                                    2,
                                    &mut body,
                                );
                                args.push(value);
                            }
                            args
                        }
                        VariantPayload::Unit => Vec::new(),
                    };
                    body.push(format!(
                        "        return {}.{}({}), nil",
                        decl.bare,
                        v.name.name,
                        args.join(", ")
                    ));
                    body.push("    }".to_string());
                }
                body.push(format!(
                    "    return _, errors.new(\"{}: expected a variant name or a one-key object\")",
                    decl.bare
                ));
            }
        }
        // `pub`, because it is inherent: a trait's methods are as visible as
        // the trait, but an associated function is private to its module
        // unless it says otherwise, and decoding is for the type's users.
        wrap_impl(
            &decl.bare,
            None,
            &format!("pub fn decode({doc}: {j}.Json) -> ({}, error)", decl.bare),
            &body,
        )
    }

    /// Read one value out of `source`, which is an `Option<json.Json>`.
    ///
    /// Every failure returns rather than defaulting. A decoder that filled in
    /// a zero for a missing field would be the same mistake the error design
    /// exists to prevent: a value that looks usable and is not.
    fn decode_of(
        &mut self,
        ty: &Type,
        source: &str,
        where_: &str,
        depth: usize,
        out: &mut Lines,
    ) -> String {
        if let Some((module, target)) = self.through_alias(ty) {
            return self.in_alias(module, ty, |w| w.decode_of(target, source, where_, depth, out));
        }
        let j = self.json.clone();
        let pad = "    ".repeat(depth);
        // Whatever came in, the rest of this works on an `Option<json.Json>`.
        // A document node arrives three ways — from an accessor, from a loop
        // over an array, from inside a narrowing `if` — and only the first is
        // already optional. Normalising once here is what lets the walk be
        // written once rather than three times.
        let source = &{
            let normalised = self.temp("src");
            out.push(format!(
                "{}let {}: Option<{j}.Json> = {}",
                pad, normalised, source
            ));
            normalised
        };
        match ty {
            Type::Path(p) => {
                let (reader, what) = match path_text(p).as_str() {
                    "int" => (format!("{j}.int_of"), "a whole number"),
                    "float" => (format!("{j}.number_of"), "a number"),
                    "bool" => (format!("{j}.bool_of"), "a boolean"),
                    "str" => (format!("{j}.text"), "a string"),
                    other => {
                        let held = self.temp("node");
                        out.push(format!("{}let {} = {}", pad, held, source));
                        out.push(format!("{}if {} == nil {{", pad, held));
                        out.push(format!(
                            "{}    return _, errors.new(\"{}: missing\")",
                            pad, where_
                        ));
                        out.push(format!("{}}}", pad));
                        let name = self.recurse_type_name(other, ty);
                        let value = self.temp("value");
                        let err = self.temp("err");
                        out.push(format!(
                            "{}let ({}, {}) = {}.decode({})",
                            pad, value, err, name, held
                        ));
                        out.push(format!("{}check {}", pad, err));
                        return value;
                    }
                };
                let held = self.temp("value");
                out.push(format!("{}let {} = {}({})", pad, held, reader, source));
                out.push(format!("{}if {} == nil {{", pad, held));
                out.push(format!(
                    "{}    return _, errors.new(\"{}: expected {}\")",
                    pad, where_, what
                ));
                out.push(format!("{}}}", pad));
                held
            }
            Type::Optional { inner, .. } => {
                let held = self.temp("maybe");
                // A field that is absent and a field written `null` both mean
                // nil. Telling them apart would be reading a distinction the
                // format does not reliably carry.
                let rendered = self.render(ty);
                out.push(format!("{}var {}: {} = nil", pad, held, rendered));
                out.push(format!("{}if !{j}.is_null({}) {{", pad, source));
                let value = self.decode_of(inner, source, where_, depth + 1, out);
                out.push(format!("{}    {} = {}", pad, held, value));
                out.push(format!("{}}}", pad));
                held
            }
            Type::Slice { elem, .. } => {
                let node = self.temp("node");
                let list = self.temp("items");
                let item = self.temp("item");
                out.push(format!("{}let {} = {}", pad, node, source));
                out.push(format!("{}if {} == nil {{", pad, node));
                out.push(format!("{}    return _, errors.new(\"{}: missing\")", pad, where_));
                out.push(format!("{}}}", pad));
                let rendered = self.render(ty);
                out.push(format!("{}var {}: {} = []", pad, list, rendered));
                out.push(format!("{}for {} in {j}.items({}) {{", pad, item, node));
                let value = self.decode_of(elem, &item, where_, depth + 1, out);
                out.push(format!("{}    {}.push({})", pad, list, value));
                out.push(format!("{}}}", pad));
                list
            }
            Type::Map { key, value, .. } => {
                if !is_str(key) {
                    return self.cannot(
                        ty.span(),
                        "a map with a key that is not `str`",
                        "a JSON object's keys are strings, and inventing a spelling for anything \
                         else would be a convention the other end has to know",
                    );
                }
                let node = self.temp("node");
                let object = self.temp("object");
                let k = self.temp("key");
                let v = self.temp("value");
                out.push(format!("{}let {} = {}", pad, node, source));
                out.push(format!("{}if {} == nil {{", pad, node));
                out.push(format!("{}    return _, errors.new(\"{}: missing\")", pad, where_));
                out.push(format!("{}}}", pad));
                let rendered = self.render(ty);
                out.push(format!("{}var {}: {} = {{ }}", pad, object, rendered));
                out.push(format!("{}for ({}, {}) in {j}.entries({}) {{", pad, k, v, node));
                let decoded = self.decode_of(value, &v, where_, depth + 1, out);
                out.push(format!("{}    {}[{}] = {}", pad, object, k, decoded));
                out.push(format!("{}}}", pad));
                object
            }
            Type::Tuple { .. } => self.cannot(
                ty.span(),
                "a tuple",
                "a tuple has no field names, so what it should become in JSON is a choice — use \
                 a struct, which has made that choice",
            ),
            Type::Fn { .. } => {
                self.cannot(ty.span(), "a function", "a function value is not data")
            }
            Type::Dyn { .. } => self.cannot(
                ty.span(),
                "a trait object",
                "a document says nothing about which concrete type it was",
            ),
            Type::Error(_) => {
                self.failed = true;
                "0".to_string()
            }
        }
    }

    // ---- shared -----------------------------------------------------------

    /// A field whose type is another declared type: recurse through its own
    /// derived method, having first checked that it has one.
    fn recurse(&mut self, name: &str, expr: &str, ty: &Type) -> String {
        let checked = self.recurse_type_name(name, ty);
        if checked.is_empty() {
            return "\"\"".to_string();
        }
        format!("{}.{}()", expr, self.trait_.method())
    }

    /// The name to reach a nested type's derived function by, or empty when
    /// the field cannot be walked — having reported why.
    fn recurse_type_name(&mut self, name: &str, ty: &Type) -> String {
        // The declaration the field names, read through the spellings of the
        // module it was written in: `m.Point` under `use models as m` is
        // `models.Point`, and `lib/geo`'s `geo.Point` is `lib/geo.Point`.
        // Without this only a type spelled exactly as its module's identity
        // could be walked, and every other one read as "no such type".
        let qualified = declared_name(self.aliases, &self.lookup, name);
        let found = self.known.get(&qualified).map(|i| &self.decls[*i]);
        let derives = found.is_some_and(|decl| decl.derives.iter().any(|(t, _)| *t == self.trait_));
        // A type that writes the trait by hand is walked through the same
        // method a derive would have written.
        if derives || self.by_hand.covers(&qualified, self.trait_).is_some() {
            return match self.respell(name) {
                Some(spelled) => spelled,
                None => {
                    self.cannot(
                        ty.span(),
                        &format!("a field of type `{}`", name),
                        "it is reached through a `type` declared in another module, and this \
                         module has no spelling for the module that type names — write the \
                         field's type directly",
                    );
                    String::new()
                }
            };
        }
        if found.is_some() {
            self.cannot(
                ty.span(),
                &format!("a field of type `{}`", name),
                &format!(
                    "`{}` does not derive `{}` — add it there, or write this one by hand",
                    name,
                    self.trait_.name()
                ),
            );
            return String::new();
        }
        // A type that exists but derives nothing, or a name that is not a type
        // at all. Either way the walk stops, and saying which is the whole
        // difference between a usable diagnostic and a puzzle.
        let exists = self.all_types.iter().any(|(n, _)| *n == qualified);
        if exists {
            self.cannot(
                ty.span(),
                &format!("a field of type `{}`", name),
                &format!(
                    "`{}` does not derive `{}` — add `@derive({})` to it, or write this one by \
                     hand",
                    name,
                    self.trait_.name(),
                    self.trait_.name()
                ),
            );
        } else {
            self.cannot(
                ty.span(),
                &format!("a field of type `{}`", name),
                "no such type, or a type parameter — a derived body walks concrete fields",
            );
        }
        String::new()
    }

    /// What `ty` stands for, when it names a `type` alias, and the module that
    /// alias was declared in — whose spellings its target is written with.
    fn through_alias(&self, ty: &Type) -> Option<(String, &'a Type)> {
        let Type::Path(p) = ty else { return None };
        if !p.args.is_empty() {
            return None;
        }
        let declared = declared_name(self.aliases, &self.lookup, &p.text());
        self.type_aliases.get(&declared).map(|(module, target)| (module.clone(), *target))
    }

    /// Walk an alias's target in the module the alias was declared in.
    ///
    /// A field typed `Id` under `type Id = int` used to stop the derive with
    /// "no such type", which was not true: the walk only knew structs and
    /// enums. It is the `int` it stands for, and is walked as one.
    fn in_alias(
        &mut self,
        module: String,
        ty: &Type,
        walk: impl FnOnce(&mut Self) -> String,
    ) -> String {
        // An alias naming itself, however indirectly, is resolution's to
        // report; the walk just has to stop.
        if self.followed >= 64 {
            return self.cannot(ty.span(), "this type", "its `type` aliases never reach a type");
        }
        self.followed += 1;
        let saved = std::mem::replace(&mut self.lookup, module);
        let result = walk(self);
        self.lookup = saved;
        self.followed -= 1;
        result
    }

    /// A type name written in the module being read, spelled so that it
    /// means the same thing in the module the code is generated into — or
    /// `None` when that module has no way to say it.
    ///
    /// The two differ only inside an alias declared elsewhere: `type Pts =
    /// [Point]` in `geo` names `geo.Point` when the field is written `geo.Pts`
    /// somewhere else.
    fn respell(&self, written: &str) -> Option<String> {
        if self.lookup == self.module {
            return Some(written.to_string());
        }
        let declared = declared_name(self.aliases, &self.lookup, written);
        let is_declared = self.all_types.iter().any(|(n, _)| *n == declared)
            || self.type_aliases.contains_key(&declared);
        if !is_declared {
            // A primitive, or one of the compiler's own generic types: the
            // same everywhere.
            return (!written.contains('.')).then(|| written.to_string());
        }
        match declared.rsplit_once('.') {
            None => self.module.is_empty().then_some(declared),
            Some((owner, name)) if owner == self.module => Some(name.to_string()),
            Some((owner, name)) => self
                .aliases
                .iter()
                .filter(|((m, _), target)| *m == self.module && target.as_str() == owner)
                .map(|((_, spelling), _)| format!("{}.{}", spelling, name))
                .min(),
        }
    }

    /// A type as Kite source that means, in the generated code's module, what
    /// it meant where it was written.
    fn render(&mut self, ty: &Type) -> String {
        if self.lookup == self.module {
            return render_type(ty);
        }
        match ty {
            Type::Path(p) => {
                let Some(base) = self.respell(&p.text()) else {
                    return self.cannot(
                        p.span,
                        &format!("a field of type `{}`", p.text()),
                        "it is reached through a `type` declared in another module, and this \
                         module has no spelling for the module that type names — write the \
                         field's type directly",
                    );
                };
                if p.args.is_empty() {
                    return base;
                }
                let args: Vec<String> = p.args.iter().map(|a| self.render(a)).collect();
                format!("{}<{}>", base, args.join(", "))
            }
            Type::Optional { inner, .. } => format!("Option<{}>", self.render(inner)),
            Type::Slice { elem, .. } => format!("[{}]", self.render(elem)),
            Type::Map { key, value, .. } => {
                format!("{{{}: {}}}", self.render(key), self.render(value))
            }
            Type::Tuple { elems, .. } => {
                let parts: Vec<String> = elems.iter().map(|e| self.render(e)).collect();
                format!("({})", parts.join(", "))
            }
            other => render_type(other),
        }
    }

    /// `Variant(a, b)` as a pattern, with what each binding holds.
    #[allow(clippy::type_complexity)]
    fn variant_pattern(
        &mut self,
        v: &kite_ast::VariantDecl,
    ) -> (String, Vec<(String, Type, Option<String>)>) {
        match &v.payload {
            VariantPayload::Unit => (v.name.name.clone(), Vec::new()),
            VariantPayload::Named(fields) => {
                let mut binds = Vec::new();
                let mut parts = Vec::new();
                for f in fields {
                    let bind = self.temp("bound");
                    parts.push(format!("{}: {}", f.name.name, bind));
                    binds.push((bind, clone_type(&f.ty), Some(f.name.name.clone())));
                }
                (format!("{}({})", v.name.name, parts.join(", ")), binds)
            }
            VariantPayload::Positional(tys) => {
                let mut binds = Vec::new();
                let mut parts = Vec::new();
                for t in tys {
                    let bind = self.temp("bound");
                    parts.push(bind.clone());
                    binds.push((bind, clone_type(t), None));
                }
                (format!("{}({})", v.name.name, parts.join(", ")), binds)
            }
        }
    }
}

/// `Type` is not `Clone`, and the writer needs to hold one past the borrow of
/// the declaration. Re-parsing the rendered form would be circular, so it is
/// rebuilt structurally — which is small, and total.
fn clone_type(t: &Type) -> Type {
    match t {
        Type::Path(p) => Type::Path(TypePath {
            segments: p.segments.clone(),
            args: p.args.iter().map(clone_type).collect(),
            span: p.span,
        }),
        Type::Optional { inner, span } => {
            Type::Optional { inner: Box::new(clone_type(inner)), span: *span }
        }
        Type::Slice { elem, span } => {
            Type::Slice { elem: Box::new(clone_type(elem)), span: *span }
        }
        Type::Map { key, value, span } => Type::Map {
            key: Box::new(clone_type(key)),
            value: Box::new(clone_type(value)),
            span: *span,
        },
        Type::Tuple { elems, span } => {
            Type::Tuple { elems: elems.iter().map(clone_type).collect(), span: *span }
        }
        Type::Fn { params, ret, span } => Type::Fn {
            params: params.iter().map(clone_type).collect(),
            ret: ret.as_ref().map(|r| Box::new(clone_type(r))),
            span: *span,
        },
        Type::Dyn { path, span } => Type::Dyn {
            path: TypePath {
                segments: path.segments.clone(),
                args: path.args.iter().map(clone_type).collect(),
                span: path.span,
            },
            span: *span,
        },
        Type::Error(span) => Type::Error(*span),
    }
}

fn is_str(t: &Type) -> bool {
    matches!(t, Type::Path(p) if path_text(p) == "str")
}

/// A Kite string literal holding `text`.
///
/// The backslash matters more here than anywhere else: `\(` opens an
/// interpolation hole, so a field name carrying one would otherwise become
/// whatever that expression evaluates to in the generated body.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn wrap_impl(type_name: &str, trait_path: Option<&str>, signature: &str, body: &[String]) -> String {
    let head = match trait_path {
        Some(t) => format!("impl {} for {} {{", t, type_name),
        None => format!("impl {} {{", type_name),
    };
    let mut out = head;
    out.push('\n');
    out.push_str("    ");
    out.push_str(signature);
    out.push_str(" {\n");
    for line in body {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("    }\n}\n");
    out
}

#[cfg(test)]
mod tests;
