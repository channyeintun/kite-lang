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
