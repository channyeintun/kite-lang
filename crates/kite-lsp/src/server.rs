//! The protocol: what the editor asks, and what this answers.
//!
//! Every answer comes from the same passes `kitec` runs. A language server
//! that re-derives its own answers is a second compiler that disagrees with
//! the first one, and the disagreement always surfaces as "the editor says
//! this is fine and the build says it is not".

use crate::json::Json;
use kite_diag::Severity;
use kite_driver::modules::{located, normalise, Files};
use kite_driver::{compile_files, Binding, Compilation, Emit};
use kite_span::{FileId, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Files the editor has open, by URI. The editor's copy is the truth while a
/// file is open — it may hold edits that are not on disk yet.
#[derive(Default)]
pub struct Server {
    open: HashMap<String, String>,
    pub shutdown: bool,
}

/// One open file, compiled the way `kitec check` would compile it.
struct Compiled {
    uri: String,
    path: String,
    text: String,
    compilation: Compilation,
    /// The [`FileId`] this file was given, which is what tells its spans apart
    /// from the prelude's and every module's. Each of those numbers its bytes
    /// from zero too, so an offset means nothing until its file is checked.
    own: Option<FileId>,
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

    fn notify(notifications: Vec<(String, Json)>) -> Reply {
        Reply { result: None, error: None, notifications }
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
                self.republish(&uri)
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
                self.republish(&uri)
            }
            "textDocument/didSave" => {
                // The buffer was already the truth, so saving it changes
                // nothing another file sees.
                let uri = uri_of(message).unwrap_or_default();
                Reply::notify(vec![self.diagnostics(&uri)])
            }
            "textDocument/didClose" => {
                let uri = uri_of(message).unwrap_or_default();
                self.open.remove(&uri);
                // An empty list clears what was shown for the file. The files
                // that import it read it from disk again, and the disk may not
                // hold what the buffer that just went away did.
                let mut notifications = vec![(
                    "textDocument/publishDiagnostics".to_string(),
                    Json::object(vec![
                        ("uri", Json::str(uri.clone())),
                        ("diagnostics", Json::Array(Vec::new())),
                    ]),
                )];
                for other in self.importers(&uri) {
                    notifications.push(self.diagnostics(&other));
                }
                Reply::notify(notifications)
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

    /// Compile an open file the way `kitec check` would if every open buffer
    /// were saved.
    ///
    /// Reading the others from disk meant an edit to `config.kite` was
    /// invisible to `main.kite` until it was saved: the editor reported a
    /// function missing while showing it on screen.
    fn compile(&self, uri: &str) -> Compiled {
        let text = self.text(uri);
        let path = path_of(uri);
        let compilation = compile_files(&path, &text, Emit::Check, false, self.edited());
        let own = file_of(&compilation, &path);
        Compiled { uri: uri.to_string(), path, text, compilation, own }
    }

    /// The disk as the editor sees it: each open buffer in place of the file
    /// it is a buffer of, and of nothing else.
    ///
    /// The buffers used to be handed over as provided modules, keyed by their
    /// path below the file being compiled. A provided key is consulted before
    /// anything on disk, so opening `md.kite` took `use md` from the declared
    /// dependency of that name, opening `config.kite` hid the directory
    /// module `config/`, a nested module's `use x/y` was answered by the
    /// entry's `x/y.kite`, and a dependency's own `use util` by the
    /// application's `md/util.kite`. Opening a file — changing nothing in it
    /// — changed what the program meant. Now every `use` resolves as it does
    /// on disk, and the file it finds is read from its buffer when there is
    /// one. The file being compiled is among them, so a module that imports
    /// it back reads what is on screen.
    fn edited(&self) -> Files {
        Files::edited(
            self.open
                .iter()
                .filter(|(uri, _)| uri.starts_with("file://"))
                .map(|(uri, text)| (PathBuf::from(path_of(uri)), text.clone())),
        )
    }

    /// The other open files that could import `uri`: those in its directory or
    /// above it, which is everywhere a `use` path reaches it from.
    fn importers(&self, uri: &str) -> Vec<String> {
        if !uri.starts_with("file://") {
            return Vec::new();
        }
        let path = path_of(uri);
        let mut found: Vec<String> = self
            .open
            .keys()
            .filter(|other| *other != uri && other.starts_with("file://"))
            .filter(|other| {
                let other = path_of(other);
                Path::new(&other).parent().is_some_and(|dir| {
                    !dir.as_os_str().is_empty() && Path::new(&path).starts_with(dir)
                })
            })
            .cloned()
            .collect();
        // In a stable order, so two runs publish the same sequence.
        found.sort();
        found
    }

    // ---- diagnostics -------------------------------------------------------

    /// This file's diagnostics, and those of every other open file that could
    /// import it — an edit here can break, or mend, a file the editor also
    /// shows.
    fn republish(&self, uri: &str) -> Reply {
        let mut notifications = vec![self.diagnostics(uri)];
        for other in self.importers(uri) {
            notifications.push(self.diagnostics(&other));
        }
        Reply::notify(notifications)
    }

    fn diagnostics(&self, uri: &str) -> (String, Json) {
        let c = self.compile(uri);
        let mut items = Vec::new();
        for d in c.compilation.diags.iter() {
            let Some(span) = d.primary_span() else { continue };
            // Only what is in this file: a diagnostic pointing into the
            // standard library is not something the editor can show against a
            // line the user has open.
            if Some(span.file) != c.own {
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
                ("range", range_of(&c.text, span)),
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
        (
            "textDocument/publishDiagnostics".to_string(),
            Json::object(vec![
                ("uri", Json::str(uri.to_string())),
                ("diagnostics", Json::Array(items)),
            ]),
        )
    }

    // ---- hover and definition ----------------------------------------------

    fn hover(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let Some(offset) = position_of(message, &self.text(&uri)) else {
            return Reply::result(Json::Null);
        };
        let c = self.compile(&uri);
        // Only this file's uses. Every file numbers its bytes from zero, so a
        // use in the prelude sits at the same offsets as one here — and the
        // prelude's are recorded first.
        let Some(found) = c.compilation.index.uses.iter().find(|u| covers(&u.at, c.own, offset))
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
        let Some(offset) = position_of(message, &self.text(&uri)) else {
            return Reply::result(Json::Null);
        };
        let c = self.compile(&uri);
        let index = &c.compilation.index;
        // The binding table first: it knows where a local was declared, and
        // the use index only knows where a local was used.
        let target = named_at(&index.bindings, c.own, offset)
            .map(|b| b.declared_at)
            .or_else(|| {
                index
                    .uses
                    .iter()
                    .find(|u| covers(&u.at, c.own, offset))
                    .map(|u| u.declared_at)
            })
            .or_else(|| binding_at(&index.bindings, c.own, offset).map(|b| b.declared_at));
        match target {
            Some(span) => Reply::result(self.location(&c, span)),
            None => Reply::result(Json::Null),
        }
    }

    /// Where a span is, as the editor names places.
    ///
    /// A span in one of the program's own modules is in a file the editor can
    /// open. One in the prelude or the standard library is not — they are
    /// compiled into the binary — so that is answered with nothing.
    fn location(&self, c: &Compiled, span: Span) -> Json {
        if Some(span.file) == c.own {
            return Json::object(vec![
                ("uri", Json::str(c.uri.clone())),
                ("range", range_of(&c.text, span)),
            ]);
        }
        let name = c.compilation.sources.file(span.file).name.clone();
        if name.to_string_lossy().starts_with('<') {
            return Json::Null;
        }
        // A module is named by the path it was read from, which is joined
        // rather than resolved: a path dependency's file is
        // `app/../lib/md/md.kite`. Folded, so the URI is one an editor opens
        // as the file it is rather than as a second tab.
        let path = match Path::new(&c.path).parent() {
            Some(dir) if name.is_relative() && Path::new(&c.path).is_absolute() => dir.join(&name),
            _ => name,
        };
        let mut shown = normalise(&path).to_string_lossy().to_string();
        if cfg!(windows) {
            shown = shown.replace('/', "\\");
        }
        // The editor's own spelling of the URI when the file is open, so the
        // answer lands in the buffer it already has. Compared by where each
        // file really is, which is also how the buffer was matched to it.
        let here = located(&path);
        let uri = self
            .open
            .keys()
            .find(|u| u.starts_with("file://") && located(Path::new(&path_of(u))) == here)
            .cloned()
            .unwrap_or_else(|| uri_of_path(&shown));
        Json::object(vec![
            ("uri", Json::str(uri)),
            ("range", range_of(c.compilation.sources.text(span.file), span)),
        ])
    }

    // ---- completion and symbols --------------------------------------------

    fn completion(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let c = self.compile(&uri);
        let mut items = Vec::new();
        for keyword in kite_lexer::KEYWORDS {
            items.push(Json::object(vec![
                ("label", Json::str(keyword)),
                ("kind", Json::number(14)), // Keyword
            ]));
        }
        for symbol in &c.compilation.index.symbols {
            // Another module's private item — the prelude's included — is not
            // a name this file can write, and offering it offers an error.
            if Some(symbol.at.file) != c.own && !symbol.is_pub {
                continue;
            }
            items.push(Json::object(vec![
                ("label", Json::str(symbol.name.clone())),
                (
                    "kind",
                    Json::number(match symbol.kind {
                        "function" | "host function" => 3, // Function
                        "struct" => 22,                    // Struct
                        "enum" => 13,                      // Enum
                        "trait" => 8,                      // Interface
                        "constant" => 21,                  // Constant
                        // The protocol has no kind for a type alias.
                        _ => 7, // Class
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
        let c = self.compile(&uri);
        let mut items = Vec::new();
        for symbol in &c.compilation.index.symbols {
            if Some(symbol.at.file) != c.own {
                continue;
            }
            items.push(Json::object(vec![
                ("name", Json::str(symbol.name.clone())),
                (
                    "kind",
                    Json::number(match symbol.kind {
                        "function" | "host function" => 12, // Function
                        "struct" => 23,                     // Struct
                        "enum" => 10,                       // Enum
                        "trait" => 11,                      // Interface
                        "constant" => 14,                   // Constant
                        // The protocol has no kind for a type alias.
                        _ => 5, // Class
                    }),
                ),
                ("range", range_of(&c.text, symbol.at)),
                ("selectionRange", range_of(&c.text, symbol.at)),
            ]));
        }
        Reply::result(Json::Array(items))
    }

    fn formatting(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let text = self.text(&uri);
        // A document the lexer cannot read gets no edits: the formatter
        // refuses it rather than lay out what survived, and the diagnostics
        // already say what is wrong.
        let Ok(formatted) = kite_fmt::format(&text) else {
            return Reply::result(Json::Array(Vec::new()));
        };
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
        let Some(offset) = position_of(message, &self.text(&uri)) else {
            return Reply::result(Json::Null);
        };
        let c = self.compile(&uri);
        let own = c.own;
        let Some(binding) = binding_at(&c.compilation.index.bindings, own, offset) else {
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
                    ("range", range_of(&c.text, s)),
                ])
            })
            .collect();
        Reply::result(Json::Array(items))
    }

    fn prepare_rename(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let Some(offset) = position_of(message, &self.text(&uri)) else {
            return Reply::refuse("nothing here can be renamed");
        };
        let c = self.compile(&uri);
        match renameable(&c.compilation, c.own, offset) {
            Err(why) => Reply::refuse(why),
            Ok(binding) => {
                // The occurrence under the cursor, so the editor selects
                // exactly what is about to change.
                let hit = std::iter::once(&binding.declared_at)
                    .chain(binding.uses.iter())
                    .find(|s| covers(s, c.own, offset))
                    .copied()
                    .unwrap_or(binding.declared_at);
                Reply::result(Json::object(vec![
                    ("range", range_of(&c.text, hit)),
                    ("placeholder", Json::str(binding.name.clone())),
                ]))
            }
        }
    }

    fn rename(&self, message: &Json) -> Reply {
        let uri = uri_of(message).unwrap_or_default();
        let Some(offset) = position_of(message, &self.text(&uri)) else {
            return Reply::refuse("nothing here can be renamed");
        };
        let c = self.compile(&uri);
        let own = c.own;
        let binding = match renameable(&c.compilation, own, offset) {
            Err(why) => return Reply::refuse(why),
            Ok(b) => b,
        };
        // In NFC, which is how §2.1 compares identifiers and how every name
        // the compiler read is held. Compared as typed, `café` with a
        // combining accent was no clash with the `café` already bound, and the
        // rename that went through changed what the old uses meant.
        let new = kite_driver::identifier_nfc(
            message.path("params.newName").and_then(|n| n.as_str()).unwrap_or(""),
        );
        if let Some(why) = bad_new_name(&new, binding, &c.compilation) {
            return Reply::refuse(why);
        }
        let mut spans = vec![binding.declared_at];
        spans.extend(binding.uses.iter().filter(|s| Some(s.file) == own));
        spans.sort_by_key(|s| s.start);
        let edits: Vec<Json> = spans
            .into_iter()
            .map(|s| {
                Json::object(vec![
                    ("range", range_of(&c.text, s)),
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
        let c = self.compile(&uri);
        let (from, to) = range_offsets(message, &c.text);
        let mut items = Vec::new();
        for hint in &c.compilation.index.hints {
            if Some(hint.after.file) != c.own {
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
                ("position", position_at(&c.text, hint.after.end)),
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
    path_from_uri(uri, cfg!(windows))
}

/// [`path_of`], for either kind of system, so both can be tested on one.
///
/// A Windows editor sends `file:///c%3A/Users/…`: the slash in front of the
/// drive letter belongs to the URI, and `/c:/Users/…` is not a path Windows
/// can open. A URI with a host is a UNC share, `\\host\share\…`.
fn path_from_uri(uri: &str, windows: bool) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return percent_decode(uri);
    };
    let (host, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let path = percent_decode(path);
    let native = |p: String| if windows { p.replace('/', "\\") } else { p };
    if !host.is_empty() && host != "localhost" {
        return native(format!("//{}{}", percent_decode(host), path));
    }
    let bytes = path.as_bytes();
    if windows && bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':'
    {
        return native(path[1..].to_string());
    }
    native(path)
}

/// The `file://` URI for a path: the way back from [`path_of`].
pub(crate) fn uri_of_path(path: &str) -> String {
    uri_from_path(path, cfg!(windows))
}

fn uri_from_path(path: &str, windows: bool) -> String {
    let path = if windows { path.replace('\\', "/") } else { path.to_string() };
    if windows {
        // `\\host\share\…`: the host is the URI's authority.
        if let Some(unc) = path.strip_prefix("//") {
            let (host, rest) = match unc.find('/') {
                Some(at) => (&unc[..at], &unc[at..]),
                None => (unc, ""),
            };
            return format!("file://{}{}", host, percent_encode(rest));
        }
        let bytes = path.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return format!("file:///{}", percent_encode(&path));
        }
    }
    format!("file://{}", percent_encode(&path))
}

/// Everything but the unreserved characters and the separator, escaped — which
/// is how editors write a path into a URI, drive colon included.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
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

/// The binding whose own name is under the cursor: its declaration or one of
/// its uses.
fn named_at(bindings: &[Binding], own: Option<FileId>, offset: u32) -> Option<&Binding> {
    bindings
        .iter()
        .find(|b| covers(&b.declared_at, own, offset) || b.uses.iter().any(|s| covers(s, own, offset)))
}

/// The binding under the cursor: its declaration or one of its uses first, and
/// only failing those a longer span that merely mentions it.
fn binding_at(bindings: &[Binding], own: Option<FileId>, offset: u32) -> Option<&Binding> {
    named_at(bindings, own, offset).or_else(|| {
        bindings
            .iter()
            .find(|b| b.mentions.iter().any(|s| covers(s, own, offset)))
    })
}

/// The binding the cursor may rename, or the reason it may not.
///
/// The rule is the table's own coverage: a rename starts only when every
/// occurrence is recorded and every recorded occurrence is editable. That
/// admits locals, and this file's own private functions and constants — and
/// refuses keywords and literals (no binding), prelude and module names
/// (declared elsewhere), `pub` names (their importers' uses are in other
/// files), types (annotations are not in the table), methods (call sites need
/// the receiver's type), host functions (the name is the host's contract), and
/// anything at all in a file that did not parse (what was skipped is in no
/// table).
fn renameable(
    compiled: &Compilation,
    own: Option<FileId>,
    offset: u32,
) -> Result<&Binding, String> {
    // What the parser skipped to recover was never resolved, so an occurrence
    // there is in no table: renaming `count` while `io.print(count +)` did not
    // parse left that `count` behind, to resolve to something else — or to
    // nothing — once the line was mended.
    let unparsed = compiled.diags.iter().any(|d| {
        d.severity == Severity::Error
            && d.code.is_some_and(|c| is_syntax(c.0))
            && d.primary_span().is_some_and(|s| Some(s.file) == own)
    });
    if unparsed {
        return Err(
            "this file has syntax errors, and a name in code that did not parse is in no table \
             a rename could edit — fix them first"
                .to_string(),
        );
    }
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
        "local" | "function" | "constant" => {}
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
    // A `pub` name is for other modules, and their `config.port` is in *their*
    // tables, not this file's — so the rename edited the declaration alone,
    // and every importer stopped compiling. Which files import this one is
    // not something an open editor knows: an importer need not be open.
    let exported = compiled.index.symbols.iter().any(|s| s.at == binding.declared_at && s.is_pub);
    if exported {
        return Err(format!(
            "`{}` is `pub`, so other modules may use it, and their uses are not in this file — \
             a rename here would edit the declaration and leave them behind",
            binding.name
        ));
    }
    Ok(binding)
}

/// Whether a diagnostic code is the lexer's or the parser's: something wrong
/// with the text itself, which the parser recovers from by skipping ahead.
fn is_syntax(code: &str) -> bool {
    code.starts_with("E00") || matches!(code, "E0100" | "E0101" | "E0102")
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
            .is_some_and(|head| kite_driver::identifier_nfc(head) == new)
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

#[cfg(test)]
mod uri_tests {
    use super::{path_from_uri, uri_from_path};

    #[test]
    fn a_unix_uri_is_its_path() {
        assert_eq!(path_from_uri("file:///home/me/a%20b.kite", false), "/home/me/a b.kite");
        assert_eq!(uri_from_path("/home/me/a b.kite", false), "file:///home/me/a%20b.kite");
    }

    /// VS Code on Windows escapes the drive colon, and the slash in front of
    /// the drive letter is the URI's, not the path's.
    #[test]
    fn a_windows_drive_loses_the_slash_in_front_of_it() {
        assert_eq!(
            path_from_uri("file:///c%3A/Users/me/main.kite", true),
            "c:\\Users\\me\\main.kite"
        );
        assert_eq!(path_from_uri("file:///C:/src/x.kite", true), "C:\\src\\x.kite");
        assert_eq!(
            uri_from_path("c:\\Users\\me\\main.kite", true),
            "file:///c%3A/Users/me/main.kite"
        );
    }

    /// A URI with a host is a share, and the share is the path's first part.
    #[test]
    fn a_windows_share_is_a_unc_path() {
        assert_eq!(
            path_from_uri("file://server/share/app/main.kite", true),
            "\\\\server\\share\\app\\main.kite"
        );
        assert_eq!(
            uri_from_path("\\\\server\\share\\app\\main.kite", true),
            "file://server/share/app/main.kite"
        );
        // `localhost` is this machine, which is no host at all.
        assert_eq!(path_from_uri("file://localhost/tmp/x.kite", false), "/tmp/x.kite");
    }

    #[test]
    fn a_path_survives_the_round_trip() {
        for (path, windows) in [
            ("/tmp/dir with space/ü.kite", false),
            ("d:\\work\\#1\\main.kite", true),
            ("\\\\nas\\kite\\main.kite", true),
        ] {
            assert_eq!(path_from_uri(&uri_from_path(path, windows), windows), path);
        }
    }
}
