use super::*;

#[test]
fn a_doc_comment_attaches_to_what_follows_it() {
    let docs = extract(
        "sample",
        "/// Adds two numbers.\n/// Twice as many as one.\npub fn add(a: int, b: int) -> int {\n    return a + b\n}\n",
    );
    assert_eq!(docs.entries.len(), 1);
    let e = &docs.entries[0];
    assert_eq!(e.name, "add");
    assert_eq!(e.kind, "function");
    assert!(e.is_pub);
    assert_eq!(e.doc, "Adds two numbers.\nTwice as many as one.");
    assert_eq!(e.signature, "pub fn add(a: int, b: int) -> int");
}

/// A blank line between a comment and a declaration means the comment was
/// about something else.
#[test]
fn a_detached_comment_is_not_documentation() {
    let docs = extract("sample", "/// Not about this.\n\npub fn f() {\n}\n");
    assert_eq!(docs.entries[0].doc, "");
}

/// The block at the top of the file is the module's own.
#[test]
fn the_header_becomes_the_overview() {
    let docs = extract(
        "sample",
        "// std/sample — what it is for.\n//\n// A second line.\n\npub fn f() {\n}\n",
    );
    assert_eq!(docs.overview, "std/sample — what it is for.\n\nA second line.");
}

#[test]
fn a_struct_lists_its_fields() {
    let docs = extract(
        "sample",
        "/// A point.\npub struct Point {\n    /// Across.\n    pub x: float\n    pub y: float\n}\n",
    );
    let e = &docs.entries[0];
    assert_eq!(e.kind, "struct");
    assert_eq!(e.members.len(), 2);
    assert_eq!(e.members[0].name, "x");
    assert_eq!(e.members[0].doc, "Across.");
    assert_eq!(e.members[1].doc, "");
}

#[test]
fn an_enum_lists_its_variants() {
    let docs = extract(
        "sample",
        "pub enum Shape {\n    /// Round.\n    Circle(radius: float)\n    Point\n}\n",
    );
    let e = &docs.entries[0];
    assert_eq!(e.kind, "enum");
    assert_eq!(e.members.len(), 2);
    assert_eq!(e.members[0].doc, "Round.");
}

#[test]
fn a_host_declaration_documents_the_boundary() {
    let docs = extract(
        "sample",
        "/// Starts a request.\n@host(\"net\")\nextern fn fetch_start(url: str) -> int\n",
    );
    assert_eq!(docs.entries[0].kind, "host function");
    assert_eq!(docs.entries[0].doc, "Starts a request.");
}

#[test]
fn an_async_function_says_so() {
    let docs = extract("sample", "pub async fn sleep(ms: int) {\n}\n");
    assert_eq!(docs.entries[0].kind, "async function");
}

#[test]
fn markdown_lists_only_the_public_surface_by_default() {
    let docs = extract(
        "sample",
        "pub fn shown() {\n}\nfn hidden() {\n}\n",
    );
    let out = markdown(&docs, true);
    assert!(out.contains("shown"), "{}", out);
    assert!(!out.contains("hidden"), "{}", out);
    let all = markdown(&docs, false);
    assert!(all.contains("hidden"), "{}", all);
}

#[test]
fn markdown_carries_the_signature_and_the_prose() {
    let docs = extract("sample", "/// Doubles it.\npub fn double(n: int) -> int {\n    return n * 2\n}\n");
    let out = markdown(&docs, true);
    assert!(out.contains("```kite\npub fn double(n: int) -> int\n```"), "{}", out);
    assert!(out.contains("Doubles it."), "{}", out);
}

/// The standard library is the tool's own first user: every module has to
/// produce something, and a module with no exports has to say so rather than
/// produce a page of nothing.
#[test]
fn the_standard_library_documents() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut seen = 0;
    for entry in std::fs::read_dir(&root).expect("std directory").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "kite") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read");
        let docs = extract(&name, &src);
        let out = markdown(&docs, true);
        assert!(out.starts_with(&format!("# {}", name)), "{}", out);
        assert!(!docs.overview.is_empty(), "{} has no overview", name);
        seen += 1;
    }
    assert!(seen >= 8, "only {} modules documented", seen);
}
