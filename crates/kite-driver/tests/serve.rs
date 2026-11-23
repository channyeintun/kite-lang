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
        kite_driver::generate_glue_with_hosts("app.wasm", &module.hosts),
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

/// A body arriving in pieces is one body, and a chunk boundary is not a
/// character boundary.
///
/// The adapter used to decode each `data` event on its own and concatenate the
/// results, so a character whose UTF-8 bytes straddled two events became two
/// replacement characters. Nothing about that needed an attacker: any
/// non-ASCII body big enough to arrive in two pieces was quietly corrupted
/// before the handler saw it, and the handler had no way to tell.
///
/// The split here is written by hand — one byte of a two-byte `é` in each
/// write, with a pause between them so they cannot share a packet — because
/// the accidental version of this depends on how the kernel happens to
/// fragment a large body.
const SPLIT_BODY_CLIENT: &str = r#"import { spawn } from "node:child_process";
import { request } from "node:http";
import { fileURLToPath } from "node:url";

const server = spawn(process.execPath, [fileURLToPath(new URL("./serve.mjs", import.meta.url))], {
  stdio: ["ignore", "pipe", "inherit"],
});
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

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const answer = await new Promise((resolve, reject) => {
    const req = request(
      { host: "127.0.0.1", port, path: "/echo", method: "POST", headers: { "transfer-encoding": "chunked" } },
      (res) => {
        let text = "";
        res.setEncoding("utf8");
        res.on("data", (c) => { text += c; });
        res.on("end", () => resolve(res.statusCode + " " + text));
      },
    );
    req.on("error", reject);
    (async () => {
      // `é` is 0xC3 0xA9. One byte now, one byte after a pause.
      req.write(Buffer.from([0x63, 0x61, 0x66, 0xc3]));
      await sleep(50);
      req.write(Buffer.from([0xa9]));
      req.end();
    })();
  });
  console.log("split " + answer);
} finally {
  stop();
}
"#;

#[test]
fn a_character_split_across_two_chunks_survives() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = serve_under_node("split-body", ROUTED, SPLIT_BODY_CLIENT);
    assert_eq!(out, "split 200 POST|café\n", "{}", out);
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

/// Start the adapter in a child, wait for "listening <port>", run `body` (a
/// string of JavaScript with `port`, `server` and `buffered` in scope), and
/// stop the child however that ends.
fn client_with(body: &str) -> String {
    format!(
        r#"import {{ spawn }} from "node:child_process";
import {{ fileURLToPath }} from "node:url";

const server = spawn(process.execPath, [fileURLToPath(new URL("./serve.mjs", import.meta.url))], {{
  stdio: ["ignore", "pipe", "inherit"],
}});
const stop = () => {{ try {{ server.kill("SIGKILL"); }} catch {{}} }};
process.on("exit", stop);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try {{
  let buffered = "";
  server.stdout.on("data", (chunk) => {{ buffered += chunk; }});
  const port = await new Promise((resolve, reject) => {{
    const timer = setTimeout(() => reject(new Error("never listened:\n" + buffered)), 15000);
    const look = () => {{
      const match = buffered.match(/listening (\d+)/);
      if (match) {{
        clearTimeout(timer);
        resolve(Number(match[1]));
      }} else {{
        setTimeout(look, 10);
      }}
    }};
    look();
  }});
{body}
}} finally {{
  stop();
}}
"#,
        body = body
    )
}

/// Two headers of one name both reach the wire, and a program's own content
/// type is the only one sent, however it capitalised the name.
///
/// The adapter gathered headers into an object, one slot per name: the first
/// of two `Set-Cookie`s was dropped, and `Content-type` beside the default
/// `content-type` sent the response out with two types.
#[test]
fn repeated_and_differently_cased_headers_survive() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let (incoming, aerr) = await http.accept(server)\n\
         \x20   if aerr != nil {\n        return\n    }\n\
         \x20   let headers = [\n\
         \x20       http.Header{ name: \"Set-Cookie\", value: \"a=1\" },\n\
         \x20       http.Header{ name: \"Set-Cookie\", value: \"b=2\" },\n\
         \x20       http.Header{ name: \"Content-type\", value: \"application/json\" },\n\
         \x20   ]\n\
         \x20   let rerr = http.respond(incoming, http.ok(\"{}\"), headers)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const r = await fetch(`http://127.0.0.1:${port}/`);
  console.log("cookies " + r.headers.getSetCookie().join(" "));
  console.log("type " + r.headers.get("content-type"));"#,
    );
    let out = serve_under_node("headers", src, &client);
    assert_eq!(out, "cookies a=1 b=2\ntype application/json\n", "{}", out);
}

/// A response header's name is held to what a request's is: a colon in a name
/// is not a header called something else, and a NUL in a value is not sent.
///
/// `respond` refused only a line break in a name, so `Set-Cookie: a=b; x` as a
/// name went out as a `Set-Cookie` header the handler never meant to write.
#[test]
fn a_response_header_name_must_be_a_token_and_a_value_has_no_nul() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let (incoming, aerr) = await http.accept(server)\n\
         \x20   if aerr != nil {\n        return\n    }\n\
         \x20   let colon = http.respond(incoming, http.ok(\"x\"),\n\
         \x20       [http.Header{ name: \"Set-Cookie: session=evil; x\", value: \"1\" }])\n\
         \x20   let nul = http.respond(incoming, http.ok(\"x\"),\n\
         \x20       [http.Header{ name: \"x-a\", value: \"a\\0b\" }])\n\
         \x20   let none: [http.Header] = []\n\
         \x20   let big = http.respond(incoming, http.status(1000, \"x\"), none)\n\
         \x20   var why = \"\"\n\
         \x20   if big != nil {\n        why = big.message()\n    }\n\
         \x20   let last = http.respond(incoming, http.ok(\"colon=\\(colon != nil) nul=\\(nul != nil) \\(why)\"), none)\n\
         \x20   if last != nil {\n        io.print(last.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const r = await fetch(`http://127.0.0.1:${port}/`);
  console.log("cookie " + JSON.stringify(r.headers.get("set-cookie")));
  console.log("body " + (await r.text()));"#,
    );
    let out = serve_under_node("header-names", src, &client);
    assert_eq!(
        out,
        "cookie null\nbody colon=true nul=true http.respond: `1000` is not a status HTTP sends (200 to 599)\n",
        "{}",
        out
    );
}

/// `run` sends the headers a handler put on its response, and answers 500 to a
/// handler whose header could not be sent without ending the server.
///
/// `run` passed no headers at all, so a JSON API served through it was
/// `text/plain` and nothing could redirect or set a cookie.
#[test]
fn run_sends_the_headers_a_handler_added() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn api(request: http.Request) -> http.Response {\n\
         \x20   let r = http.with_header(http.ok(\"{}\"), \"Content-Type\", \"application/json\")\n\
         \x20   let r2 = http.with_header(r, \"Set-Cookie\", \"a=1\")\n\
         \x20   return http.with_header(r2, \"Set-Cookie\", \"b=2\")\n\
         }\n\
         fn bad(request: http.Request) -> http.Response {\n\
         \x20   return http.with_header(http.ok(\"x\"), \"Set-Cookie: evil; x\", \"1\")\n\
         }\n\
         fn plain(request: http.Request) -> http.Response {\n\
         \x20   return http.ok(\"plain\")\n\
         }\n\
         fn data(request: http.Request) -> http.Response {\n\
         \x20   return http.json(201, \"{}\")\n\
         }\n\
         fn go(request: http.Request) -> http.Response {\n\
         \x20   return http.redirect(\"/done\")\n\
         }\n\
         fn zero(request: http.Request) -> http.Response {\n\
         \x20   return http.status(0, \"zero\")\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let routes = [\n\
         \x20       http.route(\"GET\", \"/api\", api),\n\
         \x20       http.route(\"GET\", \"/zero\", zero),\n\
         \x20       http.route(\"GET\", \"/data\", data),\n\
         \x20       http.route(\"GET\", \"/go\", go),\n\
         \x20       http.route(\"GET\", \"/bad\", bad),\n\
         \x20       http.route(\"GET\", \"/plain\", plain),\n\
         \x20   ]\n\
         \x20   let (n, rerr) = await http.run(server, routes)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const a = await fetch(`http://127.0.0.1:${port}/api`);
  console.log("api " + a.status + " " + a.headers.get("content-type") + " " + a.headers.getSetCookie().join(" "));
  const b = await fetch(`http://127.0.0.1:${port}/bad`);
  console.log("bad " + b.status + " " + JSON.stringify(b.headers.get("set-cookie")));
  const d = await fetch(`http://127.0.0.1:${port}/data`);
  console.log("data " + d.status + " " + d.headers.get("content-type"));
  const g = await fetch(`http://127.0.0.1:${port}/go`, { redirect: "manual" });
  console.log("go " + g.status + " " + g.headers.get("location"));
  const z = await fetch(`http://127.0.0.1:${port}/zero`);
  console.log("zero " + z.status);
  const p = await fetch(`http://127.0.0.1:${port}/plain`);
  console.log("plain " + p.status + " " + p.headers.get("content-type") + " " + (await p.text()));"#,
    );
    let out = serve_under_node("run-headers", src, &client);
    assert_eq!(
        out,
        "api 200 application/json a=1 b=2\nbad 500 null\ndata 201 application/json; charset=utf-8\ngo 303 /done\nzero 500\nplain 200 text/plain; charset=utf-8 plain\n",
        "{}",
        out
    );
}

/// A header value the host cannot put on the wire — a character past U+00FF,
/// or a control character other than a tab — is a 500 for that one caller,
/// and the server goes on answering.
///
/// Node refuses such a value in `writeHead`; the adapter answered 500 and told
/// the program "already answered", and `run` ended the server on it, so one
/// handler's `€` in a header stopped every route.
#[test]
fn a_header_value_the_host_cannot_carry_does_not_end_the_server() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn euro(request: http.Request) -> http.Response {\n\
         \x20   return http.with_header(http.ok(\"x\"), \"x-price\", \"5\u{20AC}\")\n\
         }\n\
         fn bell(request: http.Request) -> http.Response {\n\
         \x20   return http.redirect(\"/a\u{7}b\")\n\
         }\n\
         fn latin(request: http.Request) -> http.Response {\n\
         \x20   return http.with_header(http.ok(\"latin\"), \"x-name\", \"caf\u{E9}\tok\")\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let routes = [\n\
         \x20       http.route(\"GET\", \"/euro\", euro),\n\
         \x20       http.route(\"GET\", \"/bell\", bell),\n\
         \x20       http.route(\"GET\", \"/latin\", latin),\n\
         \x20   ]\n\
         \x20   let (n, rerr) = await http.run(server, routes)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const e = await fetch(`http://127.0.0.1:${port}/euro`);
  console.log("euro " + e.status + " " + JSON.stringify(e.headers.get("x-price")));
  const b = await fetch(`http://127.0.0.1:${port}/bell`, { redirect: "manual" });
  console.log("bell " + b.status);
  const l = await fetch(`http://127.0.0.1:${port}/latin`);
  console.log("latin " + l.status + " " + (await l.text()));"#,
    );
    let out = serve_under_node("uncarriable", src, &client);
    assert_eq!(out, "euro 500 null\nbell 500\nlatin 200 latin\n", "{}", out);
}

/// While a server waits for requests it costs nothing, and a task's sleep
/// still comes due.
///
/// The adapter ran the program to completion with the batch driver, which
/// polled the accept loop some nine hundred times a second while nothing
/// arrived, and whose clock did not move while a task waited on the host — a
/// task that slept before closing the server never woke. The count is taken
/// by wrapping the module's `$kite.poll` before the adapter loads it.
#[test]
fn an_idle_server_costs_nothing_and_its_timers_still_run() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         use std/task\n\
         fn hello(request: http.Request) -> http.Response {\n\
         \x20   return http.ok(\"hello\")\n\
         }\n\
         async fn later() -> int {\n\
         \x20   await task.sleep(300)\n\
         \x20   io.print(\"slept\")\n\
         \x20   return 0\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let t = later()\n\
         \x20   let routes = [http.route(\"GET\", \"/hello\", hello)]\n\
         \x20   let (answered, rerr) = await http.run(server, routes)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n        return\n    }\n\
         \x20   io.print(\"answered \\(answered)\")\n\
         }\n";
    // The adapter is imported in this process, after `$kite.poll` has been
    // wrapped, so the polls can be counted.
    let client = r#"import { fileURLToPath } from "node:url";
const original = WebAssembly.instantiate;
let polls = 0;
WebAssembly.instantiate = async (...args) => {
  const result = await original(...args);
  if (!result.instance || !result.instance.exports["$kite.poll"]) return result;
  const exports = { ...result.instance.exports };
  const poll = result.instance.exports["$kite.poll"];
  exports["$kite.poll"] = (task) => { polls += 1; return poll(task); };
  return { instance: { exports }, module: result.module };
};
const lines = [];
const write = process.stdout.write.bind(process.stdout);
process.stdout.write = (chunk) => { lines.push(String(chunk)); return true; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
await import(new URL("./serve.mjs", import.meta.url));
// The sleeper wakes while the accept loop waits on the host; after that the
// program has nothing to do until a request comes.
const started = Date.now();
while (!lines.join("").includes("slept") && Date.now() - started < 5000) await sleep(10);
const slept = lines.join("").includes("slept");
await sleep(50);
const before = polls;
await sleep(500);
const idle = polls - before;
const port = Number(lines.join("").match(/listening (\d+)/)[1]);
const r = await fetch(`http://127.0.0.1:${port}/hello`);
const text = await r.text();
write("slept while waiting: " + slept + "\n");
write("idle polls: " + idle + "\n");
write(r.status + " " + text + "\n");
process.exit(0);
"#;
    let out = serve_under_node("idle", src, client);
    assert_eq!(
        out,
        "slept while waiting: true\nidle polls: 0\n200 hello\n",
        "{}",
        out
    );
}

/// A shut server stops: `run` returns what it answered, and `accept` says the
/// server is shut rather than waiting for a request that cannot come.
///
/// Both used to wait for good. `accept` looped on "nothing pending" and never
/// asked whether anything ever could be, and `run` only noticed the shut
/// after answering a request — so a server shut while idle, which is the
/// ordinary way to stop one, never let `run` return.
///
/// The program is its own client, as in the test above, and also checks the
/// two routing cases a query string used to break and a header block that is
/// not `name: value` lines being refused rather than silently dropped.
#[test]
fn a_shut_server_stops_accepting_and_run_returns() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = r##"use std/http

fn index(r: http.Request) -> http.Response {
    return http.ok("q=\(or_else(http.query_parameter(r, "q"), "-"))")
}

fn user(r: http.Request) -> http.Response {
    return http.ok("user \(or_else(http.parameter("/users/:id", r.path, "id"), "-"))")
}

async fn main() {
    let (server, err) = await http.open(0)
    if err != nil {
        io.print(err.message())
        return
    }
    let port = http.port_of(server)
    let routes = [http.route("GET", "/", index), http.route("GET", "/users/:id", user)]
    let running = http.run(server, routes)
    for path in ["/?q=a+b%21", "/users/7?tab=posts"] {
        let (res, gerr) = await http.get("http://127.0.0.1:\(port)\(path)")
        if gerr != nil {
            io.print("get failed: \(gerr.message())")
        } else {
            io.print("\(path) -> \(res.status) \(res.body)")
        }
    }
    let (_, lerr) = await http.send("GET", "http://127.0.0.1:\(port)/", "", "x-user: ada\nnot a header")
    io.print("a line that is not a header is refused: \(lerr != nil)")
    http.shut(server)
    let (answered, rerr) = await running
    if rerr != nil {
        io.print("run failed: \(rerr.message())")
        return
    }
    io.print("run returned after \(answered)")
    let (_, aerr) = await http.accept(server)
    io.print("accept after shut: \(if aerr == nil { "a request" } else { aerr.message() })")
}
"##;
    let client = r#"import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const server = spawn(process.execPath, [fileURLToPath(new URL("./serve.mjs", import.meta.url))], {
  stdio: ["ignore", "pipe", "inherit"],
});
let out = "";
server.stdout.on("data", (chunk) => { out += chunk; });
// A server that never stops is the failure this is about, so it is given a
// deadline rather than trusted to exit.
const timer = setTimeout(() => { try { server.kill("SIGKILL"); } catch {} }, 15000);
const code = await new Promise((resolve) => server.on("exit", resolve));
clearTimeout(timer);
process.stdout.write(out + (code === 0 ? "" : "exited with " + code + "\n"));
"#;
    let out = serve_under_node("shut", src, client);
    assert_eq!(
        out,
        "/?q=a+b%21 -> 200 q=a b!\n\
         /users/7?tab=posts -> 200 user 7\n\
         a line that is not a header is refused: true\n\
         run returned after 2\n\
         accept after shut: http.accept: the server is shut\n"
    );
}

/// A handler cannot set a header that frames the message: `run` answers 500 to
/// it, and the next request on the connection is answered by its own handler.
///
/// A `Content-Length` shorter than the body made the rest of the body the next
/// response on a keep-alive connection, so body data could cross into HTTP
/// structure.
#[test]
fn a_handler_cannot_set_a_header_that_frames_the_message() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn page(request: http.Request) -> http.Response {\n\
         \x20   let body = \"ok\\r\\nHTTP/1.1 200 OK\\r\\ncontent-length: 4\\r\\n\\r\\nEVIL\"\n\
         \x20   return http.with_header(http.ok(body), \"Content-Length\", \"2\")\n\
         }\n\
         fn chunked(request: http.Request) -> http.Response {\n\
         \x20   return http.with_header(http.ok(\"x\"), \"transfer-encoding\", \"gzip\")\n\
         }\n\
         fn other(request: http.Request) -> http.Response {\n\
         \x20   return http.ok(\"real\")\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let routes = [\n\
         \x20       http.route(\"GET\", \"/page\", page),\n\
         \x20       http.route(\"GET\", \"/chunked\", chunked),\n\
         \x20       http.route(\"GET\", \"/other\", other),\n\
         \x20   ]\n\
         \x20   let (n, rerr) = await http.run(server, routes)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const a = await fetch(`http://127.0.0.1:${port}/page`);
  console.log("page " + a.status);
  const c = await fetch(`http://127.0.0.1:${port}/chunked`);
  console.log("chunked " + c.status);
  const o = await fetch(`http://127.0.0.1:${port}/other`);
  console.log("other " + o.status + " " + (await o.text()));"#,
    );
    let out = serve_under_node("framing", src, &client);
    assert_eq!(out, "page 500\nchunked 500\nother 200 real\n", "{}", out);
}

/// A header sent twice reaches the program as one line joined with a comma,
/// which is what `request_headers`' documentation says of the Node host.
///
/// The documentation said "every value ... sent more than once", which only a
/// host that keeps each line could give; Node has already joined them.
#[test]
fn a_repeated_request_header_arrives_joined_with_a_comma() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn who(request: http.Request) -> http.Response {\n\
         \x20   let all = http.request_headers(request, \"x-dup\")\n\
         \x20   return http.ok(\"\\(all.len()) [\\(join(all, \"|\"))]\")\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let (n, rerr) = await http.run(server, [http.route(\"GET\", \"/who\", who)])\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  const http = await import("node:http");
  const text = await new Promise((resolve, reject) => {
    const req = http.request({ port, path: "/who", headers: { "x-dup": ["1", "2"] } }, (res) => {
      let body = "";
      res.on("data", (c) => { body += c; });
      res.on("end", () => resolve(body));
    });
    req.on("error", reject);
    req.end();
  });
  console.log(text);"#,
    );
    let out = serve_under_node("repeated-header", src, &client);
    assert_eq!(out, "1 [1, 2]\n", "{}", out);
}

/// A 1xx is not a final answer, and a 204, a 205 or a 304 has no body: `run` answers
/// 500 to a handler that builds one, instead of sending an interim response
/// that leaves the connection's next answer paired with the wrong request.
#[test]
fn a_final_status_is_200_to_599_and_204_205_and_304_carry_no_body() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "use std/http\n\
         fn hundred(request: http.Request) -> http.Response {\n\
         \x20   return http.status(100, \"continue body\")\n\
         }\n\
         fn empty(request: http.Request) -> http.Response {\n\
         \x20   return http.status(204, \"\")\n\
         }\n\
         fn full(request: http.Request) -> http.Response {\n\
         \x20   return http.status(304, \"stale\")\n\
         }\n\
         fn reset(request: http.Request) -> http.Response {\n\
         \x20   return http.status(205, \"content after a 205\")\n\
         }\n\
         async fn main() {\n\
         \x20   let (server, err) = await http.open(0)\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(\"listening \\(http.port_of(server))\")\n\
         \x20   let routes = [\n\
         \x20       http.route(\"GET\", \"/100\", hundred),\n\
         \x20       http.route(\"GET\", \"/204\", empty),\n\
         \x20       http.route(\"GET\", \"/304\", full),\n\
         \x20       http.route(\"GET\", \"/205\", reset),\n\
         \x20   ]\n\
         \x20   let (n, rerr) = await http.run(server, routes)\n\
         \x20   if rerr != nil {\n        io.print(rerr.message())\n    }\n\
         }\n";
    let client = client_with(
        r#"  for (const p of ["/100", "/204", "/304", "/205"]) {
    const r = await fetch(`http://127.0.0.1:${port}${p}`);
    console.log(p + " " + r.status);
  }"#,
    );
    let out = serve_under_node("final-status", src, &client);
    assert_eq!(out, "/100 500\n/204 204\n/304 500\n/205 500\n", "{}", out);
}
