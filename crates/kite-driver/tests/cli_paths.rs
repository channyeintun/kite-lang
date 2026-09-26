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
