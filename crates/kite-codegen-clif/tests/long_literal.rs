//! A long literal of computed elements compiles in memory in proportion to
//! its length.
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

/// Four thousand interpolated strings in one slice literal, and a map literal
/// of two thousand computed entries, a quarter of their keys repeated.
///
/// Each element was a temporary of its own, and every one of them a collector
/// root live across every later call, so the stack maps were quadratic: 4,096
/// such strings took a gigabyte to compile, and 8,000 made more machine code
/// than the JIT could place, which panicked inside it. Built as they go, each
/// element is pushed as soon as it is made, and the next one reuses its
/// temporaries. The nursery is a page, so what was pushed has moved many
/// times by the end.
#[cfg(target_os = "linux")]
#[test]
fn a_long_literal_of_computed_elements_compiles_in_proportion() {
    if let Err(why) = kite_codegen_clif::supported_here() {
        eprintln!("skipping: {}", why);
        return;
    }
    let n = 4_000;
    let elems: Vec<String> = (0..n).map(|i| format!("\"s\\(k + {})\"", i)).collect();
    let entries: Vec<String> = (0..n / 2)
        .map(|i| format!("\"m\\((k + {}) % 1500)\": [k, {}]", i, i))
        .collect();
    let src = format!(
        "fn main() {{\n  var k = 0\n  let xs = [{}]\n  io.print(\"\\(xs.len()) \\(xs[0]) \\(xs[{}])\")\n\
         \x20 let m = {{{}}}\n  io.print(\"\\(m.len()) \\(m.keys()[0]) \\(m.values()[0][1])\")\n}}\n",
        elems.join(", "),
        n - 1,
        entries.join(", ")
    );

    let before = peak_kb();
    let config = RunConfig {
        nursery_bytes: Some(4096),
        ..RunConfig::default()
    };
    let (native, stats) = common::run_native_with(&src, config);
    let grew = peak_kb().saturating_sub(before);
    assert_eq!(native, format!("{} s0 s{}\n1500 m0 1500\n", n, n - 1));
    assert!(
        stats.minor_collections > 10,
        "only {} collections",
        stats.minor_collections
    );
    assert!(
        grew < 300_000,
        "compiling a literal of {} took {} MB more",
        n,
        grew / 1024
    );
    assert_eq!(common::run_vm(&src), native);
}
