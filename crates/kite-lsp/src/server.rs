//! The protocol: what the editor asks, and what this answers.
//!
//! Every answer comes from the same passes `kitec` runs. A language server
//! that re-derives its own answers is a second compiler that disagrees with
//! the first one, and the disagreement always surfaces as "the editor says
//! this is fine and the build says it is not".

use crate::json::Json;
use kite_diag::Severity;
use kite_driver::{compile, Binding, Compilation, Emit};
use kite_span::{FileId, Span};
use std::collections::HashMap;

/// Files the editor has open, by URI. The editor's copy is the truth while a
/// file is open — it may hold edits that are not on disk yet.
#[derive(Default)]
pub struct Server {
    open: HashMap<String, String>,
    pub shutdown: bool,
}

/// What to send back: an answer to a request, and any notifications.
pub struct Reply {
    pub result: Option<Json>,
    /// A refusal: the request was understood and the answer is no, with the
    /// reason. Sent as a protocol error rather than an empty result, because
    /// an empty result looks like the server silently doing nothing — and a
    /// rename that silently does nothing teaches people not to trust it.
    pub error: Option<String>,
    pub notifications: Vec<(String, Json)>,
}

impl Reply {
    fn none() -> Reply {
        Reply { result: None, error: None, notifications: Vec::new() }
    }

    fn result(value: Json) -> Reply {
        Reply { result: Some(value), error: None, notifications: Vec::new() }
    }

    fn refuse(message: impl Into<String>) -> Reply {
        Reply { result: None, error: Some(message.into()), notifications: Vec::new() }
    }
}

impl Server {
    pub fn new() -> Server {
        Server::default()
    }

    pub fn handle(&mut self, method: &str, message: &Json) -> Reply {
        match method {
            "initialize" => Reply::result(capabilities()),
            "initialized" => Reply::none(),
            "shutdown" => {
                self.shutdown = true;
                Reply::result(Json::Null)
            }

            "textDocument/didOpen" => {
                let uri = uri_of(message).unwrap_or_default();
                let text = message
                    .path("params.textDocument.text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                self.open.insert(uri.clone(), text);
                self.diagnostics(&uri)
            }
            "textDocument/didChange" => {
                let uri = uri_of(message).unwrap_or_default();
                // Full synchronisation: the editor sends the whole file. It is
                // what a compiler that reparses from scratch wants anyway, and
                // incremental sync would be a second source of truth about
                // what the file says.
                if let Some(Json::Array(changes)) = message.path("params.contentChanges") {
                    if let Some(last) = changes.last() {
                        if let Some(text) = last.get("text").and_then(|t| t.as_str()) {
                            self.open.insert(uri.clone(), text.to_string());
                        }
                    }
                }
                self.diagnostics(&uri)
            }
            "textDocument/didSave" => {
                let uri = uri_of(message).unwrap_or_default();
                self.diagnostics(&uri)
            }
            "textDocument/didClose" => {
                let uri = uri_of(message).unwrap_or_default();
                self.open.remove(&uri);
                // An empty list clears what was shown for the file.
                Reply {
                    result: None,
                    error: None,
                    notifications: vec![(
                        "textDocument/publishDiagnostics".to_string(),
                        Json::object(vec![
                            ("uri", Json::str(uri)),
                            ("diagnostics", Json::Array(Vec::new())),
                        ]),
                    )],
                }
            }

            "textDocument/hover" => self.hover(message),
            "textDocument/definition" => self.definition(message),
            "textDocument/completion" => self.completion(message),
            "textDocument/documentSymbol" => self.symbols(message),
            "textDocument/formatting" => self.formatting(message),
            "textDocument/references" => self.references(message),
            "textDocument/prepareRename" => self.prepare_rename(message),
            "textDocument/rename" => self.rename(message),
            "textDocument/inlayHint" => self.inlay_hints(message),

            // Anything else: an empty answer rather than an error. An editor
            // asks about capabilities it was not told about, and refusing is
            // noisier than saying nothing.
            _ => Reply::result(Json::Null),
        }
    }

    fn text(&self, uri: &str) -> String {
        self.open.get(uri).cloned().unwrap_or_default()
    }

    // ---- diagnostics -------------------------------------------------------

    fn diagnostics(&self, uri: &str) -> Reply {
        let text = self.text(uri);
        let path = path_of(uri);
        let compiled = compile(&path, &text, Emit::Check);
        let mut items = Vec::new();
        for d in compiled.diags.iter() {
            let Some(span) = d.primary_span() else { continue };
            // Only what is in this file: a diagnostic pointing into the
            // standard library is not something the editor can show against a
            // line the user has open.
            let Some(file) = compiled.sources.iter().find(|(_, name)| *name == path) else {
                continue;
            };
            if span.file != file.0 {
                continue;
            }
            let mut notes: Vec<String> = d.notes.clone();
            for label in d.labels.iter().skip(1) {
                notes.push(label.message.clone());
            }
            let mut message = d.message.clone();
            if let Some(first) = d.labels.first() {
                if !first.message.is_empty() {
                    message.push_str(&format!("\n{}", first.message));
                }
            }
            for note in notes {
                message.push_str(&format!("\nnote: {}", note));
            }
            items.push(Json::object(vec![
                ("range", range_of(&text, span)),
                (
                    "severity",
                    Json::number(match d.severity {
                        Severity::Error => 1,
                        Severity::Warning => 2,
                        Severity::Note => 3,
                    }),
                ),
                ("code", Json::str(d.code.map(|c| c.0).unwrap_or(""))),
                ("source", Json::str("kite")),
                ("message", Json::str(message)),
            ]));
        }
        Reply {
            result: None,
            error: None,
            notifications: vec![(
                "textDocument/publishDiagnostics".to_string(),
                Json::object(vec![
                    ("uri", Json::str(uri.to_string())),
                    ("diagnostics", Json::Array(items)),
                ]),
            )],
        }
    }

    // ---- hover and definition ----------------------------------------------

    fn hover(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let Some(offset) = position_of(message, &text) else {
            return Reply::result(Json::Null);
        };
        let compiled = compile(path_of(&uri), &text, Emit::Check);
        let Some(found) = compiled
            .index
            .uses
            .iter()
            .find(|u| u.at.start <= offset && offset < u.at.end.max(u.at.start + 1))
        else {
            return Reply::result(Json::Null);
        };
        Reply::result(Json::object(vec![(
            "contents",
            Json::object(vec![
                ("kind", Json::str("markdown")),
                (
                    "value",
                    Json::str(format!("```kite\n{}\n```\n\n*{}*", found.label, found.kind)),
                ),
            ]),
        )]))
    }

    fn definition(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let Some(offset) = position_of(message, &text) else {
            return Reply::result(Json::Null);
        };
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own_file = compiled
            .sources
            .iter()
            .find(|(_, name)| *name == path)
            .map(|(id, _)| id);
        let Some(found) = compiled
            .index
            .uses
            .iter()
            .find(|u| u.at.start <= offset && offset < u.at.end.max(u.at.start + 1))
        else {
            return Reply::result(Json::Null);
        };
        // A definition in another file — the standard library — has no URI the
        // editor can open, so only this file's are answered.
        if Some(found.declared_at.file) != own_file {
            return Reply::result(Json::Null);
        }
        Reply::result(Json::object(vec![
            ("uri", Json::str(uri)),
            ("range", range_of(&text, found.declared_at)),
        ]))
    }

    // ---- completion and symbols --------------------------------------------

    fn completion(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let compiled = compile(path_of(&uri), &text, Emit::Check);
        let mut items = Vec::new();
        for keyword in kite_lexer::KEYWORDS {
            items.push(Json::object(vec![
                ("label", Json::str(keyword)),
                ("kind", Json::number(14)), // Keyword
            ]));
        }
        for symbol in &compiled.index.symbols {
            items.push(Json::object(vec![
                ("label", Json::str(symbol.name.clone())),
                (
                    "kind",
                    Json::number(match symbol.kind {
                        "function" | "host function" => 3, // Function
                        "struct" => 22,                    // Struct
                        "enum" => 13,                      // Enum
                        "trait" => 11,                     // Interface
                        _ => 7,                            // Class
                    }),
                ),
                ("detail", Json::str(symbol.label.clone())),
            ]));
        }
        Reply::result(Json::object(vec![
            ("isIncomplete", Json::Bool(false)),
            ("items", Json::Array(items)),
        ]))
    }

    fn symbols(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own_file = compiled
            .sources
            .iter()
            .find(|(_, name)| *name == path)
            .map(|(id, _)| id);
        let mut items = Vec::new();
        for symbol in &compiled.index.symbols {
            if Some(symbol.at.file) != own_file {
                continue;
            }
            items.push(Json::object(vec![
                ("name", Json::str(symbol.name.clone())),
                (
                    "kind",
                    Json::number(match symbol.kind {
                        "function" | "host function" => 12,
                        "struct" => 23,
                        "enum" => 10,
                        "trait" => 11,
                        _ => 5,
                    }),
                ),
                ("range", range_of(&text, symbol.at)),
                ("selectionRange", range_of(&text, symbol.at)),
            ]));
        }
        Reply::result(Json::Array(items))
    }

    fn formatting(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let formatted = kite_fmt::format(&text);
        if formatted == text {
            return Reply::result(Json::Array(Vec::new()));
        }
        // One edit replacing the whole file. The formatter rewrites layout
        // rather than lines, and a minimal diff would be a second formatter.
        let end = position_at(&text, text.len() as u32);
        Reply::result(Json::Array(vec![Json::object(vec![
            (
                "range",
                Json::object(vec![
                    (
                        "start",
                        Json::object(vec![
                            ("line", Json::number(0)),
                            ("character", Json::number(0)),
                        ]),
                    ),
                    ("end", end),
                ]),
            ),
            ("newText", Json::str(formatted)),
        ])]))
    }

    // ---- references, rename and inlay hints --------------------------------

    fn references(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let Some(offset) = position_of(message, &text) else {
            return Reply::result(Json::Null);
        };
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own = file_of(&compiled, &path);
        let Some(binding) = binding_at(&compiled.index.bindings, own, offset) else {
            return Reply::result(Json::Null);
        };
        let mut spans: Vec<Span> = Vec::new();
        if message.path("params.context.includeDeclaration") == Some(&Json::Bool(true))
            && Some(binding.declared_at.file) == own
        {
            spans.push(binding.declared_at);
        }
        // Mentions too: `Rect.square` is a place `Rect` is written, and a
        // reference listing exists to be complete, not to be editable.
        spans.extend(
            binding
                .uses
                .iter()
                .chain(binding.mentions.iter())
                .filter(|s| Some(s.file) == own),
        );
        spans.sort_by_key(|s| s.start);
        let items: Vec<Json> = spans
            .into_iter()
            .map(|s| {
                Json::object(vec![
                    ("uri", Json::str(uri.clone())),
                    ("range", range_of(&text, s)),
                ])
            })
            .collect();
        Reply::result(Json::Array(items))
    }

    fn prepare_rename(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let Some(offset) = position_of(message, &text) else {
            return Reply::refuse("nothing here can be renamed");
        };
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own = file_of(&compiled, &path);
        match renameable(&compiled, own, offset) {
            Err(why) => Reply::refuse(why),
            Ok(binding) => {
                // The occurrence under the cursor, so the editor selects
                // exactly what is about to change.
                let hit = std::iter::once(&binding.declared_at)
                    .chain(binding.uses.iter())
                    .find(|s| covers(s, own, offset))
                    .copied()
                    .unwrap_or(binding.declared_at);
                Reply::result(Json::object(vec![
                    ("range", range_of(&text, hit)),
                    ("placeholder", Json::str(binding.name.clone())),
                ]))
            }
        }
    }

    fn rename(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let Some(offset) = position_of(message, &text) else {
            return Reply::refuse("nothing here can be renamed");
        };
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own = file_of(&compiled, &path);
        let binding = match renameable(&compiled, own, offset) {
            Err(why) => return Reply::refuse(why),
            Ok(b) => b,
        };
        let new = message
            .path("params.newName")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(why) = bad_new_name(&new, binding, &compiled) {
            return Reply::refuse(why);
        }
        let mut spans = vec![binding.declared_at];
        spans.extend(binding.uses.iter().filter(|s| Some(s.file) == own));
        spans.sort_by_key(|s| s.start);
        let edits: Vec<Json> = spans
            .into_iter()
            .map(|s| {
                Json::object(vec![
                    ("range", range_of(&text, s)),
                    ("newText", Json::str(new.clone())),
                ])
            })
            .collect();
        let mut changes = std::collections::BTreeMap::new();
        changes.insert(uri, Json::Array(edits));
        Reply::result(Json::object(vec![("changes", Json::Object(changes))]))
    }

    fn inlay_hints(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        let path = path_of(&uri);
        let compiled = compile(&path, &text, Emit::Check);
        let own = file_of(&compiled, &path);
        let (from, to) = range_offsets(message, &text);
        let mut items = Vec::new();
        for hint in &compiled.index.hints {
            if Some(hint.after.file) != own {
                continue;
            }
            if hint.after.end < from || hint.after.start > to {
                continue;
            }
            let label = match hint.kind {
                // The type arguments a call inferred, where a language with a
                // turbofish would have let them be written.
                "generics" => format!("<{}>", hint.text),
                _ => format!(": {}", hint.text),
            };
            items.push(Json::object(vec![
                ("position", position_at(&text, hint.after.end)),
                ("label", Json::str(label)),
                // 1 is `Type` — both kinds of hint say what something is.
                ("kind", Json::number(1)),
            ]));
        }
        Reply::result(Json::Array(items))
    }
}

/// What this server can do, in the shape `initialize` expects.
fn capabilities() -> Json {
    Json::object(vec![
        (
            "capabilities",
            Json::object(vec![
                // Full synchronisation: the editor sends the whole file.
                ("textDocumentSync", Json::number(1)),
                ("hoverProvider", Json::Bool(true)),
                ("definitionProvider", Json::Bool(true)),
                ("documentSymbolProvider", Json::Bool(true)),
                ("documentFormattingProvider", Json::Bool(true)),
                ("referencesProvider", Json::Bool(true)),
                // `prepareProvider` is what lets the refusals reach the user
                // before they have typed a new name into a rename box.
                (
                    "renameProvider",
                    Json::object(vec![("prepareProvider", Json::Bool(true))]),
                ),
                ("inlayHintProvider", Json::Bool(true)),
                (
                    "completionProvider",
                    Json::object(vec![("triggerCharacters", Json::Array(vec![Json::str(".")]))]),
                ),
            ]),
        ),
        (
            "serverInfo",
            Json::object(vec![
                ("name", Json::str("kite-lsp")),
                ("version", Json::str(env!("CARGO_PKG_VERSION"))),
            ]),
        ),
    ])
}

fn uri_of(message: &Json) -> Option<String> {
    message
        .path("params.textDocument.uri")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

/// The file path a `file://` URI names.
///
/// The path matters: a module is a sibling file or directory, so a program's
/// own imports only resolve when the compiler is told where the file lives.
fn path_of(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    percent_decode(path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The byte offset a `{line, character}` position names.
///
/// The protocol counts UTF-16 code units and Kite counts bytes, so this walks
/// the line rather than adding the number on.
fn position_of(message: &Json, text: &str) -> Option<u32> {
    let line = message.path("params.position.line")?.as_u32()?;
    let character = message.path("params.position.character")?.as_u32()?;
    Some(offset_in(text, line, character))
}

fn offset_in(text: &str, line: u32, character: u32) -> u32 {
    let mut offset = 0usize;
    for (n, line_text) in text.split_inclusive('\n').enumerate() {
        if n as u32 == line {
            let mut units = 0u32;
            for (i, c) in line_text.char_indices() {
                if units >= character {
                    return (offset + i) as u32;
                }
                units += c.len_utf16() as u32;
            }
            return (offset + line_text.len()) as u32;
        }
        offset += line_text.len();
    }
    offset as u32
}

/// The byte range an inlay-hint request asks about. A missing or malformed
/// range means the whole file — answering everything beats answering nothing.
fn range_offsets(message: &Json, text: &str) -> (u32, u32) {
    let point = |which: &str| -> Option<u32> {
        let line = message.path(&format!("params.range.{}.line", which))?.as_u32()?;
        let character = message
            .path(&format!("params.range.{}.character", which))?
            .as_u32()?;
        Some(offset_in(text, line, character))
    };
    (point("start").unwrap_or(0), point("end").unwrap_or(text.len() as u32))
}

/// The [`FileId`] the open file was given, which is what tells its spans apart
/// from the prelude's and any module's.
fn file_of(compiled: &Compilation, path: &str) -> Option<FileId> {
    compiled
        .sources
        .iter()
        .find(|(_, name)| *name == path)
        .map(|(id, _)| id)
}

/// Whether a span in the open file contains the cursor. An empty span still
/// answers to the offset it sits on, as everywhere else in this server.
fn covers(span: &Span, own: Option<FileId>, offset: u32) -> bool {
    Some(span.file) == own && span.start <= offset && offset < span.end.max(span.start + 1)
}

/// The binding under the cursor: its declaration or one of its uses first, and
/// only failing those a longer span that merely mentions it.
fn binding_at(bindings: &[Binding], own: Option<FileId>, offset: u32) -> Option<&Binding> {
    bindings
        .iter()
        .find(|b| covers(&b.declared_at, own, offset) || b.uses.iter().any(|s| covers(s, own, offset)))
        .or_else(|| {
            bindings
                .iter()
                .find(|b| b.mentions.iter().any(|s| covers(s, own, offset)))
        })
}

/// The binding the cursor may rename, or the reason it may not.
///
/// The rule is the table's own coverage: a rename starts only when every
/// occurrence is recorded and every recorded occurrence is editable. That
/// admits locals and this file's own functions — and refuses keywords and
/// literals (no binding), prelude and module names (declared elsewhere),
/// types (annotations are not in the table), methods (call sites need the
/// receiver's type), and host functions (the name is the host's contract).
fn renameable(
    compiled: &Compilation,
    own: Option<FileId>,
    offset: u32,
) -> Result<&Binding, String> {
    let Some(binding) = binding_at(&compiled.index.bindings, own, offset) else {
        return Err("nothing renameable here — only a declared name has uses to rename".to_string());
    };
    if Some(binding.declared_at.file) != own {
        let from = compiled
            .sources
            .iter()
            .find(|(id, _)| *id == binding.declared_at.file)
            .map(|(_, name)| name.to_string())
            .unwrap_or_default();
        return Err(if from == "<prelude>" {
            format!("`{}` comes from the prelude, not from this file", binding.name)
        } else {
            format!("`{}` is declared in `{}`, not in this file", binding.name, from)
        });
    }
    match binding.kind {
        "local" | "function" => {}
        "host function" => {
            return Err(format!(
                "`{}` is a host function — its name is the contract with the host, which an \
                 edit here cannot update",
                binding.name
            ));
        }
        kind => {
            return Err(format!(
                "renaming a {} is not supported: a type's name also appears in annotations, \
                 which the binding table does not record",
                kind
            ));
        }
    }
    if !binding.mentions.is_empty() {
        return Err(format!(
            "`{}` is also written inside a longer name — a qualified path or a shorthand \
             field — which a rename cannot safely rewrite",
            binding.name
        ));
    }
    Ok(binding)
}

/// Why `new` may not replace `binding`'s name, or nothing when it may.
fn bad_new_name(new: &str, binding: &Binding, compiled: &Compilation) -> Option<String> {
    // The lexer is the authority on what an identifier is: exactly one token,
    // of the right kind, consuming the whole candidate. Anything else — a
    // keyword, a literal, two words, trailing space — is refused here rather
    // than written into the file and reported by the next compile.
    if kite_lexer::keyword(new).is_some() {
        return Some(format!("`{}` is a keyword", new));
    }
    let mut scratch = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(FileId(0), new, &mut scratch);
    let one_identifier = tokens.len() == 2
        && tokens[0].kind == kite_lexer::TokenKind::Ident
        && tokens[0].span.start == 0
        && tokens[0].span.end == new.len() as u32
        && !scratch.has_errors();
    if !one_identifier {
        return Some(format!("`{}` is not a Kite identifier", new));
    }
    // Already bound where it would be visible. Locals collide only within
    // their own scope; anything top-level — this file's, a module's, the
    // prelude's — is visible everywhere, so it collides with everything.
    // Conservative on purpose: a refusal costs a second attempt, and a rename
    // that quietly changed what other names resolve to costs a debugging
    // session.
    let clash = compiled.index.bindings.iter().any(|other| {
        other.name == new
            && other.declared_at != binding.declared_at
            && match (&binding.scope, &other.scope) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    });
    if clash {
        return Some(format!(
            "`{}` is already bound where `{}` is visible; the rename would change what \
             other names mean",
            new, binding.name
        ));
    }
    // The head of a dotted name is how a module is reached, and a local wins
    // over it — so a binding taking a module's name would capture every
    // `io.print` in its scope.
    let shadows_module = compiled.index.uses.iter().any(|u| {
        compiled
            .sources
            .text(u.at.file)
            .get(u.at.start as usize..u.at.end as usize)
            .and_then(|t| t.contains('.').then(|| t.split('.').next()).flatten())
            == Some(new)
    });
    if shadows_module {
        return Some(format!(
            "`{}` is how a module is reached here, and a binding of that name would \
             shadow it",
            new
        ));
    }
    None
}

/// The `{line, character}` a byte offset lands on.
///
/// Clamped to the end of the text and then walked back to a character
/// boundary. Clamping alone left the second way a `str` index panics: an
/// offset inside a multi-byte character is in range and still not a place the
/// text can be cut. A panic here is not a failed request but the end of the
/// session.
fn position_at(text: &str, offset: u32) -> Json {
    let mut offset = (offset as usize).min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character: usize = text[line_start..offset].chars().map(|c| c.len_utf16()).sum();
    Json::object(vec![
        ("line", Json::number(line as f64)),
        ("character", Json::number(character as f64)),
    ])
}

fn range_of(text: &str, span: Span) -> Json {
    Json::object(vec![
        ("start", position_at(text, span.start)),
        ("end", position_at(text, span.end.max(span.start))),
    ])
}
