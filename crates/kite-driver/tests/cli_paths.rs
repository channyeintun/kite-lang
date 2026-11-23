//! Naming a file, and finding what is beside it.
//!
//! A Kite module is a *directory*, so what the compiler can see depends on
//! which directory it decided the program is in — and that is derived from the
//! path it was handed. The two ways of naming one file have to agree.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root")
}

fn kitec() -> PathBuf {
    // `cargo test` puts the test binary next to the compiler it built.
    let mut path = std::env::current_exe().expect("this test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "kitec.exe" } else { "kitec" })
}

/// A bare filename has no parent directory, and the module loader once read
/// that as "this program has no directory" — so `kitec check main.kite` from
/// inside a source directory could not see its own siblings, while
/// `kitec check src/main.kite` from the parent could. The starter has a
/// sibling module, which makes it the case that catches this.
#[test]
fn a_program_finds_its_siblings_when_named_without_a_directory() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary at {}", compiler.display());
        return;
    }
    let src = root().join("examples/vite-starter/src");

    let bare = Command::new(&compiler)
        .current_dir(&src)
        .args(["check", "main.kite"])
        .output()
        .expect("kitec runs");
    let qualified = Command::new(&compiler)
        .current_dir(src.join(".."))
        .args(["check", "src/main.kite"])
        .output()
        .expect("kitec runs");

    assert!(
        bare.status.success(),
        "`kitec check main.kite` from inside the directory failed:\n{}{}",
        String::from_utf8_lossy(&bare.stdout),
        String::from_utf8_lossy(&bare.stderr),
    );
    assert_eq!(
        bare.status.success(),
        qualified.status.success(),
        "naming the same file two ways gave two answers",
    );
}

/// A scratch directory with one file in it.
fn scratch(name: &str, file: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-cli-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    std::fs::write(dir.join(file), text).expect("write");
    dir
}

/// Run `kitec` in `dir`, answering with its exit status, stdout and stderr.
fn kitec_in(dir: &Path, args: &[&str]) -> Option<(bool, String, String)> {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary at {}", compiler.display());
        return None;
    }
    let out = Command::new(&compiler).current_dir(dir).args(args).output().expect("kitec runs");
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

// ---- `kitec test` runs tests, and only tests ----------------------------------

/// Discovery used to be every compiled function whose name began `test_`: a
/// closure lifted out of a test (`test_x#closure0`) was called with nothing
/// and trapped, a helper taking an argument was called and "passed", and a
/// private test — pruned as unreachable — was silently not there to find.
#[test]
fn kitec_test_runs_tests_and_only_tests() {
    let dir = scratch(
        "tests-only",
        "t.kite",
        "use std/test\n\n\
         pub fn test_with_closure() -> (int, error) {\n    let f = |x: int| x + 1\n    \
         return f(1), nil\n}\n\n\
         pub fn test_helper_takes(n: int) -> int {\n    return n\n}\n\n\
         fn test_private() -> (int, error) {\n    return 0, errors.new(\"private test failed\")\n}\n",
    );
    let Some((ok, out, err)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(!ok, "the private test fails, so the run fails:\n{}{}", out, err);
    assert!(out.contains("ok       test_with_closure"), "{}", out);
    assert!(!out.contains("#closure"), "a lifted closure is not a test:\n{}", out);
    assert!(out.contains("FAILED   test_private"), "a private test is run:\n{}", out);
    assert!(!out.contains("ok       test_helper_takes"), "{}", out);
    assert!(err.contains("`test_helper_takes` takes 1 argument"), "{}", err);
    assert!(out.contains("1 passed, 1 failed"), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Called by name, an `async fn` answers with its task. Reading the task as
/// the result reported every async test as passing, and the resume half,
/// `test_x$resume`, was run as a test of its own and trapped.
#[test]
fn an_async_test_is_driven_and_its_answer_read() {
    let dir = scratch(
        "async-test",
        "t.kite",
        "use std/task\n\n\
         pub async fn test_async_fails() -> (int, error) {\n    await task.sleep(1)\n    \
         return _, errors.new(\"this test should fail\")\n}\n\n\
         pub async fn test_async_passes() -> (int, error) {\n    await task.sleep(1)\n    \
         return 1, nil\n}\n",
    );
    let Some((ok, out, _)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(!ok, "{}", out);
    assert!(out.contains("FAILED   test_async_fails\n         this test should fail"), "{}", out);
    assert!(out.contains("ok       test_async_passes"), "{}", out);
    assert!(!out.contains("$resume"), "{}", out);
    assert!(out.contains("1 passed, 1 failed"), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A doc example fails by trapping on an `assert`, which a release build
/// drops. Under `--release` every wrong example passed.
#[test]
fn a_doc_example_is_checked_under_release_too() {
    let dir = scratch(
        "doc-release",
        "lib.kite",
        "/// Doubles.\n///\n/// ```kite\n/// assert(double(2) == 5, \"wrong\")\n/// ```\n\
         pub fn double(n: int) -> int {\n    return n * 2\n}\n",
    );
    let Some((ok, out, _)) = kitec_in(&dir, &["test", "lib.kite", "--release"]) else { return };
    assert!(!ok, "the example's claim is false:\n{}", out);
    assert!(out.contains("TRAPPED"), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- an option belongs to the commands that take it ------------------------

/// Options were read before the command was, so `kitec test --native` wrote an
/// object file and exited 0 without running a test.
#[test]
fn an_option_the_command_does_not_take_is_refused() {
    let dir = scratch(
        "flags",
        "t.kite",
        "pub fn test_one() -> (int, error) {\n    return 1, nil\n}\n\n\
         fn main() {\n    io.print(\"main ran\")\n}\n",
    );
    for args in [
        &["test", "t.kite", "--native"][..],
        &["check", "t.kite", "--native"],
        &["test", "t.kite", "--emit", "wasm"],
        &["check", "t.kite", "--emit", "wasm"],
        &["run", "t.kite", "--emit", "wasm"],
        &["fmt", "t.kite", "--release"],
    ] {
        let Some((ok, _, err)) = kitec_in(&dir, args) else { return };
        assert!(!ok, "`kitec {}` should be refused", args.join(" "));
        assert!(err.contains("error:"), "`kitec {}` said: {}", args.join(" "), err);
    }
    assert!(!dir.join("app.o").exists(), "nothing is written by a refused command");
    assert!(!dir.join("app.wasm").exists(), "nothing is written by a refused command");

    // Printing a stage is the whole of what `run --emit hir` asked for.
    let Some((ok, out, err)) = kitec_in(&dir, &["run", "t.kite", "--emit", "hir"]) else { return };
    assert!(ok, "{}", err);
    assert!(out.contains("fn main()"), "the stage is printed:\n{}", out);
    assert!(!out.lines().any(|l| l == "main ran"), "the program was not asked to run:\n{}", out);
    assert!(!err.contains("has no `main`"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- `kitec fix` makes only the edits it is sure of --------------------------

/// `kitec fix` applies a fix only where the fix is certain, and not at all to
/// a file the lexer could not read.
///
/// A `)` missing at the end of a line made every line after it an argument
/// short of a comma, each with a fix, and `kitec fix` wrote `xs.push(2,` and
/// `xs.push(3),`. A `$` dropped from `f(1 $ 2)` left `1 2`, and the fix wrote
/// `f(1, $ 2)`.
#[test]
fn kitec_fix_edits_only_what_it_is_sure_of() {
    let unclosed = "fn main() {\n    var xs = [1]\n    xs.push(2\n    xs.push(3)\n    io.print(xs.len())\n}\n";
    let dir = scratch("fix-unclosed", "t.kite", unclosed);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    // Nothing was fixable, and the file still fails: that is not a success.
    assert!(!ok, "{}", err);
    assert!(err.contains("nothing to fix"), "{}", err);
    assert_eq!(std::fs::read_to_string(dir.join("t.kite")).expect("read"), unclosed);
    let _ = std::fs::remove_dir_all(&dir);

    // A comma between arguments is a guess, so it is said and not written.
    let guess = "fn main() {\n    let n = 1\n    io.print(\"n is \" n)\n}\n";
    let dir = scratch("fix-guess", "t.kite", guess);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    // Nothing was fixable, and the file still fails: that is not a success.
    assert!(!ok, "{}", err);
    assert!(err.contains("nothing to fix"), "{}", err);
    assert_eq!(std::fs::read_to_string(dir.join("t.kite")).expect("read"), guess);
    let _ = std::fs::remove_dir_all(&dir);

    // A file with a lexical error is refused whole, even for the fix it
    // would otherwise have had.
    let lexical = "fn f(a: int b: int) -> int {\n    return a + b\n}\n\n\
                   fn main() {\n    io.print(f(1 $ 2))\n}\n";
    let dir = scratch("fix-lexical", "t.kite", lexical);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    assert!(!ok, "a file with lexical errors is refused:\n{}", err);
    assert!(err.contains("lexical errors"), "{}", err);
    assert_eq!(std::fs::read_to_string(dir.join("t.kite")).expect("read"), lexical);
    let _ = std::fs::remove_dir_all(&dir);

    // A comma between parameters is certain, and is written.
    let signature = "fn add(a: int b: int) -> int {\n    return a + b\n}\n\n\
                     fn main() {\n    io.print(add(1, 2))\n}\n";
    let dir = scratch("fix-signature", "t.kite", signature);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    assert!(ok, "{}", err);
    let fixed = std::fs::read_to_string(dir.join("t.kite")).expect("read");
    assert!(fixed.starts_with("fn add(a: int, b: int) -> int {"), "{}", fixed);
    let Some((ok, out, err)) = kitec_in(&dir, &["run", "t.kite"]) else { return };
    assert!(ok, "{}", err);
    assert_eq!(out, "3\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `kitec fix` removes every stray comma of a struct in one run, and says what
/// it left. It removed the first and reported "applied 1 fix" until the third
/// run, and after a fix it exited 0 whatever was still wrong with the file.
#[test]
fn kitec_fix_finishes_the_job_and_reports_what_is_left() {
    let commas = "struct P {\n    x: int,\n    y: int,\n    z: int\n}\n\nfn main() {\n    let p = P{ x: 1, y: 2, z: 3 }\n    io.print(p.x)\n}\n";
    let dir = scratch("fix-commas", "t.kite", commas);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    assert!(ok, "{}", err);
    assert!(err.contains("applied 2 fixes"), "{}", err);
    let fixed = std::fs::read_to_string(dir.join("t.kite")).expect("read");
    let declaration = fixed.lines().take(5).collect::<Vec<_>>().join("\n");
    assert!(!declaration.contains(','), "{}", declaration);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    assert!(ok && err.contains("nothing to fix"), "a second run changes nothing: {}", err);
    let _ = std::fs::remove_dir_all(&dir);

    // A fix that leaves another error behind says so, and fails.
    let left = "enum E {\n    A,\n    B\n}\n\nfn main() {\n    let n: int = \"s\"\n    io.print(n)\n}\n";
    let dir = scratch("fix-left", "t.kite", left);
    let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("applied 1 fix"), "{}", err);
    assert!(err.contains("E0200"), "the error that is left is shown: {}", err);
    assert!(err.contains("error left that `kitec fix` cannot fix"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A comma between two members on one line is fixed too: it becomes the line
/// break it stands for. It had no fix — removing it alone gives `x: int y:
/// int`, another error — so `kitec fix` left it, and on a one-line struct
/// said "nothing to fix" to a label reading "remove this comma".
#[test]
fn kitec_fix_puts_members_sharing_a_line_on_lines_of_their_own() {
    for (name, src) in [
        ("fix-one-line", "struct P { x: int, y: int }\nfn main() { io.print(P{ x: 1, y: 2 }.x) }\n"),
        (
            "fix-mid-line",
            "struct P {\n    x: int, y: int,\n    z: int,\n    w: int\n}\n\nfn main() {\n    let p = P{ x: 1, y: 2, z: 3, w: 4 }\n    io.print(p.x)\n}\n",
        ),
        ("fix-enum-line", "enum E { A, B(int), C }\n\nfn main() {\n    io.print(1)\n}\n"),
        // And a comma missing after `self` or between closure parameters.
        (
            "fix-self-comma",
            "struct S {\n    n: int\n}\n\nimpl S {\n    fn m(self a: int) -> int {\n        return self.n + a\n    }\n}\n\nfn main() {\n    let f = |x: int y: int| x + y\n    io.print(S{ n: 1 }.m(2) + f(3, 4))\n}\n",
        ),
    ] {
        let dir = scratch(name, "t.kite", src);
        let Some((ok, _, err)) = kitec_in(&dir, &["fix", "t.kite"]) else { return };
        assert!(ok, "{}: {}", name, err);
        assert!(!err.contains("cannot fix"), "{}: {}", name, err);
        let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
        assert!(ok, "{} still fails after kitec fix: {}", name, err);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// An error inside a doc example is reported at the line of the `///` comment
/// it was written on. The example is compiled appended to its module, and the
/// span used to be printed as the line it landed on there: line 12 of an
/// eight-line file.
#[test]
fn an_error_in_a_doc_example_points_at_the_line_it_was_written_on() {
    let src = "/// Doubles.\n///\n/// ```kite\n/// let y: int = \"s\"\n/// ```\npub fn double(n: int) -> int {\n    return n * 2\n}\n";
    let dir = scratch("doc-error-line", "t.kite", src);
    let Some((ok, out, err)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(!ok, "{}{}", out, err);
    let shown = format!("{}{}", out, err);
    assert!(shown.contains("t.kite:4:18"), "the fence's own line and column: {}", shown);
    assert!(shown.contains("4 │ /// let y: int = \"s\""), "the real line is shown: {}", shown);
    assert!(!shown.contains("t.kite:12"), "no line past the file's end: {}", shown);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One bad fence fails only itself. The fences used to be compiled as one
/// unit, so a fence that mixes a declaration with a statement (which does not
/// parse) marked every example in the file FAILED.
#[test]
fn a_bad_doc_example_fails_alone() {
    let src = "/// One.\n///\n/// ```kite\n/// assert(one() == 1, \"one\")\n/// ```\npub fn one() -> int {\n    return 1\n}\n\n\
/// Mixed.\n///\n/// ```kite\n/// fn helper() -> int { return 2 }\n/// assert(helper() == 2, \"two\")\n/// ```\npub fn two() -> int {\n    return 2\n}\n\n\
/// Three.\n///\n/// ```kite\n/// assert(three() == 3, \"three\")\n/// ```\npub fn three() -> int {\n    return 3\n}\n";
    let dir = scratch("doc-alone", "t.kite", src);
    let Some((ok, out, err)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(!ok, "{}{}", out, err);
    assert!(out.contains("FAILED   ./t.kite:12 (doc example)"), "{}", out);
    assert!(out.contains("ok       ./t.kite:3 (doc example)"), "{}", out);
    assert!(out.contains("ok       ./t.kite:22 (doc example)"), "{}", out);
    assert!(out.contains("2 passed, 1 failed"), "{}", out);
    assert!(!out.contains("FAILED   t.kite:3 "), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fence that uses what another fence declares is not blamed for a third
/// fence's mistake: compiled without the bad one, it still has the helper it
/// calls. And of two fences that declare one name, only the second fails.
#[test]
fn a_fence_that_uses_another_is_not_blamed_for_a_third() {
    let src = "/// A.\n///\n/// ```kite\n/// fn helper() -> int {\n///     return 2\n/// }\n/// ```\npub fn one() -> int {\n    return 1\n}\n\n\
/// B.\n///\n/// ```kite\n/// assert(helper() == 2, \"two\")\n/// ```\npub fn two() -> int {\n    return 2\n}\n\n\
/// C.\n///\n/// ```kite\n/// assert(one() == \"x\", \"bad\")\n/// ```\npub fn three() -> int {\n    return 3\n}\n\n\
/// D.\n///\n/// ```kite\n/// fn helper() -> int {\n///     return 3\n/// }\n/// ```\npub fn four() -> int {\n    return 4\n}\n";
    let dir = scratch("doc-uses", "t.kite", src);
    let Some((ok, out, err)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(!ok, "{}{}", out, err);
    assert!(out.contains("ok       ./t.kite:3 (doc example, compiled)"), "{}", out);
    assert!(out.contains("ok       ./t.kite:14 (doc example)"), "{}", out);
    assert!(out.contains("FAILED   ./t.kite:23 (doc example)"), "{}", out);
    assert!(out.contains("FAILED   ./t.kite:32 (doc example)"), "{}", out);
    assert!(out.contains("2 passed, 2 failed"), "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file with no tests says so in one line and still succeeds. The line used
/// to describe a signature, `-> (int, error)`, that a test does not need.
#[test]
fn a_file_with_no_tests_says_so_in_one_line() {
    let dir = scratch("no-tests", "t.kite", "fn main() {\n}\n");
    let Some((ok, _, err)) = kitec_in(&dir, &["test", "t.kite"]) else { return };
    assert!(ok, "{}", err);
    assert_eq!(err.trim().lines().count(), 1, "{}", err);
    assert!(err.contains("no tests in"), "{}", err);
    assert!(err.contains("`fn test_…()` taking no arguments"), "{}", err);
    assert!(!err.contains("(int, error)"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `kitec doc` on a file that does not parse says what is wrong and fails. It
/// printed "Nothing is exported from this module." and exited 0, which is the
/// reference of a file whose one declaration was the one with the error.
#[test]
fn kitec_doc_refuses_a_file_that_does_not_parse() {
    let dir = scratch("doc-syntax", "t.kite", "pub fn f( {\n}\n");
    let Some((ok, out, err)) = kitec_in(&dir, &["doc", "t.kite"]) else { return };
    assert!(!ok, "{}{}", out, err);
    assert!(err.contains("E0100"), "{}", err);
    assert!(err.contains("does not parse"), "{}", err);
    assert!(out.is_empty(), "no reference is printed: {}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every option is listed under `OPTIONS:`, together. Two changes merged into
/// the help at once left a sentence in the middle of the list, and `--update`,
/// `--explain`, `--version` and `--help` below it, cut off from the rest.
#[test]
fn the_help_lists_every_option_under_options() {
    let dir = scratch("help", "t.kite", "");
    let Some((ok, out, _)) = kitec_in(&dir, &["--help"]) else { return };
    assert!(ok);
    let options: Vec<&str> = out
        .lines()
        .skip_while(|l| *l != "OPTIONS:")
        .skip(1)
        .take_while(|l| !l.is_empty())
        .collect();
    let listed = |flag: &str| options.iter().any(|l| l.trim_start().starts_with(flag));
    for flag in ["--release", "--out", "--update", "--explain", "--version", "--help"] {
        assert!(listed(flag), "`{}` is not under OPTIONS:\n{}", flag, out);
    }
    assert!(
        options.iter().all(|l| l.starts_with("    ")),
        "something that is not an option is in the list:\n{}",
        out
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the source map names what a browser can find ---------------------------

/// A source map names each source relative to the directory it is written
/// into, never by an absolute path, and carries every source's text.
///
/// `kitec build src/main.kite --out dist` wrote `"sources": ["src/main.kite"]`,
/// which a browser resolves against the map's own URL — `dist/src/main.kite`,
/// a 404. An absolute input path went into the artefact as it was, naming the
/// builder's machine, and the standard library appeared as `<std/http>` with
/// nothing behind it. Now the map says `../src/main.kite`, the library's
/// modules are `kite-std/…`, and `sourcesContent` means none of it has to be
/// fetched at all.
#[test]
fn a_source_map_names_sources_where_a_browser_finds_them() {
    let dir = scratch("sourcemap", "unused.kite", "");
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(
        dir.join("src/main.kite"),
        "use std/json\n\nfn main() {\n    io.print(json.stringify(json.Json.Null))\n}\n",
    )
    .expect("write");
    let absolute = dir.join("src/main.kite");
    for entry in ["src/main.kite", absolute.to_str().unwrap()] {
        let _ = std::fs::remove_dir_all(dir.join("dist"));
        let Some((ok, _, err)) =
            kitec_in(&dir, &["build", entry, "--emit", "wasm", "--out", "dist"])
        else {
            return;
        };
        assert!(ok, "{}", err);
        let map = std::fs::read_to_string(dir.join("dist/app.wasm.map")).expect("a map");
        assert!(
            map.contains("\"sources\":[\"../src/main.kite\","),
            "built from {}: {}",
            entry,
            &map[..map.len().min(200)]
        );
        assert!(map.contains("\"kite-std/json.kite\""), "{}", &map[..map.len().min(300)]);
        assert!(map.contains("\"sourcesContent\":[\"use std/json\\n"), "{}", &map[..map.len().min(300)]);
        let home = dir.to_string_lossy().replace('\\', "/");
        assert!(!map.contains(&home), "the builder's path leaked into the map");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A source reached through a symbolic link is named relative to the map as
/// it really is on disk.
///
/// macOS's temporary directory is `/var/…` by one spelling and
/// `/private/var/…` by the other, and a process's working directory comes
/// back as the second. Comparing an input path given as the first with an
/// output directory resolved as the second climbed to the root and down
/// again: `../../../../../../../../var/folders/…/src/main.kite`. A link makes
/// the same two spellings anywhere, so this holds on every Unix, not only on
/// the one where it was found.
#[cfg(unix)]
#[test]
fn a_source_reached_through_a_link_is_named_as_it_is_on_disk() {
    let dir = scratch("sourcemap-link", "unused.kite", "");
    std::fs::create_dir_all(dir.join("real/src")).expect("src");
    std::fs::write(dir.join("real/src/main.kite"), "fn main() {\n    io.print(1)\n}\n").expect("write");
    std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).expect("link");
    let through_link = dir.join("link/src/main.kite");
    let Some((ok, _, err)) = kitec_in(
        &dir.join("real"),
        &["build", through_link.to_str().unwrap(), "--emit", "wasm", "--out", "dist"],
    ) else {
        return;
    };
    assert!(ok, "{}", err);
    let map = std::fs::read_to_string(dir.join("real/dist/app.wasm.map")).expect("a map");
    assert!(map.contains("\"sources\":[\"../src/main.kite\""), "{}", &map[..map.len().min(200)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program whose `kite.toml` is above its entry names its imported modules'
/// files as the entry was named: relative to where `kitec` runs.
///
/// The package's root became the manifest's directory, spelled from the top,
/// so every diagnostic in a module beside `src/main.kite` named its file by
/// an absolute path, while the entry's own were relative.
#[test]
fn a_module_beside_an_entry_below_its_manifest_is_named_relatively() {
    let dir = scratch("manifest-above", "kite.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(dir.join("src/main.kite"), "use money\n\nfn main() {\n    io.print(money.one())\n}\n")
        .expect("write");
    std::fs::write(dir.join("src/money.kite"), "pub fn one() -> int {\n    return \"one\"\n}\n").expect("write");
    let Some((ok, _, from_above)) = kitec_in(&dir, &["check", "src/main.kite"]) else { return };
    assert!(!ok);
    assert!(from_above.contains("─ src/money.kite:2:"), "{}", from_above);
    let Some((_, _, from_inside)) = kitec_in(&dir.join("src"), &["check", "main.kite"]) else { return };
    assert!(from_inside.contains("─ ../src/money.kite:2:"), "{}", from_inside);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A note about a prelude trait names it as a program writes it: `Hash`, not
/// the `prelude.Hash` it is held as.
#[test]
fn a_note_about_a_prelude_trait_does_not_say_prelude() {
    let dir = scratch(
        "prelude-trait",
        "t.kite",
        "struct P {\n    a: int\n}\n\nfn s1<T: Hash>(x: T) {\n}\n\nfn s2<T: Display>(x: T) {\n}\n\n\
         fn d1(x: dyn Debug) {\n}\n\nfn d2(x: dyn Display) {\n    io.print(x.nothing())\n}\n\n\
         fn main() {\n    s1(P{ a: 1 })\n    s2(P{ a: 1 })\n    d1(P{ a: 1 })\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("`P` does not implement `Hash`"), "{}", err);
    assert!(err.contains("`dyn Display` has no method `nothing`"), "{}", err);
    assert!(!err.contains("prelude."), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// After `use std/errors as errs`, the module's old name is not one this file
/// knows, and the error says what it is called here. The member that is
/// missing is named under the alias too.
#[test]
fn a_module_used_by_its_old_name_says_what_it_is_called_here() {
    let dir = scratch(
        "alias-old-name",
        "t.kite",
        "use std/errors as errs\nuse std/text as t\nuse std/json as j\n\nfn f(v: json.Json) {\n}\n\n\
         fn main() {\n    let g = errors.wrap(errs.new(\"x\"), \"c\")\n    \
         io.print(text.upper(\"a\"))\n    io.print(errs.nothing(g))\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("`errors` is imported here as `errs`"), "{}", err);
    assert!(err.contains("`text` is imported here as `t`"), "{}", err);
    assert!(err.contains("cannot find `nothing` in module `errs`"), "{}", err);
    // A type through the old name, as a parameter's type or a literal's.
    assert!(err.contains("`json` is imported here as `j`, so this file writes `j.Json`"), "{}", err);
    assert!(!err.contains("is that module imported?"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `(f(1), f(2)).0` as a statement is a projection, and its label does not
/// call it a call.
#[test]
fn a_projected_tuple_element_is_not_called_a_call() {
    let dir = scratch(
        "projection-label",
        "t.kite",
        "fn f(n: int) -> (int, error) {\n    return n, nil\n}\n\nfn main() {\n    (f(1), f(2)).0\n    ((f(3), f(4)).0)\n    \
         match 1 {\n        1 => (f(5), f(6)).0\n        _ => {}\n    }\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("this tuple element holds a value and an error"), "{}", err);
    // In parentheses, and as the arm of a statement `match`, it is the same.
    assert_eq!(err.matches("this tuple element holds").count(), 3, "{}", err);
    assert!(!err.contains("this call returns"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tuple or slice literal that holds failures, left as a statement, is
/// called what it is: no call is there.
#[test]
fn a_literal_that_holds_failures_is_not_called_a_call() {
    let dir = scratch(
        "literal-label",
        "t.kite",
        "fn f(n: int) -> (int, error) {\n    return n, nil\n}\n\nfn g() -> error {\n    return nil\n}\n\n\
         fn main() {\n    (f(1), f(2))\n    ((f(3), f(4)))\n    [g(), g()]\n    \
         match 1 {\n        1 => (g(), 1)\n        _ => [f(5)]\n    }\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert_eq!(err.matches("this tuple holds errors, and they are thrown away").count(), 3, "{}", err);
    assert_eq!(err.matches("this slice holds errors, and they are thrown away").count(), 2, "{}", err);
    assert!(!err.contains("this call returns"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A method call that fails on a local whose name is a module this file
/// imports, or a builtin one, says the local hides the module — only where
/// the module has the member: `text.foo()` gets no such note.
#[test]
fn a_local_that_hides_a_module_says_so() {
    let dir = scratch(
        "local-hides-module",
        "t.kite",
        "use std/text\n\nfn main() {\n    let text = \"ab\"\n    io.print(text.clusters(text).len())\n    \
         io.print(text.foo())\n    let io = 3\n    io.print(1)\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert_eq!(err.matches("this local `text` hides the module `text`").count(), 1, "{}", err);
    assert!(err.contains("`str` has no method `foo`"), "{}", err);
    assert!(err.contains("this local `io` hides the module `io`; to use the module, rename the local"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
    // The recipe: the local renamed, the call is the module's.
    let dir = scratch(
        "local-hides-module-fixed",
        "t.kite",
        "use std/text\n\nfn main() {\n    let s = \"ab\"\n    io.print(text.clusters(s).len())\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(ok, "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Calling a local that hides a prelude function says so, as a module's
/// constant of that name does.
#[test]
fn a_local_that_hides_a_prelude_function_says_so() {
    let dir = scratch(
        "local-hides",
        "t.kite",
        "fn main() {\n    let max = 5\n    let size = 2\n    io.print(max(1, 2))\n    io.print(size(1))\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("this local `max` hides the prelude's function `max`"), "{}", err);
    assert!(!err.contains("local `size` hides"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
    // Where the module declares its own `max`, that is what the local hides.
    let dir = scratch(
        "local-hides-own",
        "t.kite",
        "fn max(a: int, b: int) -> int {\n    return a\n}\n\nfn main() {\n    let max = 5\n    io.print(max(1, 2))\n}\n",
    );
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "t.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("this local `max` hides this module's function `max`"), "{}", err);
    assert!(!err.contains("prelude"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program given by a relative path has its manifests and its dependencies'
/// files named relative too, and folded: `E0405`'s title printed the
/// manifest's absolute path, and a dependency's file was `…/app/../a/a.kite`.
#[test]
fn a_dependency_and_its_manifest_are_named_as_the_entry_is() {
    let dir = scratch("dep-paths", "unused.kite", "");
    for (rel, text) in [
        ("a/a.kite", "pub fn hello() -> int {\n    return \"s\"\n}\n"),
        ("a/kite.toml", "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"),
        ("app/kite.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n"),
        ("app/main.kite", "use a\n\nfn main() {\n    io.print(a.hello())\n}\n"),
    ] {
        std::fs::create_dir_all(dir.join(rel).parent().unwrap()).expect("dir");
        std::fs::write(dir.join(rel), text).expect("write");
    }
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "app/main.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("─ a/a.kite:2:"), "folded and relative: {}", err);
    assert!(!err.contains(".."), "{}", err);
    // The same package with a manifest that does not read.
    std::fs::write(dir.join("a/kite.toml"), "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n").expect("write");
    let Some((ok, _, err)) = kitec_in(&dir, &["check", "app/main.kite"]) else { return };
    assert!(!ok, "{}", err);
    assert!(err.contains("E0405"), "{}", err);
    assert!(err.contains("`a/kite.toml` is not a manifest"), "title is relative and folded: {}", err);
    assert!(!err.contains(dir.to_string_lossy().as_ref()), "no absolute path: {}", err);
    let _ = std::fs::remove_dir_all(&dir);
}
