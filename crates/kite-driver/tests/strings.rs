//! The canonical WebAssembly `str` representation.
//!
//! Kite owns strings as WasmGC arrays of Unicode scalar values. These tests
//! compare the implementation with the bytecode VM, exercise JavaScript
//! boundary conversion, and ensure the removed host representations cannot
//! creep back into the module or glue.

use kite_driver::{compile, Emit};
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
     setWriter((line) => out.push(line));\n\
     await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     process.stdout.write(out.map((line) => line + \"\\n\").join(\"\"));\n";

fn build_in(name: &str, src: &str, runner: &str) -> Option<String> {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("kite-str-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");

    let compiled = compile(format!("{}.kite", name), src, Emit::Wasm);
    assert!(
        !compiled.failed(),
        "{} does not compile:\n{}",
        name,
        compiled.render_diagnostics()
    );
    let module = compiled.wasm.as_ref().expect("a module");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue_with_hosts("app.wasm", &module.hosts),
    )
    .expect("write glue");
    std::fs::write(dir.join("run.mjs"), runner).expect("write runner");

    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "{} failed under node:\n{}",
        name,
        stderr
    );
    Some(String::from_utf8(output.stdout).expect("utf-8"))
}

fn run_under_node(name: &str, src: &str) -> Option<String> {
    build_in(name, src, RUNNER)
}

fn run_on_vm(name: &str, src: &str) -> String {
    let compiled = compile(format!("{}.kite", name), src, Emit::Check);
    assert!(
        !compiled.failed(),
        "{}:\n{}",
        name,
        compiled.render_diagnostics()
    );
    let mut out = Vec::new();
    compiled.run(&mut out).expect("runs on the VM");
    String::from_utf8(out).expect("utf-8")
}

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
    (
        "dynamic-operations",
        "fn main() {\n\
         \x20 var out = \"\"\n\
         \x20 for i in 0..4 {\n    out = out + \"\\(i),\"\n  }\n\
         \x20 io.print(out)\n\
         \x20 io.print(out.len())\n\
         \x20 io.print(out.slice(2, 6))\n\
         \x20 io.print(out.index_of(\"2\"))\n\
         \x20 io.print(\"  \\u{2003}trim me\\u{3000} \".trim())\n}\n",
    ),
    (
        "unicode-scalars",
        "fn main() {\n\
         \x20 let s = \"h\\u{e9}llo\\u{1F600}\\u{65E5}\"\n\
         \x20 io.print(s.len())\n\
         \x20 io.print(s.code_at(5))\n\
         \x20 io.print(s.slice(5, 6))\n\
         \x20 io.print(s.slice(6, 7))\n\
         \x20 io.print(s.index_of(\"\\u{65E5}\"))\n\
         \x20 io.print(text.from_code(0x1F680))\n\
         \x20 io.print(text.from_code(0xD800))\n}\n",
    ),
    (
        "strings-inside-values",
        "struct Person {\n  name: str\n  tags: [str]\n}\n\
         fn main() {\n\
         \x20 let a = Person{ name: \"ada\", tags: [\"maths\", \"engines\"] }\n\
         \x20 let b = Person{ name: \"ada\", tags: [\"maths\", \"engines\"] }\n\
         \x20 io.print(a == b)\n\
         \x20 var m: { str: int } = {}\n\
         \x20 m[\"one\"] = 1\n\
         \x20 m[\"two\"] = 2\n\
         \x20 m[\"one\"] = 3\n\
         \x20 io.print(m.len())\n\
         \x20 io.print(join(m.keys(), \"|\"))\n}\n",
    ),
    (
        "optional-strings",
        "struct User {\n  id: int\n  nickname: Option<str>\n}\n\
         fn main() {\n\
         \x20 let a = User{ id: 1, nickname: \"ada\" }\n\
         \x20 let b = User{ id: 1, nickname: \"ada\" }\n\
         \x20 let c = User{ id: 1, nickname: nil }\n\
         \x20 io.print(a == b)\n\
         \x20 io.print(a == c)\n\
         \x20 let n: Option<str> = \"ada\"\n\
         \x20 let o: Option<str> = nil\n\
         \x20 io.print(n == o)\n\
         \x20 io.print(n == n)\n}\n",
    ),
    (
        "prelude-text",
        "fn main() {\n\
         \x20 io.print(join(split(\"a,b,c\", \",\"), \"-\"))\n\
         \x20 io.print(replace(\"banana\", \"na\", \"NA\"))\n\
         \x20 io.print(starts_with(\"kite\", \"ki\"))\n\
         \x20 io.print(upper(\"kite\"))\n\
         \x20 io.print(debug_str(\"a\\\"b\"))\n}\n",
    ),
];

#[test]
fn wasm_strings_match_the_vm() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let mut mismatches = Vec::new();
    for (name, src) in PROGRAMS {
        let vm = run_on_vm(name, src);
        let wasm = run_under_node(name, src).expect("node was checked");
        if vm != wasm {
            mismatches.push(format!("{}:\n  vm:   {:?}\n  wasm: {:?}", name, vm, wasm));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn exported_strings_round_trip_through_the_boundary() {
    let src = "pub fn echo(body: str) -> str {\n  return \"[\" + body + \"]\"\n}\n";
    let runner = "import { readFile } from \"node:fs/promises\";\n\
         import { instantiate, str, text } from \"./app.js\";\n\
         const bytes = new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url)));\n\
         const kite = await instantiate(bytes);\n\
         process.stdout.write(text(kite.echo(str(\"hé😀日\"))));\n";
    let Some(output) = build_in("export-round-trip", src, runner) else {
        return;
    };
    assert_eq!(output, "[hé😀日]");
}

#[test]
fn removed_host_representations_leave_no_abi_remnants() {
    let src = "fn main() {\n  io.print(\"one\" + \"two\")\n}\n";
    let compiled = compile("strings.kite", src, Emit::Wasm);
    assert!(!compiled.failed(), "{}", compiled.render_diagnostics());
    let module = compiled.wasm.as_ref().expect("a module");
    let has = |needle: &str| {
        module
            .bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    };
    for removed in [
        "wasm:js-string",
        "kite:strings",
        "str_concat",
        "str_eq",
        "str_compare",
        "str_slice",
    ] {
        assert!(!has(removed), "module still contains `{}`", removed);
    }
    assert!(has("scratch"));
    assert!(has("$kite.str.from_host"));

    let glue = kite_driver::generate_glue_with_hosts("app.wasm", &module.hosts);
    for removed in [
        "const STRINGS",
        "const INDEX",
        "function intern",
        "wasm:js-string",
    ] {
        assert!(!glue.contains(removed), "glue still contains `{}`", removed);
    }
    assert!(glue.contains("initial: 1, maximum: 1"));
}
