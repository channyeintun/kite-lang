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

/// The VM prints `expected` within `bound`.
fn vm_within(name: &str, src: &str, expected: &str, bound: Duration) {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (out, took) = on_vm(name, src);
    assert_eq!(out, expected, "the VM");
    assert!(took < bound, "the VM took {:?}", took);
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

/// A long input read between the long pieces cut from it stays linear. The
/// few strings whose characters have been indexed were let go each in turn,
/// so a loop that read each 100-character piece it cut pushed the input out
/// every fourth piece and indexed it afresh: 800,000 characters took over
/// twenty seconds on the VM and natively. The one used longest ago is let go
/// now, and the input, read every time, never is.
#[test]
fn a_long_input_read_between_its_long_pieces_stays_linear() {
    let src = "fn main() {\n  var parts: [str] = []\n  for i in 0..160000 {\n\
               \x20   parts.push(\"abcd\u{e9}fghij\")\n  }\n  let s = join(parts, \"\")\n\
               \x20 var sum = 0\n  var i = 0\n  for i + 100 <= s.len() {\n\
               \x20   let piece = s.slice(i, i + 100)\n\
               \x20   sum = sum + piece.code_at(70) + piece.len()\n    i = i + 100\n  }\n\
               \x20 io.print(\"\\(s.len()) \\(sum)\")\n}\n";
    agrees_within("long-pieces", src, "1600000 3152000\n", Duration::from_secs(20), false);
}

/// A map is a hash map, as §3.2 says: an insert, a lookup that finds its
/// key, one that does not, and a removal each cost about the same whatever
/// the map's size. Every one of them was a scan of the entries on every
/// backend, so a loop over a map of `n` entries cost `n²`: 80,000 `str` keys
/// inserted and then read back took half a minute on each. Here 100,000
/// `int` keys are inserted out of order, read back, missed, and removed from
/// the front — the removal that moved every later entry down — beside
/// 50,000 `str` keys.
#[test]
fn map_inserts_lookups_and_removals_are_constant_time() {
    let src = r#"fn main() {
    var m: {int: int} = {}
    for i in 0..100000 {
        m[i * 7919 % 100000] = i
    }
    var s = 0
    for i in 0..100000 {
        s += or_else(m[i], 0)
    }
    for i in 0..100000 {
        if m[i + 100000] == nil {
            s += 1
        }
    }
    var t: {str: int} = {}
    for i in 0..50000 {
        t["k\(i)"] = i
    }
    for i in 0..50000 {
        s += or_else(t["k\(i)"], 0)
    }
    for i in 0..100000 {
        m.remove(i)
    }
    io.print("\(s) \(m.len()) \(t.len())")
}
"#;
    agrees_within("map-ops", src, "6250025000 0 50000\n", Duration::from_secs(20), false);
}

/// A map literal of two hundred thousand entries is built in linear time:
/// each entry was looked for among those before it, so a literal of 40,000
/// took seconds on every backend, and this one half a minute on the VM.
#[test]
fn a_long_map_literal_is_built_in_linear_time() {
    let entries: Vec<String> = (0..200_000).map(|i| format!("{}: {}", i * 7919 % 200_000, i)).collect();
    let src = format!(
        "fn main() {{\n  let m = {{{}}}\n  io.print(\"\\(m.len()) \\(or_else(m[7919], -1))\")\n}}\n",
        entries.join(", ")
    );
    vm_within("map-literal", &src, "200000 1\n", Duration::from_secs(20));
}

/// A key is hashed whole, so keys that differ only far inside still spread
/// over the buckets. The hash once stopped after 32 values, and 5,000 slice
/// keys that shared their first 40 elements all landed in one bucket: a scan
/// again, 18 s on the VM and natively alike.
#[test]
fn keys_that_differ_only_deep_inside_are_still_hashed_apart() {
    let src = r#"fn key(i: int) -> [int] {
    var k: [int] = []
    for j in 0..40 {
        k.push(0)
    }
    k.push(i)
    return k
}

fn main() {
    var m: {[int]: int} = {}
    for i in 0..20000 {
        m[key(i)] = i
    }
    var s = 0
    for i in 0..20000 {
        s += or_else(m[key(i)], 0)
    }
    io.print("\(s) \(m.len())")
}
"#;
    agrees_within("map-deep-keys", src, "199990000 20000\n", Duration::from_secs(20), false);
}
