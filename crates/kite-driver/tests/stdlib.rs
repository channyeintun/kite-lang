//! The standard library's own test suite.
//!
//! Each file in `tests/std/` is an ordinary Kite program that runs its checks
//! and prints what failed. Being a program rather than a harness is what lets
//! the same file run on the bytecode VM *and* on WebAssembly and be compared: a
//! library test that only ran on one backend would not be testing the thing
//! most likely to be wrong.
//!
//! The library is written in Kite, so this is also the largest body of Kite
//! code the compiler is asked to get right.
//!
//! There was a `tests/packages/` beside this, holding the suite for the
//! `material` design system. Both are gone: a design system is a stylesheet's
//! job now, not a package's.

use kite_driver::{compile, Emit};
use std::path::{Path, PathBuf};
use std::process::Command;

fn std_tests() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/std");
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no tests/std directory at {}: {}", dir.display(), e));
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kite"))
        .collect();
    files.sort();
    files
}

fn run_on_vm(path: &Path) -> String {
    let src = std::fs::read_to_string(path).expect("read");
    let c = compile(path, &src, Emit::Check);
    assert!(
        !c.failed(),
        "{} does not compile:\n{}",
        path.display(),
        c.render_diagnostics()
    );
    let mut out = Vec::new();
    c.run(&mut out)
        .unwrap_or_else(|t| panic!("{} trapped: {}", path.display(), t));
    String::from_utf8(out).expect("utf-8")
}

#[test]
fn the_standard_librarys_tests_pass() {
    let files = std_tests();
    assert!(files.len() >= 4, "only {} test files found", files.len());
    let mut failures = Vec::new();
    for path in &files {
        let out = run_on_vm(path);
        if out.contains("FAILED") || !out.contains("0 failed") {
            failures.push(format!("{}:\n{}", path.display(), out));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Tests for modules that exist only off the web.
///
/// Not a workaround: it is the design. `std/fs` documents itself as native and
/// WASI only, and the differential comparison needs both sides to exist.
const NATIVE_ONLY: &[&str] = &["fs_test"];

/// The same files, compiled to WebAssembly and run under Node. A library that
/// passed on one backend and not the other would be a codegen bug, which is
/// the class this whole arrangement exists to find.
#[test]
fn the_standard_librarys_tests_pass_on_wasm_too() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let root = std::env::temp_dir().join(format!("kite-std-{}", std::process::id()));
    let mut mismatches = Vec::new();

    for path in &std_tests() {
        let src = std::fs::read_to_string(path).expect("read");
        let name = path.file_stem().unwrap().to_string_lossy().to_string();

        // A module the web deliberately does not have cannot be compared
        // across the two backends: there is nothing on the other side to
        // compare with. `std/fs` is native and WASI only — a page has no
        // filesystem, and the glue supplies no `fs` host precisely so that
        // asking for one fails loudly rather than reading nothing.
        if NATIVE_ONLY.contains(&name.as_str()) {
            continue;
        }

        let dir = root.join(&name);
        std::fs::create_dir_all(&dir).expect("work directory");

        let vm = run_on_vm(path);

        let c = compile(path, &src, Emit::Wasm);
        assert!(
            !c.failed(),
            "{} does not compile to wasm:\n{}",
            path.display(),
            c.render_diagnostics()
        );
        let module = c.wasm.as_ref().expect("a module");
        std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
        std::fs::write(
            dir.join("app.js"),
            // With the program's own host groups, which `kitec build` passes
            // and this did not: a module declaring `@host("…")` imports it,
            // and glue built without it cannot be instantiated at all.
            kite_driver::generate_glue_with_hosts("app.wasm", &module.hosts),
        )
        .expect("write glue");
        std::fs::write(
            dir.join("run.mjs"),
            "import { readFile } from \"node:fs/promises\";\n\
             import { run, setWriter } from \"./app.js\";\n\
             const out = [];\n\
             setWriter((l) => out.push(l));\n\
             await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
             process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n",
        )
        .expect("write runner");

        let output = Command::new("node")
            .arg(dir.join("run.mjs"))
            .output()
            .expect("node runs");
        assert!(
            output.status.success(),
            "{} failed under node:\n{}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
        let wasm = String::from_utf8(output.stdout).expect("utf-8");
        if wasm != vm {
            mismatches.push(format!("{}:\n  vm:   {:?}\n  wasm: {:?}", name, vm, wasm));
        }
        assert!(!wasm.contains("FAILED"), "{} failed on wasm:\n{}", name, wasm);
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// Every `kite` fence in the standard library's own doc comments compiles and
/// runs.
///
/// §2.3 says a doc comment's fences "are extracted and compiled as tests", and
/// for a long time nothing did it — so the examples on the reference site were
/// the one part of the library that could rot with nothing going red. This is
/// what makes the sentence true.
///
/// A fence marked ` ```kite ignore ` is an illustration and is skipped. Most of
/// the module headers are: one showing `html.mount(body, map(rows, row))` is
/// teaching a shape, and inventing a `Row` and a `body` to make it compile
/// would make it a worse explanation of the thing it exists to explain.
#[test]
fn the_standard_librarys_documentation_examples_run() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no std directory at {}: {}", dir.display(), e));
    let mut modules: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kite"))
        .collect();
    modules.sort();
    assert!(!modules.is_empty(), "no std modules found");

    let mut failures = Vec::new();
    let mut ran = 0;
    for path in &modules {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(path).expect("a std module");
        let tests = kite_driver::doctest::extract(&src, &name);
        if tests.is_empty() {
            continue;
        }
        let (augmented, names) = kite_driver::doctest::augment(&src, &tests);
        let compiled = compile(path, &augmented, Emit::Kbc);
        if compiled.failed() {
            failures.push(format!(
                "{} has a documentation example that does not compile:\n{}",
                name,
                compiled.render_diagnostics()
            ));
            continue;
        }
        for f in &names {
            let mut out = Vec::new();
            ran += 1;
            if let Err(trap) = compiled.run_named(f, &mut out) {
                failures.push(format!("{}: a documentation example trapped: {}", name, trap));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    // A guard against the extractor quietly finding nothing and the test
    // passing on an empty set, which is how a check like this dies.
    assert!(ran > 0, "no documentation examples ran");
}
