//! `str` as a real JavaScript string.
//!
//! Two representations of one type. In the default one a `str` is an `i32`
//! index into a table the glue holds, and every operation on it — including
//! `+` and `==` — is a call into JavaScript. With `--js-strings` a `str` is an
//! `externref` carrying the JS string itself, its constants are imported
//! globals the engine synthesised from the literals, and `concat` and `equals`
//! are **JS String Builtins**: intrinsics the engine compiles, not calls.
//!
//! The whole claim being tested is that a Kite program cannot tell. Every
//! program below is compiled both ways and run on the bytecode VM as well, and
//! all three outputs must be identical — which is the same bargain the rest of
//! the differential suite makes, applied to a representation rather than to a
//! backend.

use kite_driver::{compile_strings, Emit, Strings};
use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

const RUNNER: &str = "import { readFile } from \"node:fs/promises\";\n\
     import { run, setWriter } from \"./app.js\";\n\
     const out = [];\n\
     setWriter((l) => out.push(l));\n\
     await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";

fn run_under_node(name: &str, src: &str, mode: Strings) -> String {
    let tag = if mode == Strings::Builtins { "builtins" } else { "table" };
    let dir = std::env::temp_dir().join(format!("kite-str-{}-{}-{}", name, tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");

    let c = compile_strings(format!("{}.kite", name), src, Emit::Wasm, false, mode);
    assert!(
        !c.failed(),
        "{} ({}) does not compile:\n{}",
        name,
        tag,
        c.render_diagnostics()
    );
    let module = c.wasm.as_ref().expect("a module");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue_for(&module.strings, "app.wasm", &module.hosts, mode),
    )
    .expect("write glue");
    std::fs::write(dir.join("run.mjs"), RUNNER).expect("write runner");

    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{} ({}) failed under node:\n{}",
        name,
        tag,
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(output.stdout).expect("utf-8")
}

fn run_on_vm(name: &str, src: &str) -> String {
    let c = compile_strings(format!("{}.kite", name), src, Emit::Check, false, Strings::Table);
    assert!(!c.failed(), "{}:\n{}", name, c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("runs on the VM");
    String::from_utf8(out).expect("utf-8")
}

/// The programs, and what each one is for.
const PROGRAMS: &[(&str, &str)] = &[
    (
        "concat-and-compare",
        "fn main() {\n\
         \x20 let a = \"kite\"\n\
         \x20 let b = \"lang\"\n\
         \x20 io.print(a + \"-\" + b)\n\
         \x20 io.print(a == \"kite\")\n\
         \x20 io.print(a == b)\n\
         \x20 io.print(a < b)\n\
         \x20 io.print(a + b == \"kitelang\")\n}\n",
    ),
    // A string built at run time is not a constant, so it cannot be an
    // imported global — this is the case that proves `concat` really produces
    // a value the rest of the module can hold.
    (
        "strings-that-are-not-constants",
        "fn main() {\n\
         \x20 var out = \"\"\n\
         \x20 for i in 0..4 {\n    out = out + \"\\(i),\"\n  }\n\
         \x20 io.print(out)\n\
         \x20 io.print(out.len())\n\
         \x20 io.print(out.slice(2, 6))\n\
         \x20 io.print(out.index_of(\"2\"))\n}\n",
    ),
    // Characters, not UTF-16 code units. This is exactly why `length`,
    // `charCodeAt` and `substring` are *not* taken from the builtins: an
    // emoji is one character to Kite and two code units to JavaScript, and a
    // representation change must not be observable.
    (
        "characters-not-code-units",
        "fn main() {\n\
         \x20 let s = \"h\\u{e9}llo\\u{1F600}\\u{65E5}\"\n\
         \x20 io.print(s.len())\n\
         \x20 io.print(s.code_at(5))\n\
         \x20 io.print(s.slice(5, 6))\n\
         \x20 io.print(s.slice(6, 7))\n\
         \x20 io.print(s.index_of(\"\\u{65E5}\"))\n\
         \x20 io.print(hash_str(s))\n}\n",
    ),
    // Strings inside aggregates, in maps as keys, and compared structurally —
    // the generated deep-equality functions call the same operation.
    (
        "strings-inside-things",
        "struct Person {\n  name: str\n  tags: [str]\n}\n\
         fn main() {\n\
         \x20 let a = Person{ name: \"ada\", tags: [\"maths\", \"engines\"] }\n\
         \x20 let b = Person{ name: \"ada\", tags: [\"maths\", \"engines\"] }\n\
         \x20 io.print(a == b)\n\
         \x20 io.print(a.tags[1])\n\
         \x20 var m = {\"one\": 1}\n\
         \x20 m[\"two\"] = 2\n\
         \x20 io.print(join(m.keys(), \"|\"))\n\
         \x20 io.print(sorted([\"c\", \"a\", \"b\"], |x: str, y: str| x < y)[0])\n}\n",
    ),
    // The prelude, which is where most string work in a Kite program actually
    // happens.
    (
        "the-prelude-on-text",
        "fn main() {\n\
         \x20 io.print(join(split(\"a,b,c\", \",\"), \"-\"))\n\
         \x20 io.print(replace(\"banana\", \"na\", \"NA\"))\n\
         \x20 io.print(starts_with(\"kite\", \"ki\"))\n\
         \x20 io.print(upper(\"kite\"))\n\
         \x20 io.print(debug_str(\"a\\\"b\"))\n\
         \x20 io.print(if parse_int(\"42\") == nil { -1 } else { 42 })\n}\n",
    ),
];

#[test]
fn a_program_cannot_tell_which_representation_it_got() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let mut mismatches = Vec::new();
    for (name, src) in PROGRAMS {
        let vm = run_on_vm(name, src);
        let table = run_under_node(name, src, Strings::Table);
        let builtins = run_under_node(name, src, Strings::Builtins);
        if vm != table || vm != builtins {
            mismatches.push(format!(
                "{}:\n  vm:       {:?}\n  table:    {:?}\n  builtins: {:?}",
                name, vm, table, builtins
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// A module built with the builtins imports them from the engine's own
/// namespace rather than from the glue, and carries its constants as imported
/// globals rather than as a table the host hands over.
#[test]
fn the_module_says_where_its_strings_come_from() {
    let src = "fn main() {\n  io.print(\"one\" + \"two\")\n}\n";
    let plain = compile_strings("m.kite", src, Emit::Wasm, false, Strings::Table);
    let built = compile_strings("m.kite", src, Emit::Wasm, false, Strings::Builtins);
    let plain_bytes = &plain.wasm.as_ref().expect("a module").bytes;
    let built_bytes = &built.wasm.as_ref().expect("a module").bytes;

    let has = |bytes: &[u8], needle: &str| {
        bytes
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    };
    assert!(!has(plain_bytes, "wasm:js-string"), "the table build should import no builtins");
    assert!(has(built_bytes, "wasm:js-string"), "the builtins build should import them");
    assert!(has(built_bytes, "kite:strings"), "constants should arrive as imported globals");
    // The literals themselves are the import names, so they are in the module
    // either way — as a table for one and as import names for the other.
    assert!(has(built_bytes, "one"), "a constant's name is the constant");
}

/// The two import tables have to describe the same functions in the same
/// order: they are written out separately because no transform could tell a
/// boolean `i32` from a string `i32`, and separate things drift.
#[test]
fn the_two_import_tables_agree_about_what_exists() {
    let src = "fn main() {\n  io.print(\"x\")\n}\n";
    // Compiling both ways is what actually exercises the tables; a mismatch in
    // arity or order shows up as a module that does not validate, which the
    // backend's own tests check on every build.
    for mode in [Strings::Table, Strings::Builtins] {
        let c = compile_strings("m.kite", src, Emit::Wasm, false, mode);
        assert!(!c.failed(), "{}", c.render_diagnostics());
        assert!(c.wasm.is_some());
    }
}
