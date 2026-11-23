//! Programs whose cost once grew faster than their input, timed on each
//! backend.
//!
//! In a binary of its own, and one at a time within it, because a time is
//! only worth asserting when nothing else is running beside it: native runs
//! take a process-wide lock, so in the differential suite a native run's
//! clock also counted every other test's native run it queued behind. Only
//! running is timed, never compiling, and each bound is generous by two
//! orders of magnitude — the old cost exceeds it by as much again — so a
//! loaded machine cannot fail one and a quadratic runtime cannot pass it.

use kite_driver::{compile, Compilation, Emit};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn compiled(name: &str, src: &str, emit: Emit) -> Compilation {
    let c = compile(format!("{}.kite", name), src, emit);
    assert!(!c.failed(), "{} does not compile:\n{}", name, c.render_diagnostics());
    c
}

/// What the VM printed, and how long it ran.
fn on_vm(name: &str, src: &str) -> (String, Duration) {
    let c = compiled(name, src, Emit::Check);
    let mut out = Vec::new();
    let started = Instant::now();
    c.run(&mut out).unwrap_or_else(|t| panic!("{} trapped on the VM: {}", name, t));
    (String::from_utf8(out).expect("UTF-8"), started.elapsed())
}

/// What the native backend printed, and how long it ran — `None` where it
/// does not run.
fn on_native(name: &str, src: &str) -> Option<(String, Duration)> {
    kite_codegen_clif::supported_here().ok()?;
    let c = compiled(name, src, Emit::Native);
    let mut out = Vec::new();
    let started = Instant::now();
    c.native
        .as_ref()
        .expect("a native program")
        .run(&mut out)
        .unwrap_or_else(|e| panic!("{} failed on the native backend: {}", name, e));
    Some((String::from_utf8(out).expect("UTF-8"), started.elapsed()))
}

/// What the Wasm module printed under Node, and how long Node ran — `None`
/// where there is no Node.
fn on_wasm(name: &str, src: &str) -> Option<(String, Duration)> {
    let node = Command::new("node").arg("--version").output();
    if !node.is_ok_and(|o| o.status.success()) {
        return None;
    }
    let c = compiled(name, src, Emit::Wasm);
    let dir = std::env::temp_dir().join(format!("kite-scale-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("create work directory");
    std::fs::write(dir.join("app.wasm"), &c.wasm.as_ref().expect("a module").bytes).expect("write wasm");
    std::fs::write(dir.join("app.js"), kite_driver::generate_glue("app.wasm")).expect("write glue");
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
    let started = Instant::now();
    let output = Command::new("node").arg(dir.join("run.mjs")).output().expect("node runs");
    let took = started.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{} failed under node:\n{}", name, String::from_utf8_lossy(&output.stderr));
    Some((String::from_utf8(output.stdout).expect("UTF-8"), took))
}

/// Every backend prints `expected`, each within `bound`.
fn agrees_within(name: &str, src: &str, expected: &str, bound: Duration, wasm: bool) {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let check = |backend: &str, (out, took): (String, Duration)| {
        assert_eq!(out, expected, "{}", backend);
        assert!(took < bound, "{} took {:?}", backend, took);
    };
    check("the VM", on_vm(name, src));
    if let Some(run) = on_native(name, src) {
        check("the native backend", run);
    }
    if wasm {
        if let Some(run) = on_wasm(name, src) {
            check("wasm", run);
        }
    }
}

/// Reading a long string one character at a time is linear, on the VM and
/// natively, ASCII or not: `slice` and `code_at` walked from the front of the
/// string — natively they also counted all of it to clamp — so a loop over
/// the characters of 128 KB took minutes, and `json.parse` of a 114 KB
/// document took 12.7 s natively and 29 s on the VM. Here it is a fraction
/// of a second.
#[test]
fn a_long_string_is_read_by_character_in_linear_time() {
    // Doubling builds the string in linear time of its own.
    let src = "fn main() {\n  var s = \"abcdefgh\"\n  for i in 0..14 {\n    s = s + s\n  }\n\
               \x20 var sum = 0\n  for i in 0..s.len() {\n\
               \x20   sum = sum + s.code_at(i) + s.slice(i, i + 1).len()\n  }\n\
               \x20 let mixed = \"é\" + s\n  for i in 0..mixed.len() {\n\
               \x20   sum = sum + mixed.code_at(i) + mixed.slice(i, i + 1).len()\n  }\n\
               \x20 io.print(\"\\(s.len()) \\(sum)\")\n}\n";
    // The pieces the loops cut fill the nursery several times over, so
    // natively `mixed` moves while it is being read, and the marks found for
    // it where it was must not be used where it went. Wasm's strings are
    // arrays of characters, and never had this cost.
    agrees_within("long-string", src, "131072 26607850\n", Duration::from_secs(20), false);
}

/// The standard library's text functions are linear in their input on all
/// three backends. `join`, `lower`, `upper`, `debug_str` and the JSON and
/// TOML string readers and writers added each piece to one growing string,
/// and `split` cut the rest of its input off after every piece — each a copy
/// of everything so far, so `lower` of 100,000 characters took five seconds
/// even on Wasm, where a character is an array element, and these 200,000
/// took a minute and a half on the VM. They collect their pieces and join
/// them in pairs now, and `split` walks its input.
#[test]
fn the_standard_librarys_text_functions_are_linear() {
    let src = r#"use std/json
use std/toml

fn main() {
    var parts: [str] = []
    for i in 0..20000 {
        parts.push("Ab \"Cd\", Ef")
    }
    let s = join(parts, " ")
    let pieces = split(s, ", ")
    io.print("\(s.len()) \(pieces.len()) \(pieces[1])")
    io.print("\(lower(s).slice(0, 12)) \(upper(s).slice(0, 12))")
    io.print("\(replace(s, "Cd", "XYZ").len()) \(debug_str(s).len())")
    let encoded = json.stringify(json.Json.Text(s))
    let (back, err) = json.parse(encoded)
    if err != nil {
        io.print(err.message())
        return
    }
    io.print("\(encoded.len()) \(or_else(json.text(back), "") == s)")
    let (doc, terr) = toml.parse("v = \(encoded)\n")
    if terr != nil {
        io.print(terr.message())
        return
    }
    io.print("\(toml.text_at(doc, "v", "") == s) \(toml.emit(doc).len())")
}
"#;
    let expected = "239999 20001 Ef Ab \"Cd\"\nab \"cd\", ef  AB \"CD\", EF \n\
                    259999 280001\n280001 true\ntrue 280006\n";
    agrees_within("long-text", src, expected, Duration::from_secs(30), true);
}
