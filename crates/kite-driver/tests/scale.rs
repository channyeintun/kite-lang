//! Programs whose cost once grew faster than their input, timed on each
//! backend.
//!
//! In a binary of its own, and one at a time within it, because a time is
//! only worth asserting when nothing else is running beside it: native runs
//! take a process-wide lock, so in the differential suite a native run's
//! clock also counted every other test's native run it queued behind. Only
//! running is timed, never compiling — except where the checking was what
//! grew, and then only the checking — and each bound is generous by two
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
/// 50,000 `str` keys. On Wasm, where every write also copied the map, this
/// did not finish in a minute; it takes about a tenth of a second now.
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
    agrees_within("map-ops", src, "6250025000 0 50000\n", Duration::from_secs(20), true);
}

/// `toml.parse` reads a document of many keys in linear time. Each plain
/// `key = value` line went through a function that took the table, wrote the
/// key into it and handed it back, and a map handed to a function is shared
/// with the caller's, so every key copied the whole table: 30,000 keys took
/// 32 s on the VM and natively and 6 s on Wasm, and these 40,000 about twice
/// that. The section being read is held in a local now, and written in place.
#[test]
fn a_toml_document_of_many_keys_parses_in_linear_time() {
    let src = r#"use std/toml

fn main() {
    var lines: [str] = ["first = 0", "[section.inner]"]
    for i in 0..40000 {
        lines.push("key\(i) = \"value \(i)\"")
    }
    let (doc, err) = toml.parse(join(lines, "\n"))
    if err != nil {
        io.print(err.message())
        return
    }
    io.print("\(toml.text_at(doc, "section.inner.key39999", "?")) \(toml.at(doc, "first") == toml.Toml.Int(0))")
}
"#;
    agrees_within("toml-keys", src, "value 39999 true\n", Duration::from_secs(20), true);
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

/// Checking is linear in the arms of a generated `match` and in the links of
/// a field-access chain — the one place here where it is checking that is
/// timed, since that is where the cost was. Each `match` arm was looked up
/// by a scan of the enum's variants, asked about against every arm above
/// it, and the coverage of every constructor found by a pass over every
/// row; each link of `x.a.a…` spelled out the whole chain to ask whether it
/// was a dotted name. These took about a minute together; now a second.
#[test]
fn a_large_match_and_a_long_field_chain_are_checked_in_linear_time() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let n = 20_000;
    let variants: String = (0..n).map(|i| format!("  V{}\n", i)).collect();
    let arms: String = (0..n).map(|i| format!("    V{} => {},\n", i, i)).collect();
    let by_enum = format!("enum E {{\n{}}}\nfn f(e: E) -> int {{\n  return match e {{\n{}  }}\n}}\n\
                           fn main() {{\n  io.print(f(V7))\n}}\n", variants, arms);
    let arms: String = (0..n).map(|i| format!("    {} => {},\n", i, i)).collect();
    let by_int = format!("fn f(e: int) -> int {{\n  return match e {{\n{}    _ => 0,\n  }}\n}}\n\
                          fn main() {{\n  io.print(f(7))\n}}\n", arms);
    let arms: String = (0..n).map(|i| format!("    \"k{}\" => {},\n", i, i)).collect();
    let by_str = format!("fn f(e: str) -> int {{\n  return match e {{\n{}    _ => 0,\n  }}\n}}\n\
                          fn main() {{\n  io.print(f(\"k7\"))\n}}\n", arms);
    let chain = format!("fn main() {{\n  let x = 1\n  let s = x{}\n}}\n", ".a".repeat(8000));
    let started = Instant::now();
    // On the stack `kitec` gives the compiler: a chain this deep nests as
    // deep, and a test thread's stack is smaller.
    let codes = kite_driver::on_compiler_stack(move || {
        for (name, src) in [("match-enum", &by_enum), ("match-int", &by_int), ("match-str", &by_str)] {
            compiled(name, src, Emit::Check);
        }
        let c = compile("chain.kite", &chain, Emit::Check);
        let codes: Vec<String> = c.diags.iter().filter_map(|d| d.code.map(|x| x.0.to_string())).collect();
        (codes, c.render_diagnostics())
    });
    assert_eq!(codes.0, vec!["E0200"], "{}", codes.1);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(20), "checking took {:?}", took);
}

/// A write to a name that others hold part of and that holds part of others
/// (`m`, after a branch of `y = m` and a loop of `x = m`) once joined every
/// one of the first to every one of the second: five hundred of each made a
/// quarter of a million pairs, each read again for every name leaving at the
/// end, and the check took minutes. Now a second or two.
#[test]
fn a_write_to_a_name_many_copies_lead_into_and_out_of_is_checked_quickly() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let k = 500;
    let mut src = String::from(
        "fn f(i: int) -> error {\n  if i > 0 {\n    return errors.new(\"x\")\n  }\n  return nil\n}\n\n\
         fn run(n: int, b: bool) {\n  var m: error = nil\n",
    );
    for j in 0..k {
        src += &format!("  var y{}: error = f({})\n", j, j);
    }
    for i in 0..k {
        src += &format!("  var x{}: error = nil\n", i);
    }
    src += "  if b {\n    m = f(9)\n";
    for j in 0..k {
        src += &format!("    y{} = m\n", j);
    }
    src += "  }\n  for i in 0..n {\n";
    for i in 0..k {
        src += &format!("    x{} = m\n", i);
    }
    src += "  }\n  m = nil\n";
    for i in 0..k {
        src += &format!("  if x{} != nil {{\n    io.print(\"x\")\n  }}\n", i);
    }
    src += "}\n\nfn main() {\n  run(1, true)\n}\n";
    let started = Instant::now();
    let c = compile("copies.kite", &src, Emit::Check);
    let took = started.elapsed();
    // Each `y = m` writes over the failure `y` was given: that, and only
    // that, is reported.
    let codes: Vec<String> = c.diags.iter().filter_map(|d| d.code.map(|x| x.0.to_string())).collect();
    assert_eq!(codes, vec!["E0302".to_string(); k], "{}", c.render_diagnostics());
    assert!(took < Duration::from_secs(20), "checking took {:?}", took);
}

/// A key is hashed whole, so keys that differ only far inside still spread
/// over the buckets. The hash once stopped after 32 values, and 5,000 slice
/// keys that shared their first 40 elements all landed in one bucket: a scan
/// again, 18 s on the VM and natively alike. Wasm's hash reads every element
/// of a slice too (13 s there when its maps were scans).
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
    agrees_within("map-deep-keys", src, "199990000 20000\n", Duration::from_secs(20), true);
}

/// An enum key is hashed by its payload as well as its variant, on Wasm as
/// on the other two. Wasm's hash read the variant alone, so every `Id(n)`
/// shared one bucket and was found by comparing: 40,000 of them took ten
/// seconds under Node where the VM took a tenth of one, and these 100,000
/// would take about a minute.
#[test]
fn enum_keys_are_hashed_by_their_payload() {
    let src = r#"enum Key {
    Id(int)
    Pair(a: int, b: str)
    None
}

fn main() {
    var m: {Key: int} = {}
    for i in 0..100000 {
        m[Id(i)] = i
        if i % 4 == 0 {
            m[Pair(a: i, b: "p")] = 1
        }
    }
    m[None] = 2
    var s = 0
    for i in 0..100000 {
        s += or_else(m[Id(i)], 0)
    }
    for i in 0..50000 {
        m.remove(Id(i))
    }
    io.print("\(s) \(m.len()) \(or_else(m[Pair(a: 8, b: "p")], 0)) \(or_else(m[None], 0))")
}
"#;
    agrees_within("map-enum-keys", src, "4999950000 75001 1 2\n", Duration::from_secs(20), true);
}

/// A slice or a map grown across suspensions is written in place, as it is
/// outside `async`. The task's frame kept its own reference to every local it
/// had spilled, so each one came back from a suspension looking shared and
/// its next write copied it whole: a hundred thousand pushes, each after a
/// `yield`, took about twenty seconds on the VM and eight on Wasm, released,
/// and `out.push(await t)` — `task.all` — cost the same. A suspension moves
/// the locals into the frame and back out now, owned flags and all — a map's
/// too, on Wasm since its maps were given an index (15 s there before).
#[test]
fn a_slice_or_a_map_grown_across_suspensions_is_written_in_place() {
    let slices = "async fn one(i: int) -> int {\n  return i\n}\n\n\
                  pub async fn main() {\n  var out: [int] = []\n  for i in 0..100000 {\n\
                  \x20   out.push(i)\n    task.yield()\n  }\n  var got: [int] = []\n\
                  \x20 for i in 0..100000 {\n    got.push(await one(i))\n    got[i] = got[i] + 1\n  }\n\
                  \x20 io.print(\"\\(out.len()) \\(out[99999]) \\(got.len()) \\(got[99999])\")\n}\n";
    agrees_within("push-across-yield", slices, "100000 99999 100000 100000\n", Duration::from_secs(20), true);
    // A hundred thousand: at fifty thousand the old VM took about the bound
    // itself, which a faster machine would have passed.
    let maps = "pub async fn main() {\n  var m: {int: int} = {}\n  for i in 0..100000 {\n\
                \x20   m[i] = i\n    task.yield()\n  }\n  for i in 0..50000 {\n\
                \x20   m.remove(i)\n    task.yield()\n  }\n  io.print(\"\\(m.len()) \\(or_else(m[99999], -1))\")\n}\n";
    agrees_within("map-across-yield", maps, "50000 99999\n", Duration::from_secs(20), true);
}

/// Writing a slice through a struct field is linear on all three backends:
/// pushing onto a field, a nested field, `self`'s field in a `var self`
/// method, a `sync.Queue`, and a field of a struct held in a slice, and
/// storing into an element through a field. Each write took the field's
/// slice into a local that always looked shared, so every one of them copied
/// the whole slice: at three hundred thousand of each the native backend took
/// minutes and the VM hours. The field's slice is written in place now when
/// nothing else holds it, and reading its length or an element between the
/// writes, as the last loop's `if` does, does not count as holding it.
#[test]
fn a_slice_written_through_a_field_is_written_in_place() {
    let src = r#"use std/sync

struct Q {
    var items: [int]
    var total: int
}

struct Out {
    var b: Q
}

impl Q {
    fn add(var self, v: int) {
        self.items.push(v)
    }
}

fn main() {
    let n = 300000
    var q = Q{ items: [], total: 0 }
    var o = Out{ b: Q{ items: [], total: 0 } }
    var m = Q{ items: [], total: 0 }
    var s: sync.Queue<int> = sync.queue()
    var qs = [Q{ items: [0], total: 0 }]
    for i in 0..n {
        q.items.push(i)
        o.b.items.push(i)
        m.add(i)
        sync.put(s, i)
        if qs[0].items.len() <= n {
            qs[0].total += qs[0].items[i / 2]
            qs[0].items.push(i)
        }
    }
    for i in 0..n {
        q.items[i] = q.items[i] + 1
    }
    io.print("\(q.items[n - 1]) \(o.b.items.len()) \(m.items.len()) \(sync.size(s)) \(qs[0].items.len()) \(qs[0].total)")
}
"#;
    agrees_within(
        "push-through-a-field",
        src,
        "300000 300000 300000 300000 300001 22499550002\n",
        Duration::from_secs(20),
        true,
    );
}

/// Writing and removing map entries through a struct field is linear on all
/// three backends: a field, a nested field and `self`'s field in a `var self`
/// method, with lookups between the writes. Each write copied the whole map,
/// as a push copied a field's slice: at two hundred thousand the native
/// backend took a quarter of an hour, and Wasm did not finish in two minutes
/// even once its maps had an index, until the map's record kept the mark a
/// slice's header keeps.
#[test]
fn a_map_written_through_a_field_is_written_in_place() {
    let src = r#"struct S {
    var m: { int: int }
    var hits: int
}

struct Out {
    var b: S
}

impl S {
    fn put(var self, k: int, v: int) {
        self.m[k] = v
    }
}

fn main() {
    let n = 200000
    var s = S{ m: { }, hits: 0 }
    var o = Out{ b: S{ m: { }, hits: 0 } }
    var t = S{ m: { }, hits: 0 }
    for i in 0..n {
        s.m[i] = i
        o.b.m[i] = i
        t.put(i, i)
        if s.m.len() > 0 && s.m[i / 2] != nil {
            s.hits += 1
        }
    }
    for i in 0..n / 2 {
        s.m.remove(i)
        o.b.m.remove(i * 2)
    }
    io.print("\(s.m.len()) \(o.b.m.len()) \(t.m.len()) \(s.hits) \(or_else(s.m[n - 1], -1))")
}
"#;
    agrees_within(
        "map-through-a-field",
        src,
        "100000 100000 200000 200000 199999\n",
        Duration::from_secs(20),
        true,
    );
}

/// A slice or map handed to a function that only reads it, then written, is
/// written in place on all three backends: asked about through a generic
/// prelude function two calls down, read element by element, and taken
/// through a `var` parameter only read on the way. Passing a
/// local to any function cleared its owned flag natively and on Wasm, and
/// on the VM the call's staging register kept a second count, so each write
/// after a call copied the whole value: at twenty thousand the map loop
/// took 5.5 s natively and 10 s on the VM, and two hundred thousand would
/// take minutes on every backend. A callee proved not to keep its argument
/// leaves the caller the one holder now (`kite_mir::Kept`), and a call moves
/// its arguments out of the staging window.
#[test]
fn a_slice_or_map_passed_to_a_reader_is_written_in_place() {
    let src = r#"fn seen(m: { str: int }, w: str) -> bool {
    return has_key(m, w)
}

fn first(xs: [int]) -> int {
    if xs.len() == 0 {
        return 0
    }
    return xs[0] + xs[xs.len() - 1]
}

fn last(var xs: [int]) -> int {
    if xs.len() > 0 {
        return xs[xs.len() - 1]
    }
    return 0
}

fn main() {
    let n = 200000
    var m: { str: int } = { }
    var xs: [int] = []
    var ys: [int] = []
    var t = 0
    for i in 0..n {
        let w = "k\(i % (n / 2))"
        if !seen(m, w) {
            m[w] = i
        }
        t += first(xs)
        xs.push(i)
        t += last(ys) % 2
        ys.push(i)
        ys[i] = i * 2
    }
    io.print("\(m.len()) \(xs.len()) \(ys[n - 1]) \(t)")
}
"#;
    agrees_within(
        "passed-to-a-reader",
        src,
        "100000 200000 399998 19999700001\n",
        Duration::from_secs(20),
        true,
    );
}

/// A method that asks a helper about its own field's map or slice between
/// writes through the field is linear on all three backends. The read of
/// the field into the helper's argument counted as keeping it, so the next
/// write through the field copied the whole map: at forty thousand that
/// took 40 s on the VM. A call to a function that keeps nothing of the
/// argument, and can write in place nothing that was there before it ran,
/// only glances at the field now (`kite_mir::held_reads`), and so does a
/// read of it followed by rendering a number for the key, as `"j\(i)"` is.
#[test]
fn a_field_passed_to_a_reader_is_written_in_place() {
    let src = r#"fn seen(m: { str: int }, w: str) -> bool {
    return has_key(m, w)
}

fn count(xs: [int]) -> int {
    return xs.len()
}

struct Set {
    var m: { str: int }
    var order: [int]
}

impl Set {
    fn add(var self, w: str) {
        if !seen(self.m, w) {
            self.m[w] = count(self.order)
        }
        self.order.push(count(self.order))
    }

    fn add_at(var self, i: int) {
        if self.m["j\(i)"] == nil && !seen(self.m, "i\(i)") {
            self.m["j\(i)"] = i
        }
    }
}

fn main() {
    let n = 200000
    var s = Set{ m: { }, order: [] }
    for i in 0..n {
        s.add("k\(i % (n / 2))")
        s.add_at(i)
    }
    io.print("\(s.m.len()) \(s.order.len()) \(or_else(s.m["k7"], -1))")
}
"#;
    agrees_within(
        "field-passed-to-a-reader",
        src,
        "300000 200000 7\n",
        Duration::from_secs(20),
        true,
    );
}

/// A key or an index computed between a read of a field and the write after
/// it is linear on all three backends, however it is computed, as long as
/// nothing that computes it can write in place or run code it cannot see: a
/// string method (`trim`, `slice`, `len`, `index_of`, `code_at`), a builtin
/// that runs nothing (`text.from_code`), a rendered number, a function of
/// the program's, a prelude function and a method. A string method or a
/// builtin there counted as able to write in place, so the read of the
/// field counted as keeping it and the next write copied the whole map or
/// slice: at forty thousand, `self.counts[w.trim()] = …` took 10 s on the VM
/// and Wasm and 7 s natively, and this program at fifty thousand would take
/// minutes.
#[test]
fn a_key_or_index_computed_between_field_writes_is_linear() {
    let src = r#"struct Counter {
    var counts: { str: int }
    var xs: [int]
}

struct K {
    n: int
}

impl K {
    fn name(self) -> str {
        return "m\(self.n)"
    }
}

fn key(i: int) -> str {
    return "u\(i)"
}

fn idx(i: int, n: int) -> int {
    return i % n
}

impl Counter {
    fn add(var self, w: str) {
        self.counts[w.trim()] = or_else(self.counts[w.trim()], 0) + 1
    }

    fn lowered(var self, w: str) {
        self.counts[lower(w)] = or_else(self.counts[lower(w)], 0) + 1
    }
}

fn main() {
    let n = 50000
    var c = Counter{ counts: { }, xs: [0] }
    for i in 0..n {
        c.add(" w\(i % (n / 2)) ")
        c.lowered("L\(i)")
        if c.counts["s\(i)".slice(0, 9)] == nil {
            c.counts[text.from_code(65 + i % 26) + "\(i)"] = i
        }
        if c.counts[key(i)] == nil && c.counts[K{ n: i }.name()] == nil {
            c.counts[key(i)] = i
        }
        c.xs.push(c.xs["k\(i)".len() % c.xs.len()] + c.xs[idx(i, c.xs.len())])
        c.xs[("k\(i)".index_of("1") + 1) % c.xs.len()] += "k\(i)".code_at(0)
    }
    io.print("\(c.counts.len()) \(c.xs.len()) \(or_else(c.counts["w7"], -1))")
}
"#;
    agrees_within(
        "computed-key-through-a-field",
        src,
        "175000 50001 2\n",
        Duration::from_secs(20),
        true,
    );
}

/// `toml.parse` is linear in the shapes that were quadratic: forty thousand
/// sections, forty thousand dotted keys under one table, one inline table of
/// forty thousand keys, and — linear already, kept so — forty thousand plain
/// keys in one section. The document was nested `Toml` while it was read,
/// and a map is a value, so placing a key rebuilt every table on its path,
/// the root's included: this program took 51 s on Wasm and minutes natively.
/// It is read into flat tables written in place now, and nested once at the
/// end. Natively and on Wasm only: the debug VM's constant, a quarter of a
/// millisecond a section, leaves no bound that both a linear run meets and
/// the old one misses by a margin; its output is the differential suite's.
#[test]
fn toml_parse_is_linear_in_sections_dotted_keys_and_inline_tables() {
    let src = r#"use std/toml

fn count(text: str, path: str) -> int {
    let (doc, err) = toml.parse(text)
    if err != nil {
        io.print(err.message())
        return -1
    }
    return toml.keys_at(doc, path).len()
}

fn main() {
    let n = 40000
    var sections: [str] = []
    var dotted: [str] = ["[d]"]
    var inline: [str] = ["t = { "]
    var plain: [str] = ["[p]"]
    for i in 0..n {
        sections.push("[s\(i)]\na = \(i)")
        dotted.push("e.k\(i) = \(i)")
        inline.push("k\(i) = \(i), ")
        plain.push("k\(i) = \"v\(i)\"")
    }
    inline.push("last = 0 }")
    let a = count(join(sections, "\n"), "")
    let b = count(join(dotted, "\n"), "d.e")
    let c = count(join(inline, ""), "t")
    let d = count(join(plain, "\n"), "p")
    io.print("\(a) \(b) \(c) \(d)")
}
"#;
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let expected = "40000 40000 40001 40000\n";
    if let Some((out, took)) = on_native("toml-at-scale", src) {
        assert_eq!(out, expected, "the native backend");
        assert!(took < Duration::from_secs(30), "the native backend took {:?}", took);
    }
    if let Some((out, took)) = on_wasm("toml-at-scale", src) {
        assert_eq!(out, expected, "wasm");
        assert!(took < Duration::from_secs(10), "wasm took {:?}", took);
    }
}

/// A deep chain of awaits — a recursive `async fn`, the shape of a recursive
/// walk — is linear on all three backends. An `await` that had to wait parked
/// with no target, and every task finishing woke every parked task, so each
/// link of the chain finishing re-polled all the links still waiting above it:
/// twenty thousand deep took 84 s on the VM, 15 s natively and 7 s on Wasm,
/// released. An `await` parks on the task it awaits now, and a sweep walks
/// only the tasks that could run. Beside it, tasks each awaiting the one
/// spawned before them, which the scheduler finishes in one sweep.
#[test]
fn a_deep_chain_of_awaits_is_linear() {
    let down = "async fn down(n: int) -> int {\n  if n == 0 {\n    task.yield()\n    return 0\n  }\n\
                \x20 return 1 + await down(n - 1)\n}\n\n\
                async fn after(t: Task<int>) -> int {\n  return 1 + await t\n}\n\n\
                pub async fn main() {\n  io.print(await down(40000))\n\
                \x20 var t = down(0)\n  for i in 0..40000 {\n    t = after(t)\n  }\n\
                \x20 io.print(await t)\n}\n";
    agrees_within("await-chain", down, "40000\n40000\n", Duration::from_secs(20), true);
}
