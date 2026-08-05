//! A Kite program that listens on a port.
//!
//! The roadmap's Phase 11 said the server half was "the part that needs no
//! sockets" — the types, the router, and handlers that could be tested by
//! calling them. This is the other part: `std/http` now declares a listening
//! boundary, `kitec build --emit wasm` writes a Node adapter that answers it,
//! and a request made over a real socket reaches a handler and comes back.
//!
//! The requests are made from inside the runner rather than from Rust, because
//! the point is to prove one process both listens and answers — a test that
//! drove it from outside would prove the same thing about the adapter and less
//! about the program.

use kite_driver::{compile, Emit};
use std::process::Command;

mod common;
use common::Workspace;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compile a program that listens, and run it under Node with a client script
/// that talks to it.
fn serve_under_node(name: &str, src: &str, client: &str) -> String {
    // Removed when it drops, so a failing assertion below does not leave the
    // directory — nor the Node server the runner starts inside it.
    let work = Workspace::new(&format!("serve-{}", name));
    let dir = work.path();

    let c = compile(format!("{}.kite", name), src, Emit::Wasm);
    assert!(
        !c.failed(),
        "{} does not compile:\n{}",
        name,
        c.render_diagnostics()
    );
    let module = c.wasm.as_ref().expect("a module");
    assert!(
        kite_driver::listens(&module.hosts),
        "{} declares no listening boundary, so no adapter would be written",
        name
    );
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue_with_hosts(&module.strings, "app.wasm", &module.hosts),
    )
    .expect("write glue");
    std::fs::write(
        dir.join("serve.mjs"),
        kite_driver::generate_server("app.wasm"),
    )
    .expect("write adapter");
    std::fs::write(dir.join("client.mjs"), client).expect("write client");

    let output = Command::new("node")
        .arg(dir.join("client.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{} failed under node:\n{}\n{}",
        name,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

/// A router with a capture, a POST that reads a body, and a path nothing
/// matches — the three things a server has to get right before anything else.
const ROUTED: &str = "use std/http\n\
     fn hello(request: http.Request) -> http.Response {\n\
     \x20   let who = http.parameter(\"/hello/:name\", request.path, \"name\")\n\
     \x20   if who == nil {\n        return http.status(400, \"no name\")\n    }\n\
     \x20   return http.ok(\"hello \\(who)\")\n\
     }\n\
     fn echo(request: http.Request) -> http.Response {\n\
     \x20   return http.ok(\"\\(request.method)|\\(request.body)\")\n\
     }\n\
     async fn main() {\n\
     \x20   let (server, err) = await http.open(0)\n\
     \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
     \x20   io.print(\"listening \\(http.port_of(server))\")\n\
     \x20   let routes = [\n\
     \x20       http.route(\"GET\", \"/hello/:name\", hello),\n\
     \x20       http.route(\"POST\", \"/echo\", echo),\n\
     \x20   ]\n\
     \x20   let (answered, rerr) = await http.run(server, routes)\n\
     \x20   if rerr != nil {\n        io.print(rerr.message())\n        return\n    }\n\
     \x20   io.print(\"answered \\(answered)\")\n\
     }\n";

/// Start the program in a child process, wait for it to say which port it
/// bound, make three requests, and print what came back.
const CLIENT: &str = r#"import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

// `fileURLToPath`, not `.pathname`: a URL path is not a file path. On Windows
// `.pathname` gives `/C:/Users/...`, which `spawn` resolves against the current
// drive and cannot find — and it leaves percent-encoding in place, so a
// directory with a space or a `~` in its name fails the same way anywhere.
const server = spawn(process.execPath, [fileURLToPath(new URL("./serve.mjs", import.meta.url))], {
  stdio: ["ignore", "pipe", "inherit"],
});

// The child is killed however this ends. Without it a failure leaves a
// listening process behind and this script never exits, which turns a failing
// test into a hanging one.
const stop = () => { try { server.kill("SIGKILL"); } catch {} };
process.on("exit", stop);

try {
  let buffered = "";
  const port = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("the server never said it was listening:\n" + buffered)),
      15000,
    );
    server.stdout.on("data", (chunk) => {
      buffered += chunk;
      const match = buffered.match(/listening (\d+)/);
      if (match) {
        clearTimeout(timer);
        resolve(Number(match[1]));
      }
    });
    server.on("exit", (code) =>
      reject(new Error("the server exited with " + code + ":\n" + buffered)));
  });

  const say = async (label, promise) => {
    const response = await promise;
    console.log(label + " " + response.status + " " + (await response.text()));
  };

  await say("hello", fetch(`http://127.0.0.1:${port}/hello/kite`));
  await say("echo", fetch(`http://127.0.0.1:${port}/echo`, { method: "POST", body: "a body" }));
  await say("missing", fetch(`http://127.0.0.1:${port}/nowhere`));
} finally {
  stop();
}
"#;

#[test]
fn a_kite_program_answers_over_a_real_socket() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = serve_under_node("routed", ROUTED, CLIENT);
    assert_eq!(
        out,
        "hello 200 hello kite\n\
         echo 200 POST|a body\n\
         missing 404 not found\n"
    );
}

/// A 404 is a `Response`, not an error — the request succeeded and the answer
/// was "no". That rule was already true of the client half; this is it holding
/// on the other side of the same socket.
#[test]
fn a_route_that_matches_nothing_is_an_answer_and_not_a_failure() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = serve_under_node("not-found", ROUTED, CLIENT);
    assert!(out.contains("missing 404 not found"), "{}", out);
    assert!(!out.contains("error"), "{}", out);
}

/// A program that only fetches must not be handed an adapter: it would import
/// `node:http` in order never to use it.
#[test]
fn a_program_that_only_fetches_gets_no_adapter() {
    let c = compile(
        "client.kite",
        "use std/http\n\
         async fn main() {\n\
         \x20   let (r, err) = await http.get(\"data:,hi\")\n\
         \x20   if err != nil {\n        return\n    }\n\
         \x20   io.print(r.status)\n\
         }\n",
        Emit::Wasm,
    );
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let module = c.wasm.as_ref().expect("a module");
    assert!(
        !kite_driver::listens(&module.hosts),
        "a fetching program should not be given a server adapter"
    );
}

/// A newline in a header value does not become a second header.
///
/// This is the one place the boundary's `name: value` encoding is dangerous:
/// splitting on newlines turns what Node itself refuses (`ERR_INVALID_CHAR`)
/// into two well-formed headers, so a handler that interpolates anything a
/// request said into a header string would be one newline away from setting a
/// cookie somebody else chose. The adapter drops the malformed line instead.
#[test]
fn a_newline_in_a_header_does_not_smuggle_a_second_one() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn nowhere(request: http.Request) -> http.Response {\n\
         \x20   return http.not_found()\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let (incoming, aerr) = await http.accept(server)\n\
         \x20   if aerr != nil {\n        return\n    }\n\
         \x20   // Exactly the mistake an application makes: request text, straight\n\
         \x20   // into a header.\n\
         \x20   let smuggled = http.respond(incoming, http.ok(\"body\"),\n\
         \x20       [http.Header{ name: \"location\", value: \"/view/\\(incoming.request.body)\" }])\n\
         \x20   if smuggled != nil {\n\
         \x20       io.print(smuggled.message())\n\
         \x20       // Refused, so nothing was sent — answer without it, or the\n\
         \x20       // socket simply hangs.\n\
         \x20       let none: [http.Header] = []\n\
         \x20       let plain = http.respond(incoming, http.ok(\"refused\"), none)\n\
         \x20       if plain != nil {\n            io.print(plain.message())\n        }\n\
         \x20   }\n\
         }\n";
    let client = r#"import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const server = spawn(process.execPath, [fileURLToPath(new URL("./serve.mjs", import.meta.url))], {
  stdio: ["ignore", "pipe", "inherit"],
});
const stop = () => { try { server.kill("SIGKILL"); } catch {} };
process.on("exit", stop);

try {
  let buffered = "";
  const port = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("never listened:\n" + buffered)), 15000);
    server.stdout.on("data", (chunk) => {
      buffered += chunk;
      const match = buffered.match(/listening (\d+)/);
      if (match) { clearTimeout(timer); resolve(Number(match[1])); }
    });
    server.on("exit", (code) => reject(new Error("exited with " + code + ":\n" + buffered)));
  });

  const response = await fetch(`http://127.0.0.1:${port}/x`, {
    method: "POST",
    body: "x\nSet-Cookie: session=attacker; Path=/",
  });
  console.log("status " + response.status);
  console.log("body " + (await response.text()));
  console.log("set-cookie " + JSON.stringify(response.headers.get("set-cookie")));
  console.log("location " + JSON.stringify(response.headers.get("location")));
} finally {
  stop();
}
"#;
    let out = serve_under_node("header-injection", src, client);
    // The whole malformed line is dropped: no smuggled cookie, and no
    // half-written `location` either.
    assert!(
        out.contains("set-cookie null"),
        "a newline in a header value smuggled a second header:\n{}",
        out
    );
    // The whole response is refused rather than half-sent: `respond` answers
    // with an error, so no `location` goes out either.
    assert!(out.contains("location null"), "{}", out);
    // And the handler was told why, rather than left to wonder: it took the
    // error path and answered with the fallback body. (The message itself goes
    // to the server child's own output, which this client never reads.)
    assert!(
        out.contains("body refused"),
        "the handler should have been told the header was refused:\n{}",
        out
    );
}
