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
    let first_item = ast.items.iter().map(|i| i.span().start).min().unwrap_or(u32::MAX);
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
    fn overview(&self, first_item: u32) -> String {
        let attached = self.block_before(first_item);
        let mut lines = Vec::new();
        for c in self.comments {
            if c.span.start >= first_item {
                break;
            }
            if attached.contains(&c.span.start) {
                continue;
            }
            lines.push(strip(self.text(c.span)));
        }
        trim_block(&lines)
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
            let between = &self.src[c.span.end as usize..edge as usize];
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
        &self.src[span.start as usize..span.end as usize]
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
