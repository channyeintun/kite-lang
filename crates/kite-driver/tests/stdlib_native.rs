//! The standard library's own tests, and the shipped examples, on the native
//! backend — with the collector made to work.
//!
//! `stdlib.rs` runs `tests/std` on the bytecode VM and holds WebAssembly to
//! it; the differential suite runs its corpus natively with the default
//! nursery of a megabyte, which most of those programs never fill. This runs
//! the library's tests and the examples natively twice — once as a program
//! runs, and once with a nursery of a single page, so that the largest body of
//! Kite code there is runs with collections happening constantly — and holds
//! every run to the VM's output.
//!
//! `fs_test` is included, unlike on WebAssembly: `std/fs` is a native module,
//! and this is the test that the native runtime is the host it needs.

use kite_codegen_clif::{RunConfig, RunStats};
use kite_driver::{compile, Emit};
use std::path::{Path, PathBuf};

fn kite_files(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(dir);
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no {} directory: {}", dir.display(), e));
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kite"))
        .collect();
    files.sort();
    files
}

/// The VM's output, or `None` when the program cannot run there at all — an
/// example that declares a host neither backend has.
fn on_vm(path: &Path, src: &str) -> Option<String> {
    let c = compile(path, src, Emit::Check);
    assert!(!c.failed(), "{} does not compile:\n{}", path.display(), c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).ok()?;
    Some(String::from_utf8(out).expect("utf-8"))
}

fn on_native(path: &Path, src: &str, config: RunConfig) -> (String, RunStats) {
    let c = compile(path, src, Emit::Native);
    assert!(
        !c.failed(),
        "{} does not compile natively:\n{}",
        path.display(),
        c.render_diagnostics()
    );
    let program = c.native.as_ref().expect("a native program");
    let mut out = Vec::new();
    let stats = program
        .run_with(config, &mut out)
        .unwrap_or_else(|e| panic!("{} failed natively: {}", path.display(), e));
    (String::from_utf8(out).expect("utf-8"), stats)
}

/// The smallest nursery the runtime accepts.
const PAGE: RunConfig = RunConfig { nursery_bytes: Some(4096), major_threshold: None };

/// Run every file both ways and against the VM. Returns how many ran and how
/// many collections the small-nursery runs made between them.
fn agree_natively(files: &[PathBuf]) -> (usize, u64, Vec<String>) {
    let mut ran = 0;
    let mut collections = 0;
    let mut mismatches = Vec::new();
    for path in files {
        let src = std::fs::read_to_string(path).expect("read");
        let Some(vm) = on_vm(path, &src) else { continue };
        ran += 1;
        for config in [RunConfig::default(), PAGE] {
            let (native, stats) = on_native(path, &src, config);
            if config.nursery_bytes.is_some() {
                collections += stats.minor_collections;
            }
            if native != vm {
                mismatches.push(format!(
                    "{} (nursery {:?}):\n  vm:     {:?}\n  native: {:?}",
                    path.display(),
                    config.nursery_bytes,
                    vm,
                    native
                ));
            }
        }
    }
    (ran, collections, mismatches)
}

#[test]
fn the_standard_librarys_tests_pass_natively() {
    if let Err(why) = kite_codegen_clif::supported_here() {
        eprintln!("skipping: {}", why);
        return;
    }
    let files = kite_files("tests/std");
    assert!(
        files.iter().any(|p| p.ends_with("fs_test.kite")),
        "tests/std/fs_test.kite is gone, and it is the one this file exists for"
    );
    let (ran, collections, mismatches) = agree_natively(&files);
    assert_eq!(ran, files.len(), "every library test runs on the VM");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
    // Agreement means little if the collector never ran.
    assert!(collections > 100, "only {} collections across the library's tests", collections);

    // And the fs test passes, rather than agreeing with a VM that also failed.
    let fs = files.iter().find(|p| p.ends_with("fs_test.kite")).unwrap();
    let src = std::fs::read_to_string(fs).expect("read");
    let (out, _) = on_native(fs, &src, RunConfig::default());
    assert!(out.contains(" 0 failed") && !out.contains("FAILED"), "{}", out);
}

#[test]
fn every_example_agrees_natively_with_a_page_of_nursery() {
    if let Err(why) = kite_codegen_clif::supported_here() {
        eprintln!("skipping: {}", why);
        return;
    }
    let files = kite_files("examples");
    let (ran, _, mismatches) = agree_natively(&files);
    assert!(ran >= 8, "only {} examples ran on the VM", ran);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}
