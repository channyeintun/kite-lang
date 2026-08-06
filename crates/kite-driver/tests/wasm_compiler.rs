//! The compiler as WebAssembly, held to the compiler as a binary.
//!
//! `@kite-lang/compiler-wasm` is what a bundler depends on, and its whole
//! claim is that it is not a second compiler: same crate, different target,
//! identical output. A claim like that is worth an assertion, because the day
//! it stops being true a project's build and its author's terminal start
//! disagreeing about what the program means — and nothing else would notice.

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

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-wasmc-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    dir
}

/// The artefacts a bundler consumes, built both ways and compared byte for
/// byte — on the starter, because it is the case with a sibling module and
/// two `use std/…` imports rather than a single file.
#[test]
fn the_wasm_compiler_and_the_binary_build_identical_artefacts() {
    let compiler = kitec();
    let wasm = root().join("packages/kite-wasm/kite-compiler.wasm");
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary at {}", compiler.display());
        return;
    }
    // Built by `packages/kite-wasm/build.sh` and not checked in, the way every
    // other build artefact here is not checked in.
    if !wasm.exists() {
        eprintln!("skipping: run packages/kite-wasm/build.sh first");
        return;
    }
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: node is not installed");
        return;
    }

    let dir = work_dir("identical");
    let entry = root().join("examples/vite-starter/src/main.kite");

    let native = dir.join("native");
    let built = Command::new(&compiler)
        .args([
            "build",
            entry.to_str().unwrap(),
            "--emit",
            "wasm",
            "--out",
            native.to_str().unwrap(),
        ])
        .output()
        .expect("kitec runs");
    assert!(
        built.status.success(),
        "the native build failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let via_wasm = dir.join("wasm");
    let bin = root().join("packages/kite-wasm/kitec.js");
    let out = Command::new("node")
        .args([
            bin.to_str().unwrap(),
            "build",
            entry.to_str().unwrap(),
            "--out",
            via_wasm.to_str().unwrap(),
        ])
        .output()
        .expect("node runs");
    assert!(
        out.status.success(),
        "the WebAssembly build failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    for name in ["app.wasm", "app.js", "api.js", "api.d.ts"] {
        let a = std::fs::read(native.join(name)).unwrap_or_else(|_| panic!("native {}", name));
        let b = std::fs::read(via_wasm.join(name)).unwrap_or_else(|_| panic!("wasm {}", name));
        assert_eq!(
            a,
            b,
            "{} differs between the WebAssembly compiler and the binary ({} vs {} bytes)",
            name,
            a.len(),
            b.len()
        );
    }
}
