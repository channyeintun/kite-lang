//! What a module costs, as a number that fails the build when it grows.
//!
//! Under the old design a large module was an annoyance. Under this one it
//! decides whether the language is usable at all: a Kite island enhancing a
//! form on somebody's page competes with a four-kilobyte JavaScript file, and
//! nobody ships three hundred kilobytes to make a table sortable.
//!
//! WasmGC is why this is winnable. A linear-memory module ships its own
//! allocator and often a chunk of runtime; a WasmGC module ships neither,
//! because the collector belongs to the browser. Dead-code elimination does
//! most of the rest. (Identical-code-folding would do more, and is not built;
//! see docs/03, "Not built".)
//!
//! It only *stays* winnable if it is measured, which is what this file is. The
//! budgets below are generous against today's numbers on purpose: a gate that
//! fires on every ordinary change gets raised until it means nothing. These are
//! set to catch a regression of a different order — a runtime creeping in, a
//! pass that stopped pruning, a standard library module that started pulling in
//! everything else.
//!
//! **Raising a budget is a decision, not a fix.** If one of these fails, the
//! question is what got bigger and why. Write the answer down before you change
//! the number.

use kite_driver::{compile, Emit};

/// Compile to Wasm and return the module's size in bytes.
fn size_of(name: &str, src: &str) -> usize {
    let c = compile(format!("{}.kite", name), src, Emit::Wasm);
    assert!(!c.failed(), "{} does not compile:\n{}", name, c.render_diagnostics());
    c.wasm.as_ref().expect("a module").bytes.len()
}

/// A program that prints. Today: 642 bytes; it was 830 until the Wasm backend
/// stopped declaring types nothing names (see the last test).
///
/// The floor, and the number the whole premise rests on — a language that
/// needed a garbage collector inside its own binary could not be near it.
///
/// It was 399 bytes before `str` became a language-owned GC array, and 1,625
/// immediately after, because every module carried the whole twelve-function
/// string runtime whether or not the program handled text — a module
/// containing no strings at all was 1,619, which is what a cost that is about
/// presence rather than use looks like. Only the functions a program can
/// reach are emitted now; `from_host` and `to_host` stay unconditional
/// because the module exports `str` and `text` for its JavaScript API, and
/// those two call them. A program with an empty `main` is 531 bytes (735
/// before that change).
///
/// So the floor is roughly twice what it was rather than four times, and the
/// remainder is the conversion bridge the JavaScript API needs. Budget 1,024,
/// which is the old one: tight enough that the runtime cannot quietly become
/// unconditional again.
#[test]
fn hello_world_stays_small() {
    let size = size_of("hello", "fn main() {\n  io.print(\"hello\")\n}\n");
    assert!(size < 1024, "hello world is {} bytes, budget 1024", size);
}

/// A module a JavaScript project would import. Today: 865 bytes (1,069 before
/// the same change).
///
/// Four exported functions rather than one, and each `pub fn` is a real export
/// with a wrapper — which is where the difference over `hello` goes.
#[test]
fn a_library_of_four_functions_stays_small() {
    let src = "pub fn add(a: int, b: int) -> int {\n  return a + b\n}\n\
        pub fn twice(n: int) -> int {\n  return n * 2\n}\n\
        pub fn positive(n: int) -> bool {\n  return n > 0\n}\n\
        pub fn ratio(a: float, b: float) -> float {\n  return a / b\n}\n\
        fn main() {\n}\n";
    let size = size_of("lib", src);
    assert!(size < 2048, "the library is {} bytes, budget 2048", size);
}

/// A real island: five thousand rows, filtered, sorted and diffed on every
/// keystroke.
///
/// Today 28,300 bytes, against a budget that was 24,576. What got bigger,
/// measured rather than estimated — each number is this test run at that
/// commit:
///
/// 1. **1,232 bytes** (23,966 → 25,198) for the two correctness fixes in
///    `std/html`: replacing an element whose tag changed, and removing an
///    attribute a description stopped asking for. That alone put it over
///    24,576, so the budget was already failing on a clean tree before
///    anything below was written.
/// 2. **3,239 bytes** (25,198 → 28,437) for handlers on a node: the handler
///    list, one listener per element and event, the in-place patch that keeps
///    a listener reading the newest description, and the departed-key
///    collection.
/// 3. **1,178 bytes** (28,465 → 29,643) for slices that grow in place. A
///    slice became a header over a buffer with spare capacity, and `push`,
///    `xs[i] = v` and `xs[i]` became calls to a helper per element type — the
///    page pushes onto seven kinds of slice, at about a hundred bytes of
///    helper each — in exchange for `push` no longer copying the whole array
///    every time. A hundred thousand pushes took twenty seconds on this target
///    before it; the island's own lists are small, so it buys the page little
///    and costs it these bytes. The budget did not move.
/// 4. **923 bytes** (29,643 → 30,566) arrived with the standard library's
///    own round of fixes, merged after (3) and measured at the merge.
/// 5. **2,633 bytes back** (30,566 → 27,933) from the last round's changes to
///    the Wasm backend: a forward jump became a plain branch rather than a
///    trip through the dispatch loop (→ 28,438), and then overflow checks and
///    map writes became calls to shared helpers, and a local that lives within
///    one block shares a slot with others of its type.
/// 6. **540 bytes** (27,933 → 28,473) for `lower`, which the page calls on
///    every row and every query: it adds its characters to a slice and joins
///    them in pairs, where it added each to one growing string — quadratic,
///    so a 100,000-character `lower` took five seconds on this target. That
///    brings in `join` and a push onto a slice of strings.
/// 7. **388 bytes** (28,473 → 28,861) arrived with the other workstreams of
///    the same round, merged after (6) and measured at the merge; they were
///    not measured one by one.
/// 8. **561 bytes back** (28,861 → 28,300) from declaring only what the code
///    names: the structs and enums no local, signature or field can hold
///    (→ 28,843), the signatures of host imports the module does not declare
///    (→ 28,351), and the dispatchers of traits nothing implements.
///
/// `examples/page` attaches its own listeners through `std/dom` and uses none
/// of (2), so it pays for the machinery without spending it. That is the
/// honest way round for a budget to be wrong: a program that *does* use it
/// deletes a delegated dispatch table to get it, and comes out smaller than it
/// went in.
///
/// It was 5.7 KB when the demo was a counter that changed one number, and the
/// budget has always moved with the demo rather than the demo being trimmed to
/// suit the budget.
///
/// The number worth watching is the one below, which measures the language
/// rather than whatever `examples/page` happens to contain, and it has only
/// gone down.
#[test]
fn a_dom_island_stays_under_thirty_two_kilobytes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/page/main.kite");
    let src = std::fs::read_to_string(&path).expect("read examples/page/main.kite");
    let size = size_of("island", &src);
    assert!(size < 32768, "the island is {} bytes, budget 32768", size);
}

/// Importing `std/dom` does not drag the rest of the library in with it.
///
/// The specific regression this guards. `std/js` and `std/dom` are ordinary
/// Kite, so nothing stops one of them growing a dependency on `std/text` or
/// `std/json` by accident — and a program that only wanted to set a class would
/// then carry a line-breaking table. Dead-code elimination is what prevents it,
/// and this is the assertion that it still works. Today: 3,316 bytes (3,513
/// before the change the last test describes).
#[test]
fn using_the_dom_does_not_pull_in_the_whole_library() {
    let src = "use std/dom\n\n\
        fn main() {\n\
        \x20 let e = dom.find(\"#a\")\n\
        \x20 if e == nil {\n    return\n  }\n\
        \x20 let err = dom.set_class(e, \"on\", true)\n\
        \x20 if err != nil {\n    io.error(err.message())\n  }\n\
        }\n";
    let size = size_of("smalldom", src);
    assert!(size < 4096, "a class change costs {} bytes, budget 4096", size);
}

/// Importing a module costs nothing until something in it is called: a
/// `hello world` with any one standard module imported is `hello world`'s
/// size to the byte.
///
/// Dead-code elimination removed an unused module's functions, and its types
/// stayed: every struct and enum the checker knew of was declared, so `use
/// std/text` cost 550 bytes and `use std/http` 252 in a program that called
/// neither. So did a trait's dispatchers when nothing implements the trait —
/// `std/json`'s `Encode`, and the prelude's own in every program — and the
/// signature of each of the thirty-one host imports, declared or not. The
/// Wasm backend declares only what the code it emits can name now.
#[test]
fn importing_a_module_costs_nothing_until_it_is_called() {
    let hello = size_of("hello", "fn main() {\n  io.print(\"hello\")\n}\n");
    let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut modules: Vec<String> = std::fs::read_dir(&std)
        .expect("read std/")
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".kite").map(String::from))
        .filter(|m| m != "prelude")
        .collect();
    modules.sort();
    assert!(modules.len() > 10, "found only {:?}", modules);
    for m in &modules {
        let src = format!("use std/{}\n\nfn main() {{\n  io.print(\"hello\")\n}}\n", m);
        let size = size_of(m, &src);
        assert_eq!(size, hello, "`use std/{}` costs {} bytes", m, size as isize - hello as isize);
    }
}

/// The grapheme cluster table is paid for by what clusters, and by nothing
/// else in `std/text`. `clusters` follows UAX #29 as of Unicode 17.0 over one
/// Grapheme_Cluster_Break table of 1,345 runs: a program that calls it is
/// 12,806 bytes today (8,348 when it read only the mark table), and one that
/// calls only `is_combining`, which shares no table with it, is 4,669 bytes,
/// as it was before the cluster table existed.
#[test]
fn the_grapheme_table_is_paid_for_only_by_clustering() {
    let marks = size_of("marks", "use std/text\n\nfn main() {\n  io.print(text.is_combining(0x301))\n}\n");
    assert!(marks < 5120, "is_combining alone costs {} bytes, budget 5120", marks);
    let clusters = size_of("clusters", "use std/text\n\nfn main() {\n  io.print(text.clusters(\"ab\").len())\n}\n");
    assert!(clusters < 16384, "clusters costs {} bytes, budget 16384", clusters);
}
