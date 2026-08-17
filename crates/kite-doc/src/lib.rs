//! `kitec doc` — the reference, extracted from the source.
//!
//! The standard library is written in Kite, so its documentation and a user's
//! are produced by the same tool. That is the only arrangement in which the
//! two stay comparable: a library documented by hand drifts from its code, and
//! one documented by a tool nobody else can run is not a library anyone can
//! contribute to.
//!
//! A `///` comment attaches to the declaration that follows it. Everything
//! else about a declaration — its signature, whether it is `pub`, whether it
//! is `async` — is read from the parsed item rather than restated in prose,
//! so a signature in the output cannot be wrong.

use kite_ast::Item;
use kite_diag::DiagBag;
use kite_lexer::Comment;
use kite_span::{FileId, Span};

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
}

pub struct Member {
    pub name: String,
    pub signature: String,
    pub doc: String,
}

/// A module's documentation: the header comment, then every declaration.
pub struct Docs {
    pub module: String,
    /// The comment block at the top of the file, before any declaration.
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
    Docs { module: module.to_string(), overview, entries }
}

struct Reader<'a> {
    src: &'a str,
    comments: &'a [Comment],
}

impl Reader<'_> {
    /// The comment block at the top of the file: everything before the first
    /// declaration that is not attached to it.
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
    fn overview(&self, first_item: u32) -> String {
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
                    })
                    .collect();
                (
                    "struct",
                    s.name.name.clone(),
                    s.is_pub,
                    format!("struct {}", s.name.name),
                    members,
                )
            }
            Item::Enum(e) => {
                let members = e
                    .variants
                    .iter()
                    .map(|v| Member {
                        name: v.name.name.clone(),
                        signature: self.text(v.span).trim().to_string(),
                        doc: self.doc_above(v.span.start),
                    })
                    .collect();
                ("enum", e.name.name.clone(), e.is_pub, format!("enum {}", e.name.name), members)
            }
            Item::Trait(t) => {
                let members = t
                    .methods
                    .iter()
                    .map(|m| Member {
                        name: m.name.name.clone(),
                        signature: self.text(m.sig_span).trim().to_string(),
                        doc: self.doc_above(m.span.start),
                    })
                    .collect();
                ("trait", t.name.name.clone(), t.is_pub, format!("trait {}", t.name.name), members)
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
            // An `impl` block documents its methods against the type they are
            // on, which the type's own entry already lists.
            Item::Impl(_) | Item::Error(_) => return None,
        };
        Some(Entry {
            kind,
            doc: self.doc_above(item.span().start),
            name,
            signature,
            is_pub,
            members,
        })
    }

    fn text(&self, span: Span) -> &str {
        self.between(span.start, span.end)
    }
}

/// A comment's text, without its marker and one leading space.
fn strip(line: &str) -> String {
    let body = line.trim_start().trim_start_matches('/');
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
    for e in &entries {
        out.push_str(&format!("- [`{}`](#{})\n", e.name, anchor(&e.name)));
    }
    out.push('\n');

    for e in &entries {
        out.push_str(&format!("## {}\n\n", e.name));
        out.push_str(&format!("```kite\n{}\n```\n\n", e.signature));
        if !e.doc.is_empty() {
            out.push_str(&e.doc);
            out.push_str("\n\n");
        }
        if !e.members.is_empty() {
            for m in &e.members {
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

fn anchor(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests;
