//! A function of many locals compiles in memory in proportion to them.
//!
//! In a binary of its own, because it reads the process's peak memory, which
//! any test running beside it would add to.

mod common;

use kite_codegen_clif::RunConfig;

/// The process's peak resident set, in kilobytes, as `/proc` reports it.
#[cfg(target_os = "linux")]
fn peak_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("a VmHWM line")
}

/// Twelve thousand `let`s, each an overflow-checked `+`, with a string every
/// tenth one and one made at the start that is read at the end.
///
/// A debug build splits a block at every checked `+`, and Cranelift's SSA
/// builder keeps a table per variable indexed by block number, so a variable
/// per local cost memory in the product of the two: this compiled in about
/// 600 MB, twenty thousand locals in 1.5 GB, and fifty-two thousand ran out.
/// Locals made and read within one block are carried as values now. The
/// nursery is a page, so the string read at the end has moved many times
/// while it was held only as a value.
#[cfg(target_os = "linux")]
#[test]
fn a_function_of_many_locals_compiles_in_proportion_to_them() {
    if let Err(why) = kite_codegen_clif::supported_here() {
        eprintln!("skipping: {}", why);
        return;
    }
    let n = 12_000;
    let mut src = String::from("fn main() {\n  let first = \"first \\(1)\"\n");
    for i in 0..n {
        src.push_str(&format!("  let v{} = {} + 1\n", i, i));
        if i % 10 == 0 {
            src.push_str(&format!("  let s{} = \"s \\(v{})\"\n", i, i));
        }
    }
    src.push_str(&format!("  io.print(first)\n  io.print(v{})\n  io.print(s{})\n}}\n", n - 1, n - 10));

    let before = peak_kb();
    let config = RunConfig { nursery_bytes: Some(4096), ..RunConfig::default() };
    let (native, stats) = common::run_native_with(&src, config);
    let grew = peak_kb().saturating_sub(before);
    assert_eq!(native, format!("first 1\n{}\ns {}\n", n, n - 9));
    assert!(stats.minor_collections > 10, "only {} collections", stats.minor_collections);
    assert!(grew < 300_000, "compiling {} locals took {} MB more", n, grew / 1024);
    assert_eq!(common::run_vm(&src), native);
}
