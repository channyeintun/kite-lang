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
///
/// Tested on a path rather than on a name, because a name is now restricted to
/// letters, digits, `-` and `_` — it becomes a directory — and the strings this
/// rule exists for were always the other ones.
#[test]
fn a_hash_inside_a_string_is_not_a_comment() {
    let m = parse(
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n         [dependencies]\nlocal = \"vendor/a#b\"\n",
    )
    .expect("parses");
    assert_eq!(m.dependencies[0].source, Source::Path("vendor/a#b".into()));
}

#[test]
fn a_manifest_needs_a_name() {
    let err = parse("[package]\nversion = \"1.0.0\"\n").expect_err("no name");
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

/// The digest is SHA-256, held to the vectors rather than to itself.
///
/// A hand-written hash that is self-consistent but not the algorithm it claims
/// would pass every other test in this file — they only ask whether the answer
/// changes when the input does, which a great many wrong functions also do.
/// These are from FIPS 180-4, and they are the only thing here that could catch
/// a transcription error in the round constants.
#[test]
fn the_digest_is_really_sha256() {
    let hex = |bytes: &[u8]| {
        let mut h = Sha256::new();
        h.update(bytes);
        h.finish().iter().map(|b| format!("{:02x}", b)).collect::<String>()
    };

    assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(
        hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // A megabyte of one letter: more than one block, and enough of them to
    // catch a length counter that wraps or a buffer that is refilled wrongly.
    assert_eq!(
        hex(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );

    // Streaming in pieces is the same as all at once, which is the property
    // `hash_directory` relies on when it feeds a file at a time.
    let mut split = Sha256::new();
    split.update(b"ab");
    split.update(b"");
    split.update(b"c");
    let joined = split.finish().iter().map(|b| format!("{:02x}", b)).collect::<String>();
    assert_eq!(joined, hex(b"abc"));
}

/// A name and a body are length-prefixed, so moving a byte between them is a
/// different digest. Without that, a dependency could rename a file to absorb
/// a change to another one's contents.
#[test]
fn a_filename_cannot_be_traded_against_its_contents() {
    let root = std::env::temp_dir().join(format!("kite-hash-split-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let one = root.join("one");
    std::fs::create_dir_all(&one).expect("create");
    std::fs::write(one.join("ab.kite"), "c").expect("write");

    let two = root.join("two");
    std::fs::create_dir_all(&two).expect("create");
    std::fs::write(two.join("a.kite"), "bc").expect("write");

    assert_ne!(
        hash_directory(&one).expect("hashes"),
        hash_directory(&two).expect("hashes"),
        "`ab` holding `c` and `a` holding `bc` present the same bytes without a length prefix"
    );

    let _ = std::fs::remove_dir_all(&root);
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


// ---- names that would escape the vendor directory -------------------------
//
// A dependency name becomes a directory under `.kite/vendor`, and `kitec pkg`
// deletes that directory and writes into it. A name is also learned
// *transitively* — a dependency's own manifest introduces one — so the name
// that escapes need never appear in a manifest anybody here wrote. These are
// the shapes that escaped before the check existed.

#[test]
fn a_dependency_name_cannot_climb_out_of_the_vendor_directory() {
    for name in ["../../.git/hooks", "..", "../sibling", "a/b"] {
        let text = format!(
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\n{} = \"x\"\n",
            name
        );
        let err = parse(&text).expect_err(&format!("`{}` should be refused", name));
        assert!(err.message.contains("is not a dependency name"), "{}", err);
    }
}

#[test]
fn a_dependency_name_cannot_be_absolute() {
    let err = parse(
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n         [dependencies]\n/tmp/anywhere = \"x\"\n",
    )
    .expect_err("an absolute name replaces the whole path when joined");
    assert!(err.message.contains("is not a dependency name"), "{}", err);
}

/// The package name is checked too, and not for symmetry: the resolver
/// compares a candidate's own `[package] name` against the name that asked for
/// it, so leaving this unchecked would let an attacker keep the pair
/// consistent and slip the escaping name through that gate.
#[test]
fn a_package_name_is_held_to_the_same_rule() {
    let err = parse("[package]\nname = \"../../.git/hooks\"\nversion = \"1.0.0\"\n")
        .expect_err("a package name is joined onto a path too");
    assert!(err.message.contains("is not a package name"), "{}", err);
}

#[test]
fn an_ordinary_name_still_parses() {
    let m = parse(
        "[package]\nname = \"my_app_2\"\nversion = \"1.0.0\"\n\n         [dependencies]\njson_rpc_2 = \"vendor/x\"\n",
    )
    .expect("letters, digits and `_` are a name");
    assert_eq!(m.name, "my_app_2");
    assert_eq!(m.dependencies[0].name, "json_rpc_2");
}

/// A name is written in a `use`, so it is an identifier. `kite-md` was
/// accepted, and was then a package nobody could import: `use kite-md` does
/// not parse.
#[test]
fn a_name_that_cannot_be_written_in_a_use_is_refused() {
    for name in ["kite-md", "2fast", "-x"] {
        let text = format!("[package]\nname = \"{}\"\nversion = \"1.0.0\"\n", name);
        let err = parse(&text).expect_err(&format!("`{}` should be refused", name));
        assert!(err.message.contains("is not a package name"), "{}", err);
        let text = format!(
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\n{} = \"x\"\n",
            name
        );
        let err = parse(&text).expect_err(&format!("`{}` should be refused", name));
        assert!(err.message.contains("is not a dependency name"), "{}", err);
    }
}

// ---- nothing written in a manifest is ignored --------------------------------

/// A key nothing reads was dropped: `verison = "^2"` resolved as any version,
/// and `branch = "main"` beside a `git` as whatever the default branch was.
#[test]
fn a_key_nothing_reads_is_refused() {
    for (line, key) in [
        ("a = { path = \"../a\", verison = \"^2\" }", "verison"),
        ("a = { git = \"https://example.com/a\", branch = \"main\" }", "branch"),
    ] {
        let text = format!("[package]\nname = \"x\"\nversion = \"1.0.0\"\n\n[dependencies]\n{}\n", line);
        let err = parse(&text).expect_err(line);
        assert!(err.message.contains(&format!("has no `{}`", key)), "{}", err);
        assert_eq!(err.line, 6, "{}", err);
    }
    let err = parse("[package]\nname = \"x\"\nversion = \"1.0.0\"\n\n[targets]\nweb = { entry = \"a.kite\", rendrer = \"dom\" }\n")
        .expect_err("a misspelt target key");
    assert!(err.message.contains("has no `rendrer`"), "{}", err);
}

/// A tag names a commit in a repository; beside a `path` it named nothing and
/// was dropped.
#[test]
fn a_tag_beside_a_path_is_refused() {
    let err = parse(
        "[package]\nname = \"x\"\nversion = \"1.0.0\"\n\n[dependencies]\na = { path = \"../a\", tag = \"v1.0.0\" }\n",
    )
    .expect_err("a tag on a path");
    assert!(err.message.contains("a `tag` and a `path`"), "{}", err);
}

/// The program's own version is a version. It was checked only when the
/// package was somebody else's dependency.
#[test]
fn the_packages_own_version_is_checked() {
    let err = parse("[package]\nname = \"x\"\nversion = \"banana\"\n").expect_err("not a version");
    assert_eq!(err.line, 3);
    assert!(err.message.contains("is not a version"), "{}", err);
}

#[test]
fn a_dependency_declared_twice_is_refused() {
    let err = parse("[package]\nname = \"x\"\nversion = \"1.0.0\"\n\n[dependencies]\na = \"../a\"\na = \"../b\"\n")
        .expect_err("twice");
    assert!(err.message.contains("declared twice"), "{}", err);
}

// ---- the lockfile reads back ---------------------------------------------------

#[test]
fn a_lockfile_reads_back_as_it_was_written() {
    let entries = vec![
        Locked {
            name: "a".into(),
            version: "1.2.0".into(),
            source: "../a".into(),
            hash: "ab".repeat(32),
        },
        Locked {
            name: "md".into(),
            version: "2.0.0-rc.1".into(),
            source: "https://example.com/md#v2.0.0-rc.1".into(),
            hash: "cd".repeat(32),
        },
    ];
    let back = parse_lockfile(&lockfile(&entries)).expect("reads");
    assert_eq!(back, entries);
    assert!(parse_lockfile("").expect("an empty lockfile").is_empty());
    let err = parse_lockfile("[[locked]]\nname = \"a\"\n").expect_err("incomplete");
    assert!(err.message.contains("has no `version`"), "{}", err);
}

// ---- what a path dependency's hash covers -------------------------------------

fn tree(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-hash-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    dir
}

/// A path dependency is somebody's working tree. Its `.git` and its own
/// `.kite/vendor` are not the package, and a link that cannot carry Kite —
/// `node_modules/.bin` is a directory of them — used to make the whole
/// dependency unhashable.
#[cfg(unix)]
#[test]
fn a_working_tree_hashes_as_its_kite_files() {
    let dir = tree("working");
    std::fs::write(dir.join("lib.kite"), "pub fn f() {\n}\n").expect("write");
    let before = hash_directory(&dir).expect("hashes");

    for sub in [".git/objects", "node_modules/.bin", ".kite/vendor/other"] {
        std::fs::create_dir_all(dir.join(sub)).expect("create");
    }
    std::fs::write(dir.join(".kite/vendor/other/other.kite"), "fn g() {\n}\n").expect("write");
    std::fs::write(dir.join(".git/objects/x.kite"), "fn h() {\n}\n").expect("write");
    std::os::unix::fs::symlink("/bin/sh", dir.join("node_modules/.bin/tool")).expect("link");
    std::os::unix::fs::symlink("/etc/hostname", dir.join("README")).expect("link");
    assert_eq!(hash_directory(&dir).expect("still hashes"), before);

    // A link that could carry Kite is still refused.
    std::os::unix::fs::symlink("/tmp", dir.join("elsewhere")).expect("link");
    assert!(hash_directory(&dir).is_err(), "a link to a directory");
    std::fs::remove_file(dir.join("elsewhere")).expect("unlink");
    std::os::unix::fs::symlink("/etc/hostname", dir.join("sneaky.kite")).expect("link");
    assert!(hash_directory(&dir).is_err(), "a link named .kite");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a `use` can reach, the hash covers. `node_modules` was skipped with
/// `.git` and `.kite`, and unlike them it is an identifier: a dependency's
/// `use node_modules/core` compiled bytes the lockfile never saw, and changing
/// them left `kitec pkg` reporting the lockfile unchanged.
#[cfg(unix)]
#[test]
fn what_a_use_can_reach_is_hashed() {
    let dir = tree("reachable");
    std::fs::write(dir.join("dep.kite"), "use node_modules/core\n").expect("write");
    std::fs::create_dir_all(dir.join("node_modules/.bin")).expect("create");
    std::fs::write(dir.join("node_modules/core.kite"), "pub fn f() {\n}\n").expect("write");
    let before = hash_directory(&dir).expect("hashes");
    std::fs::write(dir.join("node_modules/core.kite"), "pub fn f() {\n    io.print(1)\n}\n")
        .expect("write");
    assert_ne!(hash_directory(&dir).expect("hashes"), before, "the change was not seen");

    // A link where no `use` can go is still left alone: `.bin` and `@scope`
    // are not identifiers, and nothing below them is reachable either.
    std::os::unix::fs::symlink("/tmp", dir.join("node_modules/.bin/tmp")).expect("link");
    std::fs::create_dir_all(dir.join("node_modules/@scope")).expect("create");
    std::os::unix::fs::symlink("/tmp", dir.join("node_modules/@scope/pkg")).expect("link");
    std::os::unix::fs::symlink("/etc/hostname", dir.join("node_modules/@scope/x.kite"))
        .expect("link");
    hash_directory(&dir).expect("links out of reach are not followed");
    // One a `use` could name is refused, inside `node_modules` as anywhere.
    std::os::unix::fs::symlink("/tmp", dir.join("node_modules/lodash")).expect("link");
    assert!(hash_directory(&dir).is_err(), "a reachable link to a directory");
    let _ = std::fs::remove_dir_all(&dir);
}
