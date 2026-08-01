//! The protocol: what the editor asks, and what this answers.
//!
//! Every answer comes from the same passes `kitec` runs. A language server
//! that re-derives its own answers is a second compiler that disagrees with
//! the first one, and the disagreement always surfaces as "the editor says
//! this is fine and the build says it is not".

use crate::json::Json;
use kite_diag::Severity;
use kite_driver::{compile, Emit};
use kite_span::Span;
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
    pub notifications: Vec<(String, Json)>,
}

impl Reply {
    fn none() -> Reply {
        Reply { result: None, notifications: Vec::new() }
    }

    fn result(value: Json) -> Reply {
        Reply { result: Some(value), notifications: Vec::new() }
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
        let compiled = compile(&path_of(&uri), &text, Emit::Check);
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
        let compiled = compile(&path_of(&uri), &text, Emit::Check);
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
    let mut offset = 0usize;
    for (n, line_text) in text.split_inclusive('\n').enumerate() {
        if n as u32 == line {
            let mut units = 0u32;
            for (i, c) in line_text.char_indices() {
                if units >= character {
                    return Some((offset + i) as u32);
                }
                units += c.len_utf16() as u32;
            }
            return Some((offset + line_text.len()) as u32);
        }
        offset += line_text.len();
    }
    Some(offset as u32)
}

/// The `{line, character}` a byte offset lands on.
fn position_at(text: &str, offset: u32) -> Json {
    let offset = (offset as usize).min(text.len());
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
