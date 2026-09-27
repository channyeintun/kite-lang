use crate::json::{parse, Json};
use crate::server::{uri_of_path, Server};

fn open(server: &mut Server, uri: &str, text: &str) -> Vec<Json> {
    let message = Json::object(vec![
        ("method", Json::str("textDocument/didOpen")),
        (
            "params",
            Json::object(vec![(
                "textDocument",
                Json::object(vec![("uri", Json::str(uri)), ("text", Json::str(text))]),
            )]),
        ),
    ]);
    let reply = server.handle("textDocument/didOpen", &message);
    reply
        .notifications
        .into_iter()
        .map(|(_, params)| params)
        .collect()
}

fn at(uri: &str, line: u32, character: u32) -> Json {
    Json::object(vec![(
        "params",
        Json::object(vec![
            ("textDocument", Json::object(vec![("uri", Json::str(uri))])),
            (
                "position",
                Json::object(vec![
                    ("line", Json::number(line)),
                    ("character", Json::number(character)),
                ]),
            ),
        ]),
    )])
}

#[test]
fn initialize_reports_what_it_can_do() {
    let mut s = Server::new();
    let reply = s.handle("initialize", &Json::Null);
    let result = reply.result.expect("an answer");
    assert_eq!(result.path("capabilities.hoverProvider"), Some(&Json::Bool(true)));
    assert_eq!(
        result.path("capabilities.definitionProvider"),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        result.path("capabilities.referencesProvider"),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        result.path("capabilities.renameProvider.prepareProvider"),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        result.path("capabilities.inlayHintProvider"),
        Some(&Json::Bool(true))
    );
    assert_eq!(
        result.path("serverInfo.name").and_then(|n| n.as_str()),
        Some("kite-lsp")
    );
}

#[test]
fn opening_a_broken_file_publishes_its_diagnostics() {
    let mut s = Server::new();
    let published = open(&mut s, "file:///t.kite", "fn main() {\n    let x: int = \"s\"\n}\n");
    assert_eq!(published.len(), 1);
    let Some(Json::Array(items)) = published[0].get("diagnostics") else {
        panic!("no diagnostics array");
    };
    assert_eq!(items.len(), 1, "{:?}", items);
    assert_eq!(items[0].get("code").and_then(|c| c.as_str()), Some("E0200"));
    // Line 1, where the mistake is — not line 0, and not a line in the prelude.
    assert_eq!(items[0].path("range.start.line").and_then(|l| l.as_u32()), Some(1));
    assert_eq!(items[0].get("source").and_then(|s| s.as_str()), Some("kite"));
}

#[test]
fn a_file_with_nothing_wrong_publishes_an_empty_list() {
    let mut s = Server::new();
    let published = open(&mut s, "file:///t.kite", "fn main() {\n    io.print(1)\n}\n");
    assert_eq!(published[0].get("diagnostics"), Some(&Json::Array(Vec::new())));
}

/// A diagnostic pointing into the standard library is not something the editor
/// can show against a line the user has open.
#[test]
fn only_this_files_diagnostics_are_published() {
    let mut s = Server::new();
    let published = open(&mut s, "file:///t.kite", "fn main() {\n    nope()\n}\n");
    let Some(Json::Array(items)) = published[0].get("diagnostics") else {
        panic!("no diagnostics");
    };
    assert!(items.iter().all(|d| d.path("range.start.line").is_some()));
}

#[test]
fn hovering_a_call_shows_its_signature() {
    let mut s = Server::new();
    let text = "fn add(a: int, b: int) -> int {\n    return a + b\n}\nfn main() {\n    io.print(add(1, 2))\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `add` on the last line.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 4, 13));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("fn add(a: int, b: int) -> int"), "{}", value);
}

#[test]
fn hovering_nothing_answers_null() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "fn main() {\n}\n");
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 1, 0));
    assert_eq!(reply.result, Some(Json::Null));
}

#[test]
fn go_to_definition_finds_the_declaration() {
    let mut s = Server::new();
    let text = "fn add(a: int, b: int) -> int {\n    return a + b\n}\nfn main() {\n    io.print(add(1, 2))\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/definition", &at("file:///t.kite", 4, 13));
    let result = reply.result.expect("an answer");
    assert_eq!(result.path("range.start.line").and_then(|l| l.as_u32()), Some(0));
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some("file:///t.kite"));
}

#[test]
fn completion_offers_keywords_and_the_files_own_names() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "struct Point {\n    x: int\n}\nfn main() {\n}\n");
    let reply = s.handle("textDocument/completion", &at("file:///t.kite", 4, 0));
    let Some(Json::Array(items)) = reply.result.as_ref().and_then(|r| r.get("items")) else {
        panic!("no items");
    };
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"match"), "{:?}", labels);
    assert!(labels.contains(&"Point"), "{:?}", labels);
    assert!(labels.contains(&"main"), "{:?}", labels);
    // The prelude is in scope everywhere, so its names are offered too.
    assert!(labels.contains(&"filter"), "{:?}", labels);
}

#[test]
fn document_symbols_list_this_files_declarations() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "struct Point {\n    x: int\n}\nfn main() {\n}\n");
    let reply = s.handle("textDocument/documentSymbol", &at("file:///t.kite", 0, 0));
    let Some(Json::Array(items)) = reply.result else {
        panic!("no symbols");
    };
    let names: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("name").and_then(|n| n.as_str()))
        .collect();
    assert_eq!(names, vec!["Point", "main"], "{:?}", names);
}

#[test]
fn formatting_replaces_the_whole_file_when_it_changes() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "fn main() {\nio.print(1)\n}\n");
    let reply = s.handle("textDocument/formatting", &at("file:///t.kite", 0, 0));
    let Some(Json::Array(edits)) = reply.result else {
        panic!("no edits");
    };
    assert_eq!(edits.len(), 1);
    let text = edits[0].get("newText").and_then(|t| t.as_str()).unwrap();
    assert_eq!(text, "fn main() {\n    io.print(1)\n}\n");
}

#[test]
fn formatting_an_already_formatted_file_changes_nothing() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "fn main() {\n    io.print(1)\n}\n");
    let reply = s.handle("textDocument/formatting", &at("file:///t.kite", 0, 0));
    assert_eq!(reply.result, Some(Json::Array(Vec::new())));
}

/// Formatting a document the lexer cannot read used to lay out whatever
/// tokens survived — `a ?? b` came back as `a b`, and an unterminated `/*`
/// took the rest of the file with it. A format-on-save editor applied that.
#[test]
fn formatting_a_document_with_lexical_errors_changes_nothing() {
    let mut s = Server::new();
    for text in ["fn main() {\nlet x = a ?? b\n}\n", "fn main() {\nlet x = 1 /* oops\nio.print(x)\n}\n"] {
        open(&mut s, "file:///t.kite", text);
        let reply = s.handle("textDocument/formatting", &at("file:///t.kite", 0, 0));
        assert_eq!(reply.result, Some(Json::Array(Vec::new())), "{:?}", text);
    }
}

#[test]
fn a_change_republishes_diagnostics() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "fn main() {\n}\n");
    let message = parse(
        r#"{"method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///t.kite"},"contentChanges":[{"text":"fn main() {\n  let x: int = true\n}\n"}]}}"#,
    )
    .expect("parses");
    let reply = s.handle("textDocument/didChange", &message);
    let Some(Json::Array(items)) = reply.notifications[0].1.get("diagnostics") else {
        panic!("no diagnostics");
    };
    assert_eq!(items.len(), 1, "{:?}", items);
}

#[test]
fn closing_a_file_clears_what_was_shown() {
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", "fn main() {\n    let x: int = true\n}\n");
    let reply = s.handle("textDocument/didClose", &at("file:///t.kite", 0, 0));
    assert_eq!(
        reply.notifications[0].1.get("diagnostics"),
        Some(&Json::Array(Vec::new()))
    );
}

#[test]
fn shutdown_is_answered_and_recorded() {
    let mut s = Server::new();
    let reply = s.handle("shutdown", &Json::Null);
    assert_eq!(reply.result, Some(Json::Null));
    assert!(s.shutdown);
}

/// A method this server does not implement is answered rather than refused:
/// editors ask about capabilities they were not told about.
#[test]
fn an_unknown_method_is_answered_emptily() {
    let mut s = Server::new();
    let reply = s.handle("textDocument/codeLens", &Json::Null);
    assert_eq!(reply.result, Some(Json::Null));
}

fn rename_at(uri: &str, line: u32, character: u32, new_name: &str) -> Json {
    Json::object(vec![(
        "params",
        Json::object(vec![
            ("textDocument", Json::object(vec![("uri", Json::str(uri))])),
            (
                "position",
                Json::object(vec![
                    ("line", Json::number(line)),
                    ("character", Json::number(character)),
                ]),
            ),
            ("newName", Json::str(new_name)),
        ]),
    )])
}

fn references_at(uri: &str, line: u32, character: u32, include_declaration: bool) -> Json {
    Json::object(vec![(
        "params",
        Json::object(vec![
            ("textDocument", Json::object(vec![("uri", Json::str(uri))])),
            (
                "position",
                Json::object(vec![
                    ("line", Json::number(line)),
                    ("character", Json::number(character)),
                ]),
            ),
            (
                "context",
                Json::object(vec![("includeDeclaration", Json::Bool(include_declaration))]),
            ),
        ]),
    )])
}

fn hints_over(uri: &str) -> Json {
    Json::object(vec![(
        "params",
        Json::object(vec![
            ("textDocument", Json::object(vec![("uri", Json::str(uri))])),
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
                    (
                        "end",
                        Json::object(vec![
                            ("line", Json::number(99)),
                            ("character", Json::number(0)),
                        ]),
                    ),
                ]),
            ),
        ]),
    )])
}

#[test]
fn rename_updates_the_declaration_and_every_use() {
    let mut s = Server::new();
    let text =
        "fn main() {\n    let count = 1\n    io.print(count)\n    io.print(count + 1)\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `count` on its declaration line.
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 8, "total"));
    assert_eq!(reply.error, None);
    let result = reply.result.expect("an answer");
    let Some(Json::Array(edits)) = result.get("changes").and_then(|c| c.get("file:///t.kite"))
    else {
        panic!("no edits for the file");
    };
    // The declaration and both uses, each replaced with the new name.
    assert_eq!(edits.len(), 3, "{:?}", edits);
    for edit in edits {
        assert_eq!(edit.get("newText").and_then(|t| t.as_str()), Some("total"));
    }
    assert_eq!(edits[0].path("range.start.line").and_then(|l| l.as_u32()), Some(1));
    assert_eq!(edits[2].path("range.start.line").and_then(|l| l.as_u32()), Some(3));
}

/// A prelude name is declared in the prelude, and an edit to this file cannot
/// reach it — so the rename is refused with the reason, not half-done.
#[test]
fn rename_refuses_a_prelude_name() {
    let mut s = Server::new();
    let text = "fn main() {\n    let x = first([1, 2])\n    io.print(1)\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `first` on line 1.
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 13, "head"));
    assert_eq!(reply.result, None);
    let why = reply.error.expect("a refusal");
    assert!(why.contains("prelude"), "{}", why);
}

/// The new name has to survive the lexer as one identifier and must not
/// already be bound where the old one is visible.
#[test]
fn rename_refuses_a_bad_or_taken_new_name() {
    let mut s = Server::new();
    let text =
        "fn main() {\n    let count = 1\n    let other = 2\n    io.print(count + other)\n}\n";
    open(&mut s, "file:///t.kite", text);
    let keyword = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 8, "match"));
    assert!(keyword.error.expect("a refusal").contains("keyword"));
    let broken = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 8, "9lives"));
    assert!(broken.error.expect("a refusal").contains("identifier"));
    let taken = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 8, "other"));
    assert!(taken.error.expect("a refusal").contains("already bound"));
}

/// `Point{ x }` writes one identifier for two roles — the field's name and
/// the binding's — so a rename that would rewrite it is refused rather than
/// left to rename the field along with the binding.
#[test]
fn rename_refuses_a_binding_written_as_a_shorthand_field() {
    let mut s = Server::new();
    let text = "struct Point {\n    x: int\n}\nfn main() {\n    let x = 1\n    let p = Point{ x }\n    io.print(p.x)\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `x` at its declaration on line 4.
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 4, 8, "y"));
    assert_eq!(reply.result, None);
    let why = reply.error.expect("a refusal");
    assert!(why.contains("shorthand"), "{}", why);
}

#[test]
fn prepare_rename_selects_the_name_and_refuses_a_keyword() {
    let mut s = Server::new();
    let text = "fn main() {\n    let count = 1\n    io.print(count)\n}\n";
    open(&mut s, "file:///t.kite", text);
    // On the use of `count`, the exact occurrence is offered back.
    let reply = s.handle("textDocument/prepareRename", &at("file:///t.kite", 2, 14));
    let result = reply.result.expect("an answer");
    assert_eq!(result.path("range.start.character").and_then(|c| c.as_u32()), Some(13));
    assert_eq!(
        result.get("placeholder").and_then(|p| p.as_str()),
        Some("count")
    );
    // On `let` there is nothing to rename, and the answer says so.
    let refused = s.handle("textDocument/prepareRename", &at("file:///t.kite", 1, 4));
    assert!(refused.error.is_some());
}

#[test]
fn references_find_the_declaration_and_every_use() {
    let mut s = Server::new();
    let text =
        "fn main() {\n    let count = 1\n    io.print(count)\n    io.print(count + 1)\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply =
        s.handle("textDocument/references", &references_at("file:///t.kite", 2, 14, true));
    let Some(Json::Array(items)) = reply.result else {
        panic!("no locations");
    };
    let lines: Vec<u32> = items
        .iter()
        .filter_map(|i| i.path("range.start.line").and_then(|l| l.as_u32()))
        .collect();
    assert_eq!(lines, vec![1, 2, 3], "{:?}", items);
    // Without `includeDeclaration`, only the uses.
    let reply =
        s.handle("textDocument/references", &references_at("file:///t.kite", 2, 14, false));
    let Some(Json::Array(items)) = reply.result else {
        panic!("no locations");
    };
    assert_eq!(items.len(), 2, "{:?}", items);
}

/// The two facts the source never states: the type a bare `let` received, and
/// the type arguments a generic call solved. Kite has no turbofish, so the
/// call site is the one place the latter can be seen at all.
#[test]
fn inlay_hints_show_the_inferred_type_and_the_solved_arguments() {
    let mut s = Server::new();
    let text = "fn same<T>(x: T) -> T {\n    return x\n}\nfn main() {\n    let y = same(5)\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/inlayHint", &hints_over("file:///t.kite"));
    let Some(Json::Array(items)) = reply.result else {
        panic!("no hints");
    };
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    // After `y`, the type it was given; after `same`, what the call inferred.
    assert_eq!(labels, vec![": int", "<int>"], "{:?}", items);
    assert_eq!(items[0].path("position.line").and_then(|l| l.as_u32()), Some(4));
    assert_eq!(items[0].path("position.character").and_then(|c| c.as_u32()), Some(9));
    assert_eq!(items[1].path("position.character").and_then(|c| c.as_u32()), Some(16));
}

/// Hovering a variable says what it is.
///
/// It used to answer nothing at all. `Res::Local` was skipped when the index
/// was built, under a comment claiming the editor still "gets the name and
/// where it was used" — which is what *rename* gets. Hover reads `uses`, found
/// no entry, and returned null.
#[test]
fn hovering_a_variable_shows_its_type() {
    let mut s = Server::new();
    let text = "fn main() {\n    let total = 41\n    io.print(total + 1)\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `total` on the last line, inside the call.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 2, 13));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("total"), "{}", value);
    assert!(value.contains("int"), "{}", value);
}

/// And a parameter, which is a local the function's own signature declared.
#[test]
fn hovering_a_parameter_shows_its_type() {
    let mut s = Server::new();
    let text = "fn twice(n: int) -> int {\n    return n * 2\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `n` in the body.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 1, 11));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("int"), "{}", value);
}

/// Hovering a method says what it is.
///
/// A method call is resolved by the *checker* rather than the resolver — it
/// needs the receiver's type — so it never reached the index at all, which is
/// the same reason rename refuses to touch one.
#[test]
fn hovering_a_method_shows_its_signature() {
    let mut s = Server::new();
    let text = "struct Rect {\n    w: float\n    h: float\n}\n\
                impl Rect {\n    pub fn area(self) -> float {\n        return self.w * self.h\n    }\n}\n\
                fn main() {\n    let r = Rect{ w: 2.0, h: 3.0 }\n    io.print(r.area())\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `area` in `r.area()`.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 11, 16));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("area"), "{}", value);
}

/// And a field, which is found the same way a method is.
#[test]
fn hovering_a_field_shows_its_type() {
    let mut s = Server::new();
    let text = "struct Rect {\n    w: float\n    h: float\n}\n\
                impl Rect {\n    pub fn area(self) -> float {\n        return self.w * self.h\n    }\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `w` in `self.w`.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 6, 20));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("float"), "{}", value);
}

/// And the declaration itself, which is the most likely place to hover.
///
/// The inlay hint sits beside it, but a hint is not an answer for someone who
/// has them switched off.
#[test]
fn hovering_a_declaration_shows_its_type() {
    let mut s = Server::new();
    let text = "fn main() {\n    let names = [\"x\", \"y\"]\n    io.print(names.len())\n}\n";
    open(&mut s, "file:///t.kite", text);
    // `names` where it is declared.
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", 1, 8));
    let value = reply
        .result
        .expect("an answer")
        .path("contents.value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(value.contains("names"), "{}", value);
    assert!(value.contains("[str]"), "{}", value);
}

fn change(server: &mut Server, uri: &str, text: &str) -> Vec<Json> {
    let message = Json::object(vec![
        ("method", Json::str("textDocument/didChange")),
        (
            "params",
            Json::object(vec![
                ("textDocument", Json::object(vec![("uri", Json::str(uri))])),
                (
                    "contentChanges",
                    Json::Array(vec![Json::object(vec![("text", Json::str(text))])]),
                ),
            ]),
        ),
    ]);
    let reply = server.handle("textDocument/didChange", &message);
    reply
        .notifications
        .into_iter()
        .map(|(_, params)| params)
        .collect()
}

/// The codes of the diagnostics published for one URI, if it was published.
fn codes_for(published: &[Json], uri: &str) -> Option<Vec<String>> {
    let entry = published
        .iter()
        .find(|p| p.get("uri").and_then(|u| u.as_str()) == Some(uri))?;
    let Some(Json::Array(items)) = entry.get("diagnostics") else {
        return None;
    };
    Some(
        items
            .iter()
            .map(|d| d.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string())
            .collect(),
    )
}

/// A directory of real files, for what the loader reads from disk.
struct Project {
    dir: std::path::PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("kite-lsp-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Project { dir }
    }

    /// Write a file, and answer the URI an editor would name it by.
    fn file(&self, name: &str, text: &str) -> String {
        let path = self.dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory for it");
        }
        std::fs::write(&path, text).expect("written");
        self.uri(name)
    }

    /// The URI an editor names a file in the project by, written or not —
    /// `file:///C%3A/…` on Windows, where `file://` and a path is not a URI.
    fn uri(&self, name: &str) -> String {
        uri_of_path(&self.dir.join(name).to_string_lossy())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Hover answers from this file only.
///
/// Every file numbers its bytes from zero, and the prelude's uses are recorded
/// before the program's. Hover used to take the first span covering the
/// offset in *any* file, so a cursor on nothing in particular — or on a name
/// that did not resolve — was described as whatever the prelude had there.
#[test]
fn hover_never_answers_with_another_files_declaration() {
    // An offset where the prelude has a use, found from the compiler's index.
    let probe = kite_driver::compile("/probe.kite", "fn main() {\n}\n", kite_driver::Emit::Check);
    let prelude_use = probe
        .index
        .uses
        .iter()
        .find(|u| u.at.file.0 == 0 && u.at.start > 40)
        .expect("the prelude uses names")
        .at
        .start as usize;
    // A file that is all comment until well past that offset.
    let mut text = String::from("fn main() {\n");
    while text.len() < prelude_use + 100 {
        text.push_str("    // nothing to see here, and nothing to hover over at all\n");
    }
    text.push_str("}\n");
    let before = &text[..prelude_use];
    let line = before.matches('\n').count() as u32;
    let character = (before.len() - before.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    let mut s = Server::new();
    open(&mut s, "file:///t.kite", &text);
    let reply = s.handle("textDocument/hover", &at("file:///t.kite", line, character));
    assert_eq!(reply.result, Some(Json::Null), "a comment was described");
    let reply = s.handle("textDocument/definition", &at("file:///t.kite", line, character));
    assert_eq!(reply.result, Some(Json::Null), "a comment had a definition");
}

/// Go to definition on a local reaches its `let`, not the use it started on.
#[test]
fn go_to_definition_on_a_local_finds_its_declaration() {
    let mut s = Server::new();
    let text = "fn helper(n: int) -> int {\n    let total = n + 1\n    return total\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/definition", &at("file:///t.kite", 2, 12));
    let result = reply.result.expect("an answer");
    assert_eq!(result.path("range.start.line").and_then(|l| l.as_u32()), Some(1));
    assert_eq!(result.path("range.start.character").and_then(|c| c.as_u32()), Some(8));
    // And a parameter reaches the signature.
    let reply = s.handle("textDocument/definition", &at("file:///t.kite", 1, 16));
    let result = reply.result.expect("an answer");
    assert_eq!(result.path("range.start.line").and_then(|l| l.as_u32()), Some(0));
    assert_eq!(result.path("range.start.character").and_then(|c| c.as_u32()), Some(10));
}

/// A name from one of the program's own modules is in a file the editor can
/// open, so definition answers with that file.
#[test]
fn go_to_definition_reaches_a_sibling_module() {
    let p = Project::new("definition");
    let config = p.file("config.kite", "// the port\n\npub fn port() -> int {\n    return 80\n}\n");
    let main_text = "use config\n\nfn main() {\n    io.print(config.port())\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    // `port` in `config.port()`. config.kite is not open, so it was read from
    // disk and its URI is made from its path.
    let reply = s.handle("textDocument/definition", &at(&main, 3, 21));
    let result = reply.result.expect("an answer");
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some(config.as_str()));
    assert_eq!(result.path("range.start.line").and_then(|l| l.as_u32()), Some(2));
    // Open, it is answered with the editor's own URI for the buffer, and the
    // range is in the buffer's text rather than the file's.
    let unsaved = "pub fn port() -> int {\n    return 80\n}\n";
    open(&mut s, "file:///elsewhere/config.kite", unsaved);
    open(&mut s, &config, unsaved);
    let reply = s.handle("textDocument/definition", &at(&main, 3, 21));
    let result = reply.result.expect("an answer");
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some(config.as_str()));
    assert_eq!(result.path("range.start.line").and_then(|l| l.as_u32()), Some(0));
}

/// An open buffer is the truth, for the files that import it too.
///
/// Modules used to be read from disk whatever the editor held, so an unsaved
/// `pub fn host` in config.kite was an unknown name in main.kite until it was
/// saved — and an edit that broke main.kite was not reported there at all.
#[test]
fn an_unsaved_module_is_what_its_importers_see() {
    let p = Project::new("unsaved");
    let config = p.file("config.kite", "pub fn port() -> int {\n    return 80\n}\n");
    // Only on disk, and imported by the unsaved buffer: a handed-over module
    // still finds its own imports beside it.
    p.file("helper.kite", "pub fn name() -> str {\n    return \"x\"\n}\n");
    let main_text = "use config\n\nfn main() {\n    io.print(config.host())\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    let published = open(
        &mut s,
        &config,
        "use helper\n\npub fn port() -> int {\n    return 80\n}\n\n\
         pub fn host() -> str {\n    return helper.name()\n}\n",
    );
    assert_eq!(codes_for(&published, &config), Some(Vec::new()), "{:?}", published);
    let published = open(&mut s, &main, main_text);
    assert_eq!(codes_for(&published, &main), Some(Vec::new()), "{:?}", published);

    // Taking `host` away again is reported against main.kite, which is
    // republished along with the file that changed.
    let published = change(&mut s, &config, "pub fn port() -> int {\n    return 80\n}\n");
    assert_eq!(codes_for(&published, &config), Some(Vec::new()));
    let broken = codes_for(&published, &main).expect("main.kite is republished");
    assert_eq!(broken.len(), 1, "{:?}", published);

    // Closing the buffer goes back to the disk, where `host` never existed.
    let reply = s.handle("textDocument/didClose", &at(&config, 0, 0));
    let published: Vec<Json> = reply.notifications.into_iter().map(|(_, p)| p).collect();
    assert_eq!(codes_for(&published, &config), Some(Vec::new()));
    assert_eq!(codes_for(&published, &main).map(|c| c.len()), Some(1), "{:?}", published);
}

/// Completion offers what this file can write: another module's private items
/// — the prelude's included — are not among them.
#[test]
fn completion_leaves_out_another_modules_private_items() {
    let mut s = Server::new();
    open(
        &mut s,
        "file:///proj/config.kite",
        "pub fn port() -> int {\n    return secret()\n}\n\nfn secret() -> int {\n    return 80\n}\n",
    );
    open(&mut s, "file:///proj/main.kite", "use config\n\nfn own() {\n}\n\nfn main() {\n}\n");
    let reply = s.handle("textDocument/completion", &at("file:///proj/main.kite", 6, 0));
    let Some(Json::Array(items)) = reply.result.as_ref().and_then(|r| r.get("items")) else {
        panic!("no items");
    };
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"config.port"), "{:?}", labels);
    assert!(labels.contains(&"own"), "{:?}", labels);
    assert!(!labels.contains(&"config.secret"), "{:?}", labels);
}

/// A constant's uses are all in the binding table, so it renames like a local
/// — and it is listed as a constant, not a class.
#[test]
fn a_constant_renames_and_is_listed_as_a_constant() {
    let mut s = Server::new();
    let text = "let LIMIT = 10\n\nfn main() {\n    io.print(LIMIT)\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 3, 14, "CAP"));
    assert_eq!(reply.error, None);
    let result = reply.result.expect("an answer");
    let Some(Json::Array(edits)) = result.get("changes").and_then(|c| c.get("file:///t.kite"))
    else {
        panic!("no edits");
    };
    assert_eq!(edits.len(), 2, "{:?}", edits);

    let reply = s.handle("textDocument/documentSymbol", &at("file:///t.kite", 0, 0));
    let Some(Json::Array(symbols)) = reply.result else {
        panic!("no symbols");
    };
    let limit = symbols
        .iter()
        .find(|i| i.get("name").and_then(|n| n.as_str()) == Some("LIMIT"))
        .expect("LIMIT is listed");
    assert_eq!(limit.get("kind").and_then(|k| k.as_u32()), Some(14));

    let reply = s.handle("textDocument/completion", &at("file:///t.kite", 3, 0));
    let Some(Json::Array(items)) = reply.result.as_ref().and_then(|r| r.get("items")) else {
        panic!("no items");
    };
    let limit = items
        .iter()
        .find(|i| i.get("label").and_then(|n| n.as_str()) == Some("LIMIT"))
        .expect("LIMIT is offered");
    assert_eq!(limit.get("kind").and_then(|k| k.as_u32()), Some(21));
}

/// §2.1 compares identifiers after NFC, so `café` spelled with a combining
/// accent is the same variable — and a rename that skipped that spelling left
/// a use of a name that no longer existed.
#[test]
fn rename_rewrites_every_spelling_of_the_name() {
    let mut s = Server::new();
    let text = "fn main() {\n    let caf\u{e9} = 1\n    io.print(cafe\u{301} + caf\u{e9})\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 1, 9, "tea"));
    assert_eq!(reply.error, None);
    let result = reply.result.expect("an answer");
    let Some(Json::Array(edits)) = result.get("changes").and_then(|c| c.get("file:///t.kite"))
    else {
        panic!("no edits");
    };
    assert_eq!(edits.len(), 3, "{:?}", edits);
}

/// The diagnostics a file has now, as the messages the editor would show.
fn messages_for(server: &mut Server, uri: &str) -> Vec<String> {
    let reply = server.handle("textDocument/didSave", &at(uri, 0, 0));
    let published: Vec<Json> = reply.notifications.into_iter().map(|(_, p)| p).collect();
    let Some(Json::Array(items)) = published.first().and_then(|p| p.get("diagnostics")).cloned()
    else {
        panic!("nothing published for {}", uri);
    };
    items
        .iter()
        .map(|d| d.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string())
        .collect()
}

// ---- an open buffer is the file it is a buffer of, and nothing else ----------
//
// The editor handed every open buffer over as a provided module, keyed by its
// path below the file being compiled, and a provided key was consulted before
// anything on disk. Merely opening a file changed what `use` lines meant, and
// the editor showed errors `kitec check` did not have. In each of these the
// buffers opened hold exactly what is on disk, so the editor and the build
// must agree.

/// A declared dependency is what `use md` reaches, however many files called
/// `md.kite` are open beside the entry.
#[test]
fn an_open_sibling_does_not_take_a_declared_dependencys_name() {
    let p = Project::new("dep-over-buffer");
    p.file("lib/md/kite.toml", "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    let dependency =
        p.file("lib/md/md.kite", "pub fn render() -> str {\n    return \"from dependency\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmd = { path = \"../lib/md\" }\n",
    );
    let sibling_text = "pub fn render() -> int {\n    return 1\n}\n";
    let sibling = p.file("app/md.kite", sibling_text);
    let main_text = "use md\n\nfn main() {\n    let s: str = md.render()\n    io.print(s)\n}\n";
    let main = p.file("app/main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    open(&mut s, &sibling, sibling_text);
    assert_eq!(messages_for(&mut s, &main), Vec::<String>::new());
    // And definition goes where the build went.
    let reply = s.handle("textDocument/definition", &at(&main, 3, 20));
    let result = reply.result.expect("an answer");
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some(dependency.as_str()));
}

/// A dependency's own `use util` is its own `util`, not a buffer of the
/// application's that happens to sit where the dependency's name would put
/// it.
#[test]
fn a_dependencys_import_is_not_answered_by_an_application_buffer() {
    let p = Project::new("dep-import-buffer");
    p.file("md/kite.toml", "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    p.file("md/md.kite", "use util\n\npub fn render() -> util.Thing {\n    return util.make()\n}\n");
    p.file(
        "md/util.kite",
        "pub struct Thing {\n    pub v: int\n}\n\npub fn make() -> Thing {\n    return Thing{ v: 7 }\n}\n",
    );
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmd = { path = \"../md\" }\n",
    );
    let stray_text = "pub struct Thing {\n    pub v: str\n}\n\n\
                      pub fn make() -> Thing {\n    return Thing{ v: \"app\" }\n}\n";
    let stray = p.file("app/md/util.kite", stray_text);
    let main_text = "use md\n\nfn main() {\n    let n: int = md.render().v\n    io.print(n)\n}\n";
    let main = p.file("app/main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    open(&mut s, &stray, stray_text);
    assert_eq!(messages_for(&mut s, &main), Vec::<String>::new());
}

/// Inside `a/`, `use x/y` is `a/x/y.kite`, whether or not an `x/y.kite`
/// beside the entry is open.
#[test]
fn a_nested_modules_import_is_not_answered_by_an_entry_level_buffer() {
    let p = Project::new("nested-import-buffer");
    p.file("a/m.kite", "use x/y\n\npub fn f() -> y.Thing {\n    return y.make()\n}\n");
    p.file(
        "a/x/y.kite",
        "pub struct Thing {\n    pub v: int\n}\n\npub fn make() -> Thing {\n    return Thing{ v: 5 }\n}\n",
    );
    let top_text = "pub struct Thing {\n    pub v: str\n}\n\n\
                    pub fn make() -> Thing {\n    return Thing{ v: \"top\" }\n}\n";
    let top = p.file("x/y.kite", top_text);
    let main_text = "use a/m\n\nfn main() {\n    let n: int = m.f().v\n    io.print(n)\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    open(&mut s, &top, top_text);
    assert_eq!(messages_for(&mut s, &main), Vec::<String>::new());
}

/// `use config` is the directory `config/` when there is one (§13.1), and an
/// open `config.kite` beside it does not change that.
#[test]
fn an_open_file_does_not_hide_a_directory_module() {
    let p = Project::new("dir-over-buffer");
    p.file("config/load.kite", "pub fn port() -> int {\n    return 80\n}\n");
    let file_text = "pub fn other() -> int {\n    return 1\n}\n";
    let file = p.file("config.kite", file_text);
    let main_text = "use config\n\nfn main() {\n    io.print(config.port())\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    open(&mut s, &file, file_text);
    assert_eq!(messages_for(&mut s, &main), Vec::<String>::new());
    let reply = s.handle("textDocument/definition", &at(&main, 3, 21));
    let result = reply.result.expect("an answer");
    let uri = result.get("uri").and_then(|u| u.as_str()).unwrap_or_default();
    assert!(uri.ends_with("/config/load.kite"), "{}", uri);
}

/// An unsaved file in a directory module is part of the module, as it will
/// be once saved — and an edit to it is what the module's importers see.
#[test]
fn an_unsaved_file_in_a_directory_module_is_part_of_it() {
    let p = Project::new("dir-unsaved");
    p.file("config/load.kite", "pub fn port() -> int {\n    return 80\n}\n");
    let main_text = "use config\n\nfn main() {\n    io.print(config.port() + config.extra())\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    assert_eq!(messages_for(&mut s, &main).len(), 1, "`extra` is nowhere yet");
    let fresh = p.uri("config/extra.kite");
    open(&mut s, &fresh, "pub fn extra() -> int {\n    return 1\n}\n");
    assert_eq!(messages_for(&mut s, &main), Vec::<String>::new());
}

/// A cycle back to the open file is shown in it, as `kitec check` reports it.
/// It was reported inside a second copy of the file, read as a module, and
/// the editor — which shows only the file's own diagnostics — showed nothing.
#[test]
fn a_cycle_back_to_the_open_file_is_shown_in_it() {
    let p = Project::new("cycle");
    let a_text = "use b\n\npub fn fa() -> int {\n    return b.fb()\n}\n\nfn main() {\n    io.print(fa())\n}\n";
    let a = p.file("a.kite", a_text);
    p.file("b.kite", "use a\n\npub fn fb() -> int {\n    return 1\n}\n");
    let mut s = Server::new();
    let published = open(&mut s, &a, a_text);
    assert_eq!(codes_for(&published, &a), Some(vec!["E0402".to_string()]), "{:?}", published);
}

/// Definition into a path dependency answers with the open buffer's URI. The
/// loader names the file `app/../lib/md/md.kite`, which matched no open URI,
/// and the editor opened the file a second time.
#[test]
fn definition_into_a_path_dependency_lands_in_its_open_buffer() {
    let p = Project::new("dep-definition");
    p.file("lib/md/kite.toml", "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    let dependency_text = "pub fn render() -> str {\n    return \"from dependency\"\n}\n";
    let dependency = p.file("lib/md/md.kite", dependency_text);
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmd = { path = \"../lib/md\" }\n",
    );
    let main_text = "use md\n\nfn main() {\n    io.print(md.render())\n}\n";
    let main = p.file("app/main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &main, main_text);
    // Closed, the URI is still the file's own path, folded.
    let reply = s.handle("textDocument/definition", &at(&main, 3, 17));
    let result = reply.result.expect("an answer");
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some(dependency.as_str()));
    open(&mut s, &dependency, dependency_text);
    let reply = s.handle("textDocument/definition", &at(&main, 3, 17));
    let result = reply.result.expect("an answer");
    assert_eq!(result.get("uri").and_then(|u| u.as_str()), Some(dependency.as_str()));
}

// ---- what a rename may touch -------------------------------------------------

/// A `pub` name's importers are in other files, so a rename in its own file
/// edited the declaration alone and broke every one of them.
#[test]
fn rename_refuses_a_pub_name() {
    let p = Project::new("rename-pub");
    let config_text = "pub fn port() -> int {\n    return 80\n}\n\npub let PORT = 80\n";
    let config = p.file("config.kite", config_text);
    let main_text = "use config\n\nfn main() {\n    io.print(config.port() + config.PORT)\n}\n";
    let main = p.file("main.kite", main_text);
    let mut s = Server::new();
    open(&mut s, &config, config_text);
    open(&mut s, &main, main_text);
    for (line, character) in [(0, 8), (4, 9)] {
        let reply = s.handle("textDocument/rename", &rename_at(&config, line, character, "p2"));
        assert_eq!(reply.result, None);
        let why = reply.error.expect("a refusal");
        assert!(why.contains("`pub`"), "{}", why);
        let refused = s.handle("textDocument/prepareRename", &at(&config, line, character));
        assert!(refused.error.is_some());
    }
}

/// §2.1 compares identifiers after NFC, so a new name that is an existing one
/// spelled with a combining accent is that name. It was compared byte for
/// byte, got through, and changed what the old uses resolved to.
#[test]
fn rename_refuses_a_name_already_bound_under_another_spelling() {
    let mut s = Server::new();
    let text = "fn main() {\n    let caf\u{e9} = 1\n    if true {\n        let x = 2\n        io.print(caf\u{e9} + x)\n    }\n}\n";
    open(&mut s, "file:///t.kite", text);
    for new in ["caf\u{e9}", "cafe\u{301}"] {
        let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 3, 12, new));
        let why = reply.error.expect("a refusal");
        assert!(why.contains("already bound"), "{:?}: {}", new, why);
    }
    // A name accepted is written in NFC, the form the compiler holds it in.
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 3, 12, "the\u{301}"));
    let result = reply.result.expect("an answer");
    let Some(Json::Array(edits)) = result.get("changes").and_then(|c| c.get("file:///t.kite"))
    else {
        panic!("no edits");
    };
    assert!(edits.iter().all(|e| e.get("newText").and_then(|t| t.as_str()) == Some("th\u{e9}")));
}

/// What the parser skipped to recover was never resolved, so an occurrence
/// there is in no table: the rename edited the rest, and the leftover spelling
/// meant something else once the line was mended.
#[test]
fn rename_refuses_while_the_file_does_not_parse() {
    let mut s = Server::new();
    let text = "fn helper(a: int) -> int {\n    return a + 1\n}\n\n\
                fn main() {\n    let v = helper(1\n    io.print(helper(2))\n}\n";
    open(&mut s, "file:///t.kite", text);
    let reply = s.handle("textDocument/rename", &rename_at("file:///t.kite", 0, 4, "helper2"));
    assert_eq!(reply.result, None);
    let why = reply.error.expect("a refusal");
    assert!(why.contains("syntax errors"), "{}", why);
}

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// A message that is not JSON is answered with the protocol's parse error, and
/// the next one is still read. One unpaired surrogate used to end the session.
#[test]
fn a_malformed_message_is_answered_and_the_session_goes_on() {
    let input = [
        frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize""#),
        frame(r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"x":"\ud800"}}"#),
        frame(r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#),
        frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    ]
    .concat();
    let mut output = Vec::new();
    let code = crate::serve(&mut std::io::Cursor::new(input), &mut output);
    let said = String::from_utf8(output).expect("utf-8");
    assert!(said.contains(r#""code":-32700"#), "{}", said);
    assert!(said.contains(r#""id":2"#), "{}", said);
    assert!(said.contains(r#""id":3"#), "{}", said);
    assert_eq!(code, 0);
}

/// The other ways a message could end the session: a length the server
/// allocated before reading (a panic on the capacity, or an abort on the
/// allocation), nesting it recursed into until the stack ran out, and a header
/// line that was not UTF-8, which read as the stream closing. Each is answered,
/// and the next message is read.
#[test]
fn no_malformed_message_ends_the_session() {
    let deep = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"foo","params":{}{}}}"#,
        "[".repeat(50_000),
        "]".repeat(50_000)
    );
    let oversized = "Content-Length: 1000000000000\r\n\r\n{}";
    let input = [
        frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).into_bytes(),
        frame(&deep).into_bytes(),
        b"X-Junk: \xff\xfe\r\n".to_vec(),
        frame(r#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#).into_bytes(),
        oversized.as_bytes().to_vec(),
    ]
    .concat();
    let mut output = Vec::new();
    // The stream ends inside the oversized body, which is the stream ending.
    let code = crate::serve(&mut std::io::Cursor::new(input), &mut output);
    let said = String::from_utf8(output).expect("utf-8");
    assert!(said.contains(r#""id":1"#), "{}", said);
    assert!(said.contains(r#""id":7"#), "the header did not end the session: {}", said);
    assert_eq!(said.matches(r#""code":-32700"#).count(), 2, "{}", said);
    assert!(said.contains("1000000000000 bytes"), "{}", said);
    assert_eq!(code, 0);

    // A length past the limit, followed by the body it states: the body is
    // passed over and the session goes on.
    let body = "x".repeat(70 << 20);
    let input = [
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body),
        frame(r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#),
        frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    ]
    .concat();
    let mut output = Vec::new();
    let code = crate::serve(&mut std::io::Cursor::new(input), &mut output);
    let said = String::from_utf8(output).expect("utf-8");
    assert!(said.contains(r#""id":3"#), "{}", said);
    assert_eq!(code, 0);
    // And one no buffer could hold, which panicked on the capacity.
    let huge = "Content-Length: 18446744073709551615\r\n\r\n{}";
    let mut output = Vec::new();
    crate::serve(&mut std::io::Cursor::new(huge.as_bytes().to_vec()), &mut output);
    assert!(String::from_utf8(output).expect("utf-8").contains("-32700"));
}

/// `exit` without a `shutdown` first is the editor stopping a server it did
/// not ask to stop, and the protocol says that exits with 1.
#[test]
fn exit_without_shutdown_is_a_failure() {
    let input = frame(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    let mut output = Vec::new();
    assert_eq!(crate::serve(&mut std::io::Cursor::new(input), &mut output), 1);
}

// ---- what an edit elsewhere republishes --------------------------------------

/// A file not yet saved, in a directory not yet created, is part of that
/// directory's module when the project is opened through a symbolic link.
/// Its buffer was keyed by the linked spelling and the directory was asked
/// for by the real one, so the module was `cannot find module`.
#[cfg(unix)]
#[test]
fn an_unsaved_directory_module_is_found_through_a_linked_project() {
    let p = Project::new("linked-unsaved");
    let main_text = "use newdir\n\nfn main() {\n    io.print(newdir.v())\n}\n";
    p.file("main.kite", main_text);
    let link = std::env::temp_dir().join(format!("kite-lsp-linked-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&p.dir, &link).expect("a link");
    let through = |name: &str| uri_of_path(&link.join(name).to_string_lossy());
    let mut s = Server::new();
    open(&mut s, &through("newdir/new.kite"), "pub fn v() -> int {\n    return 1\n}\n");
    let main = through("main.kite");
    let published = open(&mut s, &main, main_text);
    let _ = std::fs::remove_file(&link);
    assert_eq!(codes_for(&published, &main), Some(Vec::new()), "{:?}", published);
}
