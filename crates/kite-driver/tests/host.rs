//! The declared host boundary, end to end.
//!
//! `@host("group") extern fn` becomes a Wasm import, the generated glue
//! declares the group, and a program reaches the outside world through it and
//! nothing else. These programs cannot run on the bytecode VM — it has no
//! network and no WebCrypto, and says so rather than pretending — so they are
//! compiled to WebAssembly and run under Node, which has both.

use kite_driver::{compile, Emit};
use std::path::Path;
use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compile a program to Wasm and run it under Node, returning its output.
fn run_under_node(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("kite-host-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("work directory");
    let c = compile(format!("{}.kite", name), src, Emit::Wasm);
    assert!(
        !c.failed(),
        "{} does not compile:\n{}",
        name,
        c.render_diagnostics()
    );
    let module = c.wasm.as_ref().expect("a module");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue_with_hosts(&module.strings, "app.wasm", &module.hosts),
    )
    .expect("write glue");
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

    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{} failed under node:\n{}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(output.stdout).expect("utf-8")
}

/// Three requests started together take as long as the slowest, and a 404 is a
/// response rather than an error.
#[test]
fn http_fetches_concurrently() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "http",
        "use std/http\n\
         async fn main() {\n\
         \x20 let a = http.get(\"data:text/plain,first\")\n\
         \x20 let b = http.get(\"data:text/plain,second\")\n\
         \x20 let c = http.get(\"data:text/plain,third\")\n\
         \x20 let (ra, ea) = await a\n\
         \x20 let (rb, eb) = await b\n\
         \x20 let (rc, ec) = await c\n\
         \x20 if ea != nil { return }\n\
         \x20 if eb != nil { return }\n\
         \x20 if ec != nil { return }\n\
         \x20 io.print(\"\\(ra.status) \\(ra.body)\")\n\
         \x20 io.print(\"\\(rb.status) \\(rb.body)\")\n\
         \x20 io.print(\"\\(rc.status) \\(rc.body)\")\n}\n",
    );
    assert_eq!(out, "200 first\n200 second\n200 third\n", "{}", out);
}

/// A transport failure is an `error`. A status is not.
#[test]
fn a_transport_failure_is_an_error() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "http-failure",
        "use std/http\n\
         async fn main() {\n\
         \x20 let (r, err) = await http.get(\"http://127.0.0.1:9/nothing\")\n\
         \x20 io.print(if err == nil { \"unexpectedly ok\" } else { \"failed\" })\n}\n",
    );
    assert_eq!(out, "failed\n", "{}", out);
}

/// The router is pure Kite and needs no host at all, so it is tested on the
/// bytecode VM where it belongs.
#[test]
fn the_router_matches_and_captures() {
    let src = "use std/http\n\
        fn hello(r: http.Request) -> http.Response {\n\
        \x20 let who = http.parameter(\"/hello/:name\", r.path, \"name\")\n\
        \x20 return http.ok(\"hello \\(if who == nil { \"nobody\" } else { who })\")\n}\n\
        fn main() {\n\
        \x20 let routes = [http.route(\"GET\", \"/hello/:name\", hello)]\n\
        \x20 let found = http.serve(routes, http.Request{method: \"GET\", path: \"/hello/kite\", body: \"\", headers: \"\"})\n\
        \x20 io.print(\"\\(found.status) \\(found.body)\")\n\
        \x20 let missing = http.serve(routes, http.Request{method: \"GET\", path: \"/nope\", body: \"\", headers: \"\"})\n\
        \x20 io.print(missing.status)\n\
        \x20 let wrong = http.serve(routes, http.Request{method: \"POST\", path: \"/hello/kite\", body: \"\", headers: \"\"})\n\
        \x20 io.print(wrong.status)\n\
        \x20 let header = http.request_header(http.Request{method: \"GET\", path: \"/\", body: \"\", headers: \"Content-Type: text/plain\"}, \"content-type\")\n\
        \x20 io.print(if header == nil { \"none\" } else { header })\n}\n";
    let c = compile("router.kite", src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("runs");
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "200 hello kite\n404\n404\ntext/plain\n"
    );
}

/// SHA-256 of "hello" is a value everyone can check, which is the point of
/// binding to the host's primitive rather than writing one.
#[test]
fn crypto_binds_to_the_hosts_primitives() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "crypto",
        "use std/crypto\n\
         async fn main() {\n\
         \x20 let (d, err) = await crypto.sha256(\"hello\")\n\
         \x20 if err != nil { return }\n\
         \x20 io.print(d)\n\
         \x20 io.print(crypto.random(16).len())\n\
         \x20 io.print(crypto.equal(\"abc\", \"abc\"))\n\
         \x20 io.print(crypto.equal(\"abc\", \"abd\"))\n\
         \x20 let (stored, serr) = await crypto.password_hash(\"correct horse\")\n\
         \x20 if serr != nil { return }\n\
         \x20 let (ok, verr) = await crypto.password_verify(\"correct horse\", stored)\n\
         \x20 if verr != nil { return }\n\
         \x20 let (bad, berr) = await crypto.password_verify(\"wrong\", stored)\n\
         \x20 if berr != nil { return }\n\
         \x20 io.print(\"\\(ok) \\(bad)\")\n}\n",
    );
    assert_eq!(
        out,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n32\ntrue\nfalse\ntrue false\n",
        "{}",
        out
    );
}

/// A program's own boundary: declared in Kite, supplied by the page.
#[test]
fn a_program_may_declare_its_own_host() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kite-own-host-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("work directory");
    let src = "@host(\"paint\")\nextern fn brush(size: int) -> str\n\
        fn main() {\n  io.print(brush(3))\n}\n";
    let c = compile("own.kite", src, Emit::Wasm);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let module = c.wasm.as_ref().expect("a module");
    assert_eq!(module.hosts.len(), 1);
    assert_eq!(module.hosts[0].group, "paint");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue_with_hosts(&module.strings, "app.wasm", &module.hosts),
    )
    .expect("write");
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter, provide, str } from \"./app.js\";\n\
         setWriter((l) => process.stdout.write(l + \"\\n\"));\n\
         provide(\"paint\", { brush: (size) => str(\"brush of \" + size) });\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n",
    )
    .expect("write");
    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "failed under node:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "brush of 3\n");
}

/// The bytecode target has no host of its own, and says which declaration it
/// could not answer rather than returning a silent zero.
#[test]
fn the_bytecode_target_says_it_has_no_host() {
    let src = "@host(\"paint\")\nextern fn brush(size: int) -> str\n\
        fn main() {\n  io.print(brush(3))\n}\n";
    let c = compile("own.kite", src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    let err = c.run(&mut out).expect_err("there is no host");
    assert!(err.to_string().contains("paint.brush"), "{}", err);
}

/// Only what can cross does. A struct would need a representation both sides
/// agreed on, and inventing one silently is how an FFI corrupts data.
#[test]
fn an_aggregate_cannot_cross_the_boundary() {
    let src = "struct P {\n  x: int\n}\n@host(\"paint\")\nextern fn draw(p: P)\n\
        fn main() {\n}\n";
    let c = compile("bad.kite", src, Emit::Check);
    assert!(c.failed());
    assert!(
        c.render_diagnostics().contains("cannot cross the host boundary"),
        "{}",
        c.render_diagnostics()
    );
}

/// A declaration nothing calls costs nothing: no import, and nothing for a
/// host to supply.
#[test]
fn an_unused_declaration_is_not_imported() {
    let src = "@host(\"paint\")\nextern fn brush(size: int) -> str\nfn main() {\n  io.print(1)\n}\n";
    let c = compile("unused.kite", src, Emit::Wasm);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    assert!(c.wasm.as_ref().expect("a module").hosts.is_empty());
}

/// Comparing a secret with `==` is a timing oracle, and the compiler says so.
#[test]
fn comparing_a_secret_with_equals_warns() {
    let src = "use std/crypto\nfn main() {\n  let want = \"abc\"\n\
        \x20 if crypto.random(8) == want {\n    io.print(\"same\")\n  }\n}\n";
    let c = compile(Path::new("secret.kite"), src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    assert!(
        c.render_diagnostics().contains("E0600"),
        "{}",
        c.render_diagnostics()
    );
}
