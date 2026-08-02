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
use std::collections::HashMap;

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
}

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
    /// Traits this type already implements by hand, by their written path.
    hand_written: Vec<String>,
    generic: bool,
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
pub fn expand(items: &[Item], item_modules: &[String], diags: &mut DiagBag) -> Option<Derived> {
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
            hand_written: Vec::new(),
            generic,
        });
    }
    if decls.is_empty() {
        return None;
    }

    // A hand-written `impl Debug for X` beside a `@derive(Debug)` would be two
    // bodies for one method, and the error for that points at generated code.
    // It is better caught here, where the derive can be named.
    for (i, item) in items.iter().enumerate() {
        let Item::Impl(imp) = item else { continue };
        let Some(tr) = &imp.trait_path else { continue };
        let module = item_modules.get(i).cloned().unwrap_or_default();
        let target = path_text(&imp.self_ty);
        let trait_name = path_text(tr);
        for decl in &mut decls {
            // An `impl` written unqualified means the type of *its own*
            // module. Two modules may each declare a `User`, and taking a
            // hand-written `impl` in one as covering the other's would let a
            // derive recurse into a method that is not there.
            let names_it = target == decl.qualified || (target == decl.bare && module == decl.module);
            if names_it {
                decl.hand_written.push(trait_name.clone());
            }
        }
    }

    let known: HashMap<String, usize> =
        decls.iter().enumerate().map(|(i, d)| (d.qualified.clone(), i)).collect();
    // Every type in the program, so a field can be recognised as a type that
    // exists but does not derive — which is a much better diagnostic than one
    // about a missing method.
    let mut all_types: Vec<(String, String)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let module = item_modules.get(i).cloned().unwrap_or_default();
        match item {
            Item::Struct(s) => all_types.push((s.name.name.clone(), module)),
            Item::Enum(e) => all_types.push((e.name.name.clone(), module)),
            _ => {}
        }
    }

    let mut source = String::from(
        "// Generated by `@derive`. This file is written by the compiler, parsed\n\
         // like any other, and is what actually runs — there is no second path\n\
         // through the checker for a derived body.\n",
    );
    let mut modules = Vec::new();
    for index in 0..decls.len() {
        for (trait_, at) in decls[index].derives.clone() {
            let mut w = Writer {
                decls: &decls,
                known: &known,
                all_types: &all_types,
                module: decls[index].module.clone(),
                trait_,
                next: 0,
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
    Some(Derived { source, modules })
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
    module: String,
    trait_: Derivable,
    next: usize,
    diags: &'d mut DiagBag,
    /// Set when a field could not be walked. The item is dropped rather than
    /// emitted half-written, so one bad field is one diagnostic and not a
    /// cascade of parse errors in generated text.
    failed: bool,
}

/// Statements accumulate as lines at a known indent; an expression is a string.
type Lines = Vec<String>;

impl Writer<'_, '_> {
    fn temp(&mut self, stem: &str) -> String {
        self.next += 1;
        format!("{}_{}", stem, self.next)
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
                .with_note("write the implementation by hand, once per instantiation or with a bound"),
            );
            return String::new();
        }
        let path = self.trait_.path();
        if !path.is_empty() && decl.hand_written.iter().any(|t| t == path || t == self.trait_.name())
        {
            self.failed = true;
            self.diags.push(
                Diagnostic::error(
                    codes::E0701,
                    format!("`{}` already implements `{}`", decl.bare, self.trait_.name()),
                )
                .with_primary(at, "derived here")
                .with_note("a derive writes the same method the hand-written `impl` does")
                .with_note("remove one of the two"),
            );
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
                body.push("    var out = \"\"".to_string());
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
                    body.push(format!("            out = {}", parts.join(" + ")));
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
                body.push("    return out".to_string());
            }
        }
        wrap_impl(&decl.bare, Some("Debug"), "fn debug(self) -> str", &body)
    }

    /// A value of `ty`, held in `expr`, rendered for a programmer.
    fn debug_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
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
        body.push("    var h = hash_seed()".to_string());
        match &decl.kind {
            Shape::Struct(s) => {
                for f in &s.fields {
                    let bound = self.temp("field");
                    body.push(format!("    let {} = self.{}", bound, f.name.name));
                    let value = self.hash_of(&f.ty, &bound, 1, &mut body);
                    body.push(format!("    h = hash_combine(h, {})", value));
                }
            }
            Shape::Enum(e) => {
                body.push("    match self {".to_string());
                for (index, v) in e.variants.iter().enumerate() {
                    let (head, binds) = self.variant_pattern(v);
                    body.push(format!("        {} => {{", head));
                    // The variant's position, so two variants with the same
                    // payload do not hash alike.
                    body.push(format!("            h = hash_combine(h, {})", index));
                    for (bind, ty, _) in &binds {
                        let mut inner: Lines = Vec::new();
                        let value = self.hash_of(ty, bind, 3, &mut inner);
                        body.extend(inner);
                        body.push(format!("            h = hash_combine(h, {})", value));
                    }
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
            }
        }
        body.push("    return h".to_string());
        wrap_impl(&decl.bare, Some("Hash"), "fn hash(self) -> int", &body)
    }

    fn hash_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
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
        let mut body: Lines = Vec::new();
        match &decl.kind {
            Shape::Struct(s) => {
                body.push("    var fields: { str: json.Json } = { }".to_string());
                for f in &s.fields {
                    let bound = self.temp("field");
                    body.push(format!("    let {} = self.{}", bound, f.name.name));
                    let value = self.encode_of(&f.ty, &bound, 1, &mut body);
                    body.push(format!("    fields[{}] = {}", quote(&f.name.name), value));
                }
                body.push("    return json.Json.Object(fields)".to_string());
            }
            Shape::Enum(e) => {
                // Externally tagged: a unit variant is its own name as text,
                // and a payload is a one-key object. It round-trips, it reads,
                // and it is what every other language's JSON does — which
                // matters more here than elegance, because the other end of
                // this is not written in Kite.
                body.push("    var out = json.Json.Null".to_string());
                body.push("    match self {".to_string());
                for v in &e.variants {
                    let (head, binds) = self.variant_pattern(v);
                    body.push(format!("        {} => {{", head));
                    if binds.is_empty() {
                        body.push(format!(
                            "            out = json.Json.Text({})",
                            quote(&v.name.name)
                        ));
                    } else if binds.iter().all(|(_, _, label)| label.is_some()) {
                        body.push(
                            "            var payload: { str: json.Json } = { }".to_string(),
                        );
                        for (bind, ty, label) in &binds {
                            let mut inner: Lines = Vec::new();
                            let value = self.encode_of(ty, bind, 3, &mut inner);
                            body.extend(inner);
                            body.push(format!(
                                "            payload[{}] = {}",
                                quote(label.as_deref().unwrap_or("")),
                                value
                            ));
                        }
                        body.push("            var wrapper: { str: json.Json } = { }".to_string());
                        body.push(format!(
                            "            wrapper[{}] = json.Json.Object(payload)",
                            quote(&v.name.name)
                        ));
                        body.push("            out = json.Json.Object(wrapper)".to_string());
                    } else {
                        body.push("            var payload: [json.Json] = []".to_string());
                        for (bind, ty, _) in &binds {
                            let mut inner: Lines = Vec::new();
                            let value = self.encode_of(ty, bind, 3, &mut inner);
                            body.extend(inner);
                            body.push(format!("            payload.push({})", value));
                        }
                        body.push("            var wrapper: { str: json.Json } = { }".to_string());
                        body.push(format!(
                            "            wrapper[{}] = json.Json.Array(payload)",
                            quote(&v.name.name)
                        ));
                        body.push("            out = json.Json.Object(wrapper)".to_string());
                    }
                    body.push("        }".to_string());
                }
                body.push("    }".to_string());
                body.push("    return out".to_string());
            }
        }
        wrap_impl(&decl.bare, Some("json.Encode"), "fn encode(self) -> json.Json", &body)
    }

    fn encode_of(&mut self, ty: &Type, expr: &str, depth: usize, out: &mut Lines) -> String {
        let pad = "    ".repeat(depth);
        match ty {
            Type::Path(p) => match path_text(p).as_str() {
                // JSON has one numeric type, and `json.stringify` writes a
                // whole number without a point, so an `int` survives the round
                // trip as an `int`.
                "int" => format!("json.Json.Number({} as float)", expr),
                "float" => format!("json.Json.Number({})", expr),
                "bool" => format!("json.Json.Bool({})", expr),
                "str" => format!("json.Json.Text({})", expr),
                other => self.recurse(other, expr, ty),
            },
            Type::Optional { inner, .. } => {
                let held = self.temp("some");
                let node = self.temp("node");
                out.push(format!("{}let {} = {}", pad, held, expr));
                out.push(format!("{}var {} = json.Json.Null", pad, node));
                out.push(format!("{}if {} != nil {{", pad, held));
                let value = self.encode_of(inner, &held, depth + 1, out);
                out.push(format!("{}    {} = {}", pad, node, value));
                out.push(format!("{}}}", pad));
                node
            }
            Type::Slice { elem, .. } => {
                let list = self.temp("items");
                let item = self.temp("item");
                out.push(format!("{}var {}: [json.Json] = []", pad, list));
                out.push(format!("{}for {} in {} {{", pad, item, expr));
                let value = self.encode_of(elem, &item, depth + 1, out);
                out.push(format!("{}    {}.push({})", pad, list, value));
                out.push(format!("{}}}", pad));
                format!("json.Json.Array({})", list)
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
                out.push(format!("{}var {}: {{ str: json.Json }} = {{ }}", pad, object));
                out.push(format!("{}for ({}, {}) in {} {{", pad, k, v, expr));
                let encoded = self.encode_of(value, &v, depth + 1, out);
                out.push(format!("{}    {}[{}] = {}", pad, object, k, encoded));
                out.push(format!("{}}}", pad));
                format!("json.Json.Object({})", object)
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
                "json.Json.Null".to_string()
            }
        }
    }

    // ---- Decode -----------------------------------------------------------

    fn decode_item(&mut self, decl: &Decl) -> String {
        let mut body: Lines = Vec::new();
        match &decl.kind {
            Shape::Struct(s) => {
                let mut values = Vec::new();
                for f in &s.fields {
                    let where_ = format!("{}.{}", decl.bare, f.name.name);
                    let value = self.decode_of(
                        &f.ty,
                        &format!("json.field(doc, {})", quote(&f.name.name)),
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
                body.push(format!("    let {} = json.text(doc)", tag));
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
                        "    let {} = json.field(doc, {})",
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
                                    &format!("json.field({}, {})", held, quote(&f.name.name)),
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
                                    &format!("json.at({}, {})", held, i),
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
        wrap_impl(
            &decl.bare,
            None,
            &format!("fn decode(doc: json.Json) -> ({}, error)", decl.bare),
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
        let pad = "    ".repeat(depth);
        // Whatever came in, the rest of this works on an `Option<json.Json>`.
        // A document node arrives three ways — from an accessor, from a loop
        // over an array, from inside a narrowing `if` — and only the first is
        // already optional. Normalising once here is what lets the walk be
        // written once rather than three times.
        let source = &{
            let normalised = self.temp("src");
            out.push(format!(
                "{}let {}: Option<json.Json> = {}",
                pad, normalised, source
            ));
            normalised
        };
        match ty {
            Type::Path(p) => {
                let (reader, what) = match path_text(p).as_str() {
                    "int" => ("json.int_of", "a whole number"),
                    "float" => ("json.number_of", "a number"),
                    "bool" => ("json.bool_of", "a boolean"),
                    "str" => ("json.text", "a string"),
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
                out.push(format!("{}var {}: {} = nil", pad, held, render_type(ty)));
                out.push(format!("{}if !json.is_null({}) {{", pad, source));
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
                out.push(format!("{}var {}: {} = []", pad, list, render_type(ty)));
                out.push(format!("{}for {} in json.items({}) {{", pad, item, node));
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
                out.push(format!("{}var {}: {} = {{ }}", pad, object, render_type(ty)));
                out.push(format!("{}for ({}, {}) in json.entries({}) {{", pad, k, v, node));
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
        // A dotted name already says which module it is in, and a name written
        // in the root module is its own qualified form.
        let qualified = if name.contains('.') || self.module.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.module, name)
        };
        let found = self
            .known
            .get(&qualified)
            .or_else(|| self.known.get(name))
            .map(|i| &self.decls[*i]);
        if let Some(decl) = found {
            if decl.derives.iter().any(|(t, _)| *t == self.trait_) {
                return name.to_string();
            }
            let path = self.trait_.path();
            if !path.is_empty() && decl.hand_written.iter().any(|t| t == path || t == self.trait_.name()) {
                return name.to_string();
            }
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
        let exists = self
            .all_types
            .iter()
            .any(|(n, _)| *n == qualified || *n == name || n.ends_with(&format!(".{}", name)));
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
