//! What a module costs, as a number that fails the build when it grows.
//!
//! Under the old design a large module was an annoyance. Under this one it
//! decides whether the language is usable at all: a Kite island enhancing a
//! form on somebody's page competes with a four-kilobyte JavaScript file, and
//! nobody ships three hundred kilobytes to make a table sortable.
//!
//! WasmGC is why this is winnable. A linear-memory module ships its own
//! allocator and often a chunk of runtime; a WasmGC module ships neither,
//! because the collector belongs to the browser. Dead-code elimination and
//! identical-code-folding do the rest.
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

/// A program that prints. Today: 830 bytes.
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
/// those two call them. A string-free program is 735 bytes.
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

/// A module a JavaScript project would import. Today: 1,057 bytes.
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
/// Today 28,437 bytes, against a budget that was 24,576. What got bigger,
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
/// rather than whatever `examples/page` happens to contain, and it has not
/// moved at all.
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
/// and this is the assertion that it still works.
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
