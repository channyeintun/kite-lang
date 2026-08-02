use crate::json::{parse, Json};
use crate::server::Server;

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
