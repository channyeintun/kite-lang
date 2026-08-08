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

/// A program that prints. Today: 1,625 bytes.
///
/// The floor, and the number the whole premise rests on — a language that
/// needed a garbage collector inside its own binary could not be near it.
///
/// It was 399 bytes, and the budget was 1,024. Making `str` a language-owned
/// GC array is what moved it: every module now carries the twelve-function
/// string runtime and the three-import conversion bridge, because the module
/// exports `str` and `text` for its JavaScript API whether or not the program
/// itself ever handles text. A program with no strings in it at all is 1,619
/// bytes, which is the shape of the cost — it is the runtime's presence, not
/// this program's use of it.
///
/// Written down rather than absorbed: this is a floor that quadrupled, and
/// the answer to "what got bigger and why" is "a runtime, unconditionally".
/// Emitting only the runtime functions a program can reach would recover most
/// of it and is worth doing; the budget below is the honest number until
/// somebody does. It is deliberately not slack — 2,048 leaves room for the
/// bridge to be tuned but not for a second runtime to arrive unnoticed.
#[test]
fn hello_world_stays_small() {
    let size = size_of("hello", "fn main() {\n  io.print(\"hello\")\n}\n");
    assert!(size < 2048, "hello world is {} bytes, budget 2048", size);
}

/// A module a JavaScript project would import. Today: 1,823 bytes.
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
/// Today about 19 KB, of which roughly 4.5 KB is `std/html`'s keyed diff. It
/// was 5.7 KB when it was a counter that changed one number, and the budget has
/// moved with the demo rather than the demo being trimmed to suit the budget.
///
/// The number worth watching is the one below, which measures the language
/// rather than whatever `examples/page` happens to contain, and it has not
/// moved at all.
#[test]
fn a_dom_island_stays_under_twenty_four_kilobytes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/page/main.kite");
    let src = std::fs::read_to_string(&path).expect("read examples/page/main.kite");
    let size = size_of("island", &src);
    assert!(size < 24576, "the island is {} bytes, budget 24576", size);
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
