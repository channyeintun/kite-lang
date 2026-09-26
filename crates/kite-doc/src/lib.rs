//! `kitec doc` — the reference, extracted from the source.
//!
//! The standard library is written in Kite, so its documentation and a user's
//! are produced by the same tool. That is the only arrangement in which the
//! two stay comparable: a library documented by hand drifts from its code, and
//! one documented by a tool nobody else can run is not a library anyone can
//! contribute to.
//!
//! A `///` comment attaches to the declaration that follows it, and `//!`
//! lines are the module's own overview. Everything else about a declaration —
//! its signature, whether it is `pub`, whether it is `async` — is read from
//! the parsed item rather than restated in prose, so a signature in the output
//! cannot be wrong.

use kite_ast::{Item, TypePath};
use kite_diag::DiagBag;
use kite_lexer::Comment;
use kite_span::{FileId, Span};
use std::collections::HashMap;

/// One documented declaration.
pub struct Entry {
    /// `function`, `struct`, `enum`, `trait`, `type alias`, `host function`.
    pub kind: &'static str,
    pub name: String,
    /// The declaration as written, without its body.
    pub signature: String,
    /// The `///` lines, with the marker and one leading space removed.
    pub doc: String,
    pub is_pub: bool,
    /// Members: a struct's fields, an enum's variants, a trait's methods.
    pub members: Vec<Member>,
    /// What `impl` blocks in the same file add to a type: its methods, and
    /// the traits it implements.
    pub methods: Vec<Member>,
}

pub struct Member {
    pub name: String,
    pub signature: String,
    pub doc: String,
    /// Whether an importer can reach it. A field without `pub` is the
    /// module's own business even when its struct is exported.
    pub is_pub: bool,
}

/// A module's documentation: the header comment, then every declaration.
pub struct Docs {
    pub module: String,
    /// The module's `//!` lines — or, in a file that has none, the comment
    /// block at the top of the file, before any declaration.
    pub overview: String,
    pub entries: Vec<Entry>,
}

/// Extract the documentation from one file.
pub fn extract(module: &str, src: &str) -> Docs {
    let mut diags = DiagBag::new();
    let file = FileId(0);
    let (tokens, comments) = kite_lexer::tokenize_with_comments(file, src, &mut diags);
    let ast = kite_parser::parse(file, src, &tokens, &mut diags);

    let reader = Reader { src, comments: &comments };
    // With no items, everything in the file is above the first one — so the
    // edge is the end of the source, not `u32::MAX`. The sentinel was used as
    // a slice bound further down, so a file holding comments and no
    // declarations (a stub, or a header plus `use` lines, which parse into
    // `file.uses` rather than `file.items`) indexed the source at 4 GiB and
    // panicked. That is an abort in the wasm build and across the `extern "C"`
    // boundary the playground calls through.
    let first_item = ast
        .items
        .iter()
        .map(|i| i.span().start)
        .min()
        .unwrap_or(src.len() as u32);
    let overview = reader.overview(first_item);

    let mut entries = Vec::new();
    for item in &ast.items {
        if let Some(entry) = reader.entry(item) {
            entries.push(entry);
        }
    }

    // An `impl` block documents its methods against the type they are on:
    // `Listener.cancel` belongs on `Listener`'s entry, which is where a
    // reader looking for what a `Listener` can do will be. They used to be
    // left out altogether.
    for item in &ast.items {
        let Item::Impl(imp) = item else { continue };
        let target = last_segment(&imp.self_ty);
        let (methods, owner_kind) = reader.impl_members(imp);
        match entries.iter_mut().find(|e| e.name == target && e.kind != "function") {
            Some(entry) => entry.methods.extend(methods),
            // A type declared somewhere else gets an entry of its own here,
            // which is the only place its methods from this file are said.
            None => entries.push(Entry {
                kind: owner_kind,
                name: target.to_string(),
                signature: reader.text(imp_head(imp)).trim().to_string(),
                doc: reader.doc_above(imp.span.start),
                is_pub: methods.iter().any(|m| m.is_pub),
                members: Vec::new(),
                methods,
            }),
        }
    }
    Docs { module: module.to_string(), overview, entries }
}

/// The name a type path ends in: `Listener` for `window.Listener<T>`.
fn last_segment(path: &TypePath) -> &str {
    path.segments.last().map(|s| s.name.as_str()).unwrap_or("")
}

/// The part of an `impl` before its `{`.
fn imp_head(imp: &kite_ast::ImplDecl) -> Span {
    let end = imp.self_ty.span.end;
    Span::new(imp.span.file, imp.span.start, end.max(imp.span.start))
}

struct Reader<'a> {
    src: &'a str,
    comments: &'a [Comment],
}

impl Reader<'_> {
    /// The module's overview.
    ///
    /// `//!` lines say outright that they are the file's own documentation,
    /// and where a file has them they are the overview, wherever in the header
    /// they sit. A file without them has its header read the older way,
    /// below. The marker comes off either way; it used to stay on a `//!`
    /// line as a `!`, so the reference pages of `std/dom`, `std/canvas`,
    /// `std/js`, `std/window` and `std/html` began every line of their
    /// overview with one.
    fn overview(&self, first_item: u32) -> String {
        let module: Vec<String> = self
            .comments
            .iter()
            .filter(|c| c.module && c.span.start < first_item)
            .map(|c| strip(self.text(c.span)))
            .collect();
        if !module.is_empty() {
            return trim_block(&module);
        }
        self.header(first_item)
    }

    /// The file's own header: the run of comments it opens with.
    ///
    /// **The run**, and not everything before the first declaration. A file
    /// separates its sections with a comment —
    ///
    /// ```text
    /// // ---- display ----------------------------------------------------
    /// ```
    ///
    /// — and one of those sitting above the first declaration is not about the
    /// module. It used to be collected anyway, so `std/prelude`'s reference
    /// page ended its overview with a row of hyphens and the word `display`
    /// run into the sentence before it. Four modules did that.
    ///
    /// A blank line ends the header. Inside one, a paragraph break is an empty
    /// `//` line, which is a comment and keeps the run going.
    fn header(&self, first_item: u32) -> String {
        let attached = self.block_before(first_item);
        let mut lines = Vec::new();
        let mut previous_end: Option<u32> = None;
        for c in self.comments {
            if c.span.start >= first_item {
                break;
            }
            if attached.contains(&c.span.start) {
                continue;
            }
            if let Some(end) = previous_end {
                let between = self.between(end, c.span.start);
                if between.matches('\n').count() > 1 {
                    break;
                }
            }
            lines.push(strip(self.text(c.span)));
            previous_end = Some(c.span.end);
        }
        trim_block(&lines)
    }

    /// The source between two offsets, or `""` if they do not describe a range
    /// inside it.
    ///
    /// Slicing a `str` by unvalidated `u32` offsets panics twice over — out of
    /// bounds, and on an index that is not a character boundary — and this
    /// runs inside a language server and a wasm module, where a panic is not a
    /// diagnostic but the end of the process.
    fn between(&self, lo: u32, hi: u32) -> &str {
        let lo = lo as usize;
        let hi = (hi as usize).min(self.src.len());
        if lo > hi {
            return "";
        }
        self.src.get(lo..hi).unwrap_or("")
    }

    /// The run of comment lines immediately above `at`, with nothing but
    /// whitespace between them.
    fn block_before(&self, at: u32) -> Vec<u32> {
        let mut block: Vec<u32> = Vec::new();
        let mut edge = at;
        for c in self.comments.iter().rev() {
            if c.span.end > at {
                continue;
            }
            // Clamped rather than indexed directly: a span is a `u32` pair and
            // this is the boundary where a bad one stops being a wrong answer
            // and becomes a dead process.
            let between = self.between(c.span.end, edge);
            // One line break separates a comment from what it documents; two
            // mean it was about something else.
            if between.matches('\n').count() > 1 || !between.trim().is_empty() {
                break;
            }
            block.push(c.span.start);
            edge = c.span.start;
        }
        block.reverse();
        block
    }

    fn doc_above(&self, at: u32) -> String {
        let block = self.block_before(at);
        let lines: Vec<String> = block
            .iter()
            .filter_map(|start| self.comments.iter().find(|c| c.span.start == *start))
            .filter(|c| c.doc)
            .map(|c| strip(self.text(c.span)))
            .collect();
        trim_block(&lines)
    }

    /// A struct, enum or trait's signature: what is written before its `{`,
    /// generic parameters and `pub` included. It used to be rebuilt as
    /// `struct Pair`, which left out the two things a reader of
    /// `pub struct Pair<A, B>` needs first.
    fn head(&self, span: Span, name: Span) -> String {
        let rest = self.between(name.end, span.end);
        let generics = rest.split('{').next().unwrap_or("").trim_end();
        let before = self.between(span.start, name.start);
        // The keyword and `pub` before the name, without the `@derive` line
        // above them — that is listed as what it is, below.
        let keywords = before.lines().last().unwrap_or("").trim();
        format!("{} {}{}", keywords, self.text(name), generics)
    }

    fn method(&self, m: &kite_ast::MethodDecl, is_pub: bool) -> Member {
        Member {
            name: m.name.name.clone(),
            signature: self.text(m.sig_span).trim().to_string(),
            doc: self.doc_above(m.span.start),
            is_pub,
        }
    }

    /// What an `impl` adds to its type: an inherent one's methods, each as
    /// public as it says, or the trait a trait impl implements — whose
    /// methods are the trait's to document, and as public as the trait is.
    fn impl_members(&self, imp: &kite_ast::ImplDecl) -> (Vec<Member>, &'static str) {
        match &imp.trait_path {
            None => (imp.methods.iter().map(|m| self.method(m, m.is_pub)).collect(), "methods"),
            Some(tr) => (
                vec![Member {
                    name: last_segment(tr).to_string(),
                    signature: self.text(imp_head(imp)).trim().to_string(),
                    doc: self.doc_above(imp.span.start),
                    is_pub: true,
                }],
                "trait implementation",
            ),
        }
    }

    fn entry(&self, item: &Item) -> Option<Entry> {
        let (kind, name, is_pub, signature, members) = match item {
            Item::Fn(f) => (
                if f.is_async { "async function" } else { "function" },
                f.name.name.clone(),
                f.is_pub,
                self.text(f.sig_span).trim().to_string(),
                Vec::new(),
            ),
            Item::Extern(e) => (
                "host function",
                e.name.name.clone(),
                e.is_pub,
                self.text(e.span).trim().to_string(),
                Vec::new(),
            ),
            Item::Struct(s) => {
                let members = s
                    .fields
                    .iter()
                    .map(|f| Member {
                        name: f.name.name.clone(),
                        signature: self.text(f.span).trim().to_string(),
                        doc: self.doc_above(f.span.start),
                        is_pub: f.is_pub,
                    })
                    .collect();
                ("struct", s.name.name.clone(), s.is_pub, self.head(s.span, s.name.span), members)
            }
            Item::Enum(e) => {
                let members = e
                    .variants
                    .iter()
                    .map(|v| Member {
                        name: v.name.name.clone(),
                        signature: self.text(v.span).trim().to_string(),
                        doc: self.doc_above(v.span.start),
                        // A variant is as visible as its enum.
                        is_pub: true,
                    })
                    .collect();
                ("enum", e.name.name.clone(), e.is_pub, self.head(e.span, e.name.span), members)
            }
            Item::Trait(t) => {
                let members = t
                    .methods
                    .iter()
                    // A trait's methods are its interface, as visible as it.
                    .map(|m| self.method(m, true))
                    .collect();
                ("trait", t.name.name.clone(), t.is_pub, self.head(t.span, t.name.span), members)
            }
            Item::TypeAlias(a) => (
                "type alias",
                a.name.name.clone(),
                a.is_pub,
                self.text(a.span).trim().to_string(),
                Vec::new(),
            ),
            // The whole declaration is the signature: a constant's value is
            // the interesting half, and it is short by construction.
            Item::Const(c) => (
                "constant",
                c.name.name.clone(),
                c.is_pub,
                self.text(c.span).trim().to_string(),
                Vec::new(),
            ),
            // An `impl` block is gathered onto the entry of the type it is
            // for, once every entry exists; see `extract`.
            Item::Impl(_) | Item::Error(_) => return None,
        };
        let derives = match item {
            Item::Struct(s) => &s.derives,
            Item::Enum(e) => &e.derives,
            _ => &Vec::new(),
        };
        let signature = if derives.is_empty() {
            signature
        } else {
            let names: Vec<&str> = derives.iter().map(|d| d.name.as_str()).collect();
            format!("@derive({})\n{}", names.join(", "), signature)
        };
        Some(Entry {
            kind,
            doc: self.doc_above(item.span().start),
            name,
            signature,
            is_pub,
            members,
            methods: Vec::new(),
        })
    }

    fn text(&self, span: Span) -> &str {
        self.between(span.start, span.end)
    }
}

/// A comment's text, without its marker and one leading space.
fn strip(line: &str) -> String {
    let body = line.trim_start().trim_start_matches('/');
    let body = body.strip_prefix('!').unwrap_or(body);
    body.strip_prefix(' ').unwrap_or(body).to_string()
}

fn trim_block(lines: &[String]) -> String {
    let mut out: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The reference as Markdown.
///
/// Markdown rather than HTML because the documentation is read in as many
/// places as the source is — a terminal, a repository host, an editor — and
/// only one of them renders HTML.
///
/// The public reference is what an importer can reach: exported declarations,
/// and of their members only the `pub` ones. A field without `pub` is the
/// module's own, however public its struct — `crypto.Key.handle` and
/// `dom.Element.raw` were listed in a reference to things nobody outside
/// could touch.
pub fn markdown(docs: &Docs, public_only: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", docs.module));
    if !docs.overview.is_empty() {
        out.push_str(&docs.overview);
        out.push_str("\n\n");
    }

    let entries: Vec<&Entry> = docs
        .entries
        .iter()
        .filter(|e| e.is_pub || !public_only)
        .collect();
    if entries.is_empty() {
        out.push_str("_Nothing is exported from this module._\n");
        return out;
    }

    // A table of contents, because a module's surface is what a reader is
    // looking for and scrolling to find it is not reading.
    let mut anchors = Anchors::default();
    let anchors: Vec<String> = entries.iter().map(|e| anchors.next(&e.name)).collect();
    for (e, anchor) in entries.iter().zip(&anchors) {
        out.push_str(&format!("- [`{}`](#{})\n", e.name, anchor));
    }
    out.push('\n');

    let shown = |m: &&Member| m.is_pub || !public_only;
    for e in &entries {
        out.push_str(&format!("## {}\n\n", e.name));
        out.push_str(&format!("```kite\n{}\n```\n\n", e.signature));
        if !e.doc.is_empty() {
            out.push_str(&e.doc);
            out.push_str("\n\n");
        }
        for list in [&e.members, &e.methods] {
            let visible: Vec<&Member> = list.iter().filter(shown).collect();
            if visible.is_empty() {
                continue;
            }
            for m in visible {
                out.push_str(&format!("- `{}`", m.signature));
                if !m.doc.is_empty() {
                    out.push_str(&format!(" — {}", m.doc.replace('\n', " ")));
                }
                out.push('\n');
            }
            out.push('\n');
        }
    }
    out
}

/// Heading anchors, as a repository host makes them: lowercase, and a
/// heading whose anchor is already taken gets `-1`, `-2` and so on after it.
/// `Pair` and `pair` both linked to `#pair`, which is the first of them.
#[derive(Default)]
struct Anchors {
    seen: HashMap<String, usize>,
}

impl Anchors {
    fn next(&mut self, name: &str) -> String {
        let base = anchor(name);
        let count = self.seen.entry(base.clone()).or_insert(0);
        let out = if *count == 0 { base } else { format!("{}-{}", base, count) };
        *count += 1;
        out
    }
}

fn anchor(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests;
