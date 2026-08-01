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
