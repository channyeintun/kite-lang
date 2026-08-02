use super::*;

const FULL: &str = r#"
[package]
name    = "myapp"     # the name a dependency would use
version = "0.1.0"

[targets]
web    = { entry = "src/main.kite", renderer = "dom" }
native = { entry = "src/main.kite" }

[dependencies]
markdown = { git = "https://github.com/example/kite-markdown", tag = "v1.2.0" }
json     = { git = "https://github.com/example/kite-json", version = "^1.2" }
shared   = { path = "../shared" }
local    = "vendor/local"
"#;

#[test]
fn a_manifest_reads_as_the_specification_writes_it() {
    let m = parse(FULL).expect("parses");
    assert_eq!(m.name, "myapp");
    assert_eq!(m.version, "0.1.0");

    assert_eq!(m.targets["web"].entry, "src/main.kite");
    assert_eq!(m.targets["web"].renderer.as_deref(), Some("dom"));
    assert_eq!(m.targets["native"].renderer, None);

    assert_eq!(m.dependencies.len(), 4);
    assert_eq!(m.dependencies[0].name, "markdown");
    assert_eq!(
        m.dependencies[0].source,
        Source::Git {
            url: "https://github.com/example/kite-markdown".into(),
            tag: Some("v1.2.0".into()),
        }
    );
    assert_eq!(m.dependencies[0].version, None);
    assert_eq!(
        m.dependencies[1].source,
        Source::Git { url: "https://github.com/example/kite-json".into(), tag: None }
    );
    assert_eq!(m.dependencies[1].version, Some(Requirement::parse("^1.2").unwrap()));
    assert_eq!(m.dependencies[2].source, Source::Path("../shared".into()));
    assert_eq!(m.dependencies[3].source, Source::Path("vendor/local".into()));
}

/// A `#` inside a string is not a comment: a manifest's strings are paths and
/// URLs, which contain them.
#[test]
fn a_hash_inside_a_string_is_not_a_comment() {
    let m = parse("[package]\nname = \"a#b\"\nversion = \"1\"\n").expect("parses");
    assert_eq!(m.name, "a#b");
}

#[test]
fn a_manifest_needs_a_name() {
    let err = parse("[package]\nversion = \"1\"\n").expect_err("no name");
    assert!(err.message.contains("name"), "{}", err);
}

#[test]
fn an_error_says_which_line() {
    let err = parse("[package]\nname = \"a\"\nnonsense\n").expect_err("not a pair");
    assert_eq!(err.line, 3);
    assert!(err.to_string().starts_with("kite.toml:3:"), "{}", err);
}

#[test]
fn an_unknown_table_is_reported_rather_than_ignored() {
    let err = parse("[package]\nname = \"a\"\n\n[scripts]\npostinstall = \"rm -rf /\"\n")
        .expect_err("no such table");
    assert!(err.message.contains("[scripts]"), "{}", err);
}

#[test]
fn a_dependency_needs_exactly_one_source() {
    let err = parse(
        "[package]\nname = \"a\"\n\n[dependencies]\nboth = { path = \"x\", git = \"y\" }\n",
    )
    .expect_err("two sources");
    assert!(err.message.contains("exactly one"), "{}", err);
}

/// A tag names one commit; a version names a range to be resolved. Accepting
/// both on one dependency would mean silently ignoring one of them.
#[test]
fn a_tag_and_a_version_do_not_mix() {
    let err = parse(
        "[package]\nname = \"a\"\n\n[dependencies]\nmd = { git = \"u\", tag = \"v1\", \
         version = \"^1\" }\n",
    )
    .expect_err("both");
    assert!(err.message.contains("pick one"), "{}", err);
    assert!(err.message.contains("a tag pins, a version resolves"), "{}", err);
}

#[test]
fn a_bad_version_requirement_names_the_dependency() {
    let err = parse(
        "[package]\nname = \"a\"\n\n[dependencies]\nmd = { git = \"u\", version = \"latest\" }\n",
    )
    .expect_err("not a requirement");
    assert!(err.message.contains("`md`"), "{}", err);
    assert!(err.message.contains("`latest`"), "{}", err);
    assert_eq!(err.line, 5);
}

#[test]
fn a_target_needs_an_entry() {
    let err = parse("[package]\nname = \"a\"\n\n[targets]\nweb = { renderer = \"dom\" }\n")
        .expect_err("no entry");
    assert!(err.message.contains("entry"), "{}", err);
}

// ---- the lockfile ---------------------------------------------------------

#[test]
fn a_hash_covers_every_kite_file_and_its_contents() {
    let dir = std::env::temp_dir().join(format!("kite-hash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("create");
    std::fs::write(dir.join("src/a.kite"), "fn a() {\n}\n").expect("write");
    std::fs::write(dir.join("README.md"), "not code").expect("write");

    let first = hash_directory(&dir).expect("hashes");
    // A file that is not Kite does not change it.
    std::fs::write(dir.join("README.md"), "still not code").expect("write");
    assert_eq!(hash_directory(&dir).expect("hashes"), first);

    // Contents do.
    std::fs::write(dir.join("src/a.kite"), "fn a() {\n  io.print(1)\n}\n").expect("write");
    assert_ne!(hash_directory(&dir).expect("hashes"), first);

    // So does a new file.
    let second = hash_directory(&dir).expect("hashes");
    std::fs::write(dir.join("src/b.kite"), "fn b() {\n}\n").expect("write");
    assert_ne!(hash_directory(&dir).expect("hashes"), second);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lockfile_is_written_to_be_read_by_a_person() {
    let text = lockfile(&[Locked {
        name: "markdown".into(),
        version: "1.2.0".into(),
        source: "https://github.com/example/kite-markdown#v1.2.0".into(),
        hash: "0123456789abcdef".into(),
    }]);
    assert!(text.contains("[[locked]]"), "{}", text);
    assert!(text.contains("name = \"markdown\""), "{}", text);
    assert!(text.contains("version = \"1.2.0\""), "{}", text);
    assert!(text.contains("hash = \"0123456789abcdef\""), "{}", text);
    assert!(text.starts_with("# Generated by `kitec pkg`. Commit this."), "{}", text);
}
