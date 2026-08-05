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

/// The default runner: instantiate, run, print what the program wrote.
const PLAIN_RUNNER: &str = "import { readFile } from \"node:fs/promises\";\n\
     import { run, setWriter } from \"./app.js\";\n\
     const out = [];\n\
     setWriter((l) => out.push(l));\n\
     await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";

/// Compile a program to Wasm and run it under Node, returning its output.
fn run_under_node(name: &str, src: &str) -> String {
    run_runner_under_node(name, src, PLAIN_RUNNER, &[])
}

/// The same, with the runner written out and the node arguments given.
///
/// Streams need something to stream from, so the tests that use them bring a
/// server along in the runner rather than reaching for a network that may or
/// may not be there.
fn run_runner_under_node(name: &str, src: &str, runner: &str, args: &[&str]) -> String {
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
    std::fs::write(dir.join("run.mjs"), runner).expect("write runner");

    let output = Command::new("node")
        .args(args)
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

/// A port nothing is listening on, for a test that needs to be reached.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    drop(listener);
    port
}

/// A server for the tests below: it echoes the method and body of any request,
/// streams three events at `/events`, and echoes text frames over a socket at
/// `/ws`.
///
/// The socket half is written out by hand because Node has the WebSocket
/// *client* and no server, and a dependency taken on for one test would be a
/// dependency in the build. It reads one small masked frame at a time, which
/// is all these tests send.
fn server_runner(port: u16) -> String {
    format!(
        r#"import {{ readFile }} from "node:fs/promises";
import {{ createServer }} from "node:http";
import {{ createHash }} from "node:crypto";
import {{ writeSync }} from "node:fs";
import {{ run, setWriter }} from "./app.js";

const out = [];
setWriter((l) => out.push(l));

const server = createServer((req, res) => {{
  if (req.url === "/events") {{
    res.writeHead(200, {{
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    }});
    res.write("id: 1\ndata: first\n\n");
    res.write("event: tick\nid: 2\ndata: second\n\n");
    res.write("data: line one\ndata: line two\n\n");
    return;
  }}
  let body = "";
  req.on("data", (c) => {{ body += c; }});
  req.on("end", () => {{
    res.writeHead(200, {{ "content-type": "text/plain" }});
    res.end(req.method + "|" + body);
  }});
}});

server.on("upgrade", (req, socket) => {{
  const key = req.headers["sec-websocket-key"];
  const accept = createHash("sha1")
    .update(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
    .digest("base64");
  socket.write(
    "HTTP/1.1 101 Switching Protocols\r\n" +
      "Upgrade: websocket\r\nConnection: Upgrade\r\n" +
      "Sec-WebSocket-Accept: " + accept + "\r\n\r\n",
  );
  socket.on("data", (buf) => {{
    if ((buf[0] & 0x0f) !== 1) return;
    const len = buf[1] & 0x7f;
    const mask = buf.subarray(2, 6);
    const data = buf.subarray(6, 6 + len);
    let text = "";
    for (let i = 0; i < len; i++) text += String.fromCharCode(data[i] ^ mask[i % 4]);
    const reply = Buffer.from("echo:" + text);
    socket.write(Buffer.concat([Buffer.from([0x81, reply.length]), reply]));
  }});
}});

await new Promise((r) => server.listen({port}, "127.0.0.1", r));
await run(new Uint8Array(await readFile(new URL("./app.wasm", import.meta.url))));
// Written synchronously: `process.stdout` is a pipe here, and a pipe's writes
// are asynchronous on Windows — so the exit below could outrun the output.
writeSync(1, out.map((l) => l + "\n").join(""));
// The event-stream and socket tests leave keep-alive connections open on
// purpose, and exiting while those are mid-close trips an assertion inside
// libuv on Windows. Destroying them first is what makes the shutdown
// deterministic rather than a race the platform sometimes loses.
server.closeAllConnections();
server.close();
// A stream the program left open would keep the loop alive, and the program
// is over.
process.exit(0);
"#,
        port = port
    )
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

/// Every method reaches the server as itself, QUERY included.
///
/// QUERY is the reason this test exists: it is a draft method with a body, and
/// what makes it work is that `fetch` forbids only CONNECT, TRACE and TRACK
/// and passes any other token through. A server echoing the method back is the
/// only way to know that actually happened rather than being normalised into
/// something else on the way out.
#[test]
fn every_method_reaches_the_server_including_query() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let port = free_port();
    let src = format!(
        "use std/http\n\
         async fn main() {{\n\
         \x20 let at = \"http://127.0.0.1:{port}/\"\n\
         \x20 let (q, qe) = await http.query(at, \"{{}}\")\n\
         \x20 let (p, pe) = await http.patch(at, \"{{}}\")\n\
         \x20 let (o, oe) = await http.options(at)\n\
         \x20 let (h, he) = await http.head(at)\n\
         \x20 if qe != nil {{ return }}\n\
         \x20 if pe != nil {{ return }}\n\
         \x20 if oe != nil {{ return }}\n\
         \x20 if he != nil {{ return }}\n\
         \x20 io.print(q.body)\n\
         \x20 io.print(p.body)\n\
         \x20 io.print(o.body)\n\
         \x20 io.print(\"HEAD \\(h.status) \\(http.succeeded(h))\")\n}}\n",
        port = port
    );
    let out = run_runner_under_node("methods", &src, &server_runner(port), &[]);
    assert_eq!(out, "QUERY|{}\nPATCH|{}\nOPTIONS|\nHEAD 200 true\n", "{}", out);
}

/// A 404 is not a success, which is the whole point of `succeeded` being a
/// range and not a floor.
#[test]
fn a_404_is_a_response_and_not_a_success() {
    let src = "use std/http\n\
        fn main() {\n\
        \x20 io.print(http.succeeded(http.ok(\"fine\")))\n\
        \x20 io.print(http.succeeded(http.not_found()))\n\
        \x20 io.print(http.succeeded(http.status(500, \"no\")))\n\
        \x20 io.print(http.succeeded(http.status(204, \"\")))\n}\n";
    let c = compile("status.kite", src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("runs");
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "true\nfalse\nfalse\ntrue\n"
    );
}

/// Server-sent events arrive in order, named events included, and the data of
/// a multi-line event arrives whole.
#[test]
fn server_sent_events_arrive_in_order() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let port = free_port();
    let src = format!(
        "use std/http\n\
         async fn main() {{\n\
         \x20 let (s, err) = await http.events_named(\"http://127.0.0.1:{port}/events\", \"tick\")\n\
         \x20 if err != nil {{\n    io.print(\"failed: \\(err.message())\")\n    return\n  }}\n\
         \x20 for i in 0..3 {{\n\
         \x20   let (e, re) = await http.receive(s)\n\
         \x20   if re != nil {{\n      io.print(\"error\")\n      return\n    }}\n\
         \x20   io.print(\"\\(e.name) \\(e.id) \\(e.data)\")\n  }}\n\
         \x20 http.close(s)\n}}\n",
        port = port
    );
    let out = run_runner_under_node(
        "sse",
        &src,
        &server_runner(port),
        &["--experimental-eventsource"],
    );
    assert_eq!(
        out,
        "message 1 first\ntick 2 second\nmessage 2 line one\nline two\n",
        "{}",
        out
    );
}

/// Closing drains rather than discards, and then says it was closed — not
/// that it failed.
///
/// Two things are being pinned. Events that already arrived are still the
/// program's to read, because throwing away data the host already delivered
/// would lose it silently. And once they are read the stream reports being
/// closed, which is a different bug from a stream whose network went away and
/// sends a reader looking somewhere else.
#[test]
fn a_closed_event_stream_drains_then_says_it_is_closed() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let port = free_port();
    let src = format!(
        "use std/http\n\
         async fn main() {{\n\
         \x20 let (s, err) = await http.events(\"http://127.0.0.1:{port}/events\")\n\
         \x20 if err != nil {{\n    io.print(\"failed to open\")\n    return\n  }}\n\
         \x20 http.close(s)\n\
         \x20 var read = 0\n\
         \x20 for {{\n\
         \x20   let (e, re) = await http.receive(s)\n\
         \x20   if re != nil {{\n      io.print(\"\\(read > 0) \\(re.message())\")\n      return\n    }}\n\
         \x20   read = read + e.data.len()\n  }}\n}}\n",
        port = port
    );
    let out = run_runner_under_node(
        "sse-closed",
        &src,
        &server_runner(port),
        &["--experimental-eventsource"],
    );
    assert_eq!(out, "true sse: the stream is closed\n", "{}", out);
}

/// A socket connects, carries a message each way, and closes.
#[test]
fn a_socket_carries_messages_both_ways() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let port = free_port();
    let src = format!(
        "use std/socket\n\
         async fn main() {{\n\
         \x20 let (s, err) = await socket.connect(\"ws://127.0.0.1:{port}/ws\")\n\
         \x20 if err != nil {{\n    io.print(\"failed: \\(err.message())\")\n    return\n  }}\n\
         \x20 io.print(\"open \\(socket.open(s))\")\n\
         \x20 let sent = socket.send(s, \"ping\")\n\
         \x20 if sent != nil {{\n    io.print(\"send failed\")\n    return\n  }}\n\
         \x20 let (message, re) = await socket.receive(s)\n\
         \x20 if re != nil {{\n    io.print(\"receive failed\")\n    return\n  }}\n\
         \x20 io.print(message)\n\
         \x20 socket.close(s)\n\
         \x20 io.print(\"open \\(socket.open(s))\")\n}}\n",
        port = port
    );
    let out = run_runner_under_node("socket", &src, &server_runner(port), &[]);
    assert_eq!(out, "open true\necho:ping\nopen false\n", "{}", out);
}

/// Sending on a socket that never opened is an error, not a silent drop.
#[test]
fn a_socket_that_cannot_connect_says_so() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "socket-refused",
        "use std/socket\n\
         async fn main() {\n\
         \x20 let (s, err) = await socket.connect(\"ws://127.0.0.1:9/nothing\")\n\
         \x20 io.print(if err == nil { \"unexpectedly open\" } else { \"refused\" })\n}\n",
    );
    assert_eq!(out, "refused\n", "{}", out);
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

/// Sealed text opens back to the plaintext, and stops opening the moment a
/// character changes or the key is wrong — an authentication failure is an
/// error, not a wrong answer. Importing the same hex twice gives keys that
/// agree, which is what makes a stored key usable.
#[test]
fn aes_gcm_seals_opens_and_rejects_tampering() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "seal",
        "use std/crypto\n\
         async fn main() {\n\
         \x20 let (key, kerr) = await crypto.generate_key()\n\
         \x20 if kerr != nil {\n    io.print(\"keygen failed: \\(kerr.message())\")\n    return\n  }\n\
         \x20 let (sealed, serr) = await crypto.seal(key, \"the plan, in the usual place\")\n\
         \x20 if serr != nil {\n    io.print(\"seal failed: \\(serr.message())\")\n    return\n  }\n\
         \x20 let (clear, oerr) = await crypto.open(key, sealed)\n\
         \x20 if oerr != nil {\n    io.print(\"open failed: \\(oerr.message())\")\n    return\n  }\n\
         \x20 io.print(clear)\n\
         \x20 let (altered, terr) = await crypto.open(key, sealed + \"00\")\n\
         \x20 io.print(\"tampered rejected \\(terr != nil)\")\n\
         \x20 let (other, gerr) = await crypto.generate_key()\n\
         \x20 if gerr != nil { return }\n\
         \x20 let (stolen, werr) = await crypto.open(other, sealed)\n\
         \x20 io.print(\"wrong key rejected \\(werr != nil)\")\n\
         \x20 let material = crypto.random(32)\n\
         \x20 let (a, ae) = await crypto.import_key(material)\n\
         \x20 if ae != nil { return }\n\
         \x20 let (b, be) = await crypto.import_key(material)\n\
         \x20 if be != nil { return }\n\
         \x20 let (again, aerr) = await crypto.seal(a, \"again\")\n\
         \x20 if aerr != nil { return }\n\
         \x20 let (back, berr) = await crypto.open(b, again)\n\
         \x20 if berr != nil {\n    io.print(\"import failed: \\(berr.message())\")\n    return\n  }\n\
         \x20 io.print(back)\n}\n",
    );
    assert_eq!(
        out,
        "the plan, in the usual place\ntampered rejected true\nwrong key rejected true\nagain\n",
        "{}",
        out
    );
}

/// A signature verifies against the exported public key, and stops verifying
/// when the message changes or the signature is not the real one.
#[test]
fn ed25519_signs_and_verifies() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "sign",
        "use std/crypto\n\
         async fn main() {\n\
         \x20 let (pair, perr) = await crypto.signing_key()\n\
         \x20 if perr != nil {\n    io.print(\"keygen failed: \\(perr.message())\")\n    return\n  }\n\
         \x20 let (public, kerr) = await crypto.verify_key(pair)\n\
         \x20 if kerr != nil { return }\n\
         \x20 io.print(public.len())\n\
         \x20 let (signature, serr) = await crypto.sign(pair, \"a promise\")\n\
         \x20 if serr != nil { return }\n\
         \x20 io.print(signature.len())\n\
         \x20 let (good, gerr) = await crypto.verify(public, \"a promise\", signature)\n\
         \x20 if gerr != nil {\n    io.print(\"verify failed: \\(gerr.message())\")\n    return\n  }\n\
         \x20 let (other, oerr) = await crypto.verify(public, \"another promise\", signature)\n\
         \x20 if oerr != nil { return }\n\
         \x20 let (forged, ferr) = await crypto.verify(public, \"a promise\", crypto.random(64))\n\
         \x20 if ferr != nil { return }\n\
         \x20 io.print(\"\\(good) \\(other) \\(forged)\")\n}\n",
    );
    assert_eq!(out, "64\n128\ntrue false false\n", "{}", out);
}

/// Two sides that exchange public halves derive the same key — one seals and
/// the other opens — and a third pair derives something else, which cannot.
#[test]
fn x25519_agreement_derives_the_same_key_on_both_sides() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = run_under_node(
        "agree",
        "use std/crypto\n\
         async fn main() {\n\
         \x20 let (alice, aerr) = await crypto.agreement_key()\n\
         \x20 if aerr != nil {\n    io.print(\"keygen failed: \\(aerr.message())\")\n    return\n  }\n\
         \x20 let (bob, berr) = await crypto.agreement_key()\n\
         \x20 if berr != nil { return }\n\
         \x20 let (to_bob, e1) = await crypto.exchange_key(alice)\n\
         \x20 if e1 != nil { return }\n\
         \x20 let (to_alice, e2) = await crypto.exchange_key(bob)\n\
         \x20 if e2 != nil { return }\n\
         \x20 let (hers, e3) = await crypto.agree(alice, to_alice)\n\
         \x20 if e3 != nil {\n    io.print(\"agree failed: \\(e3.message())\")\n    return\n  }\n\
         \x20 let (his, e4) = await crypto.agree(bob, to_bob)\n\
         \x20 if e4 != nil { return }\n\
         \x20 let (sealed, e5) = await crypto.seal(hers, \"meet where we said\")\n\
         \x20 if e5 != nil { return }\n\
         \x20 let (clear, e6) = await crypto.open(his, sealed)\n\
         \x20 if e6 != nil {\n    io.print(\"shared keys differ: \\(e6.message())\")\n    return\n  }\n\
         \x20 io.print(clear)\n\
         \x20 let (eve, e7) = await crypto.agreement_key()\n\
         \x20 if e7 != nil { return }\n\
         \x20 let (guessed, e8) = await crypto.agree(eve, to_alice)\n\
         \x20 if e8 != nil { return }\n\
         \x20 let (overheard, e9) = await crypto.open(guessed, sealed)\n\
         \x20 io.print(\"eavesdropper rejected \\(e9 != nil)\")\n}\n",
    );
    assert_eq!(
        out,
        "meet where we said\neavesdropper rejected true\n",
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

// ---- host objects ----------------------------------------------------------

/// A runner that supplies a `js` group and two objects to point at.
///
/// Nothing here is a DOM: what is being tested is that a reference the host
/// made survives a round trip through Kite, including being stored in a struct
/// field and taken out again. A plain object proves that as well as a node
/// would, and needs no browser.
const HOST_OBJECT_RUNNER: &str = "import { readFile } from \"node:fs/promises\";\n\
     import { run, provide, setWriter, str, text } from \"./app.js\";\n\
     const WORLD = { document: { tag: \"doc\" }, window: { tag: \"win\" } };\n\
     // A `str` crosses as an index into the glue's table unless the module was\n\
     // built with --js-strings, so a host reads one with `text` and returns one\n\
     // with `str`. A `JsValue` needs neither: it is already the object.\n\
     provide(\"js\", {\n\
     \x20 global: (name) => WORLD[text(name)],\n\
     \x20 same: (a, b) => a === b,\n\
     \x20 tag_of: (v) => str(v.tag),\n\
     });\n\
     const out = [];\n\
     setWriter((l) => out.push(l));\n\
     // `run` calls `main` and then drives whatever it started, which an\n\
     // `async fn main` needs and a plain one does not mind.\n\
     await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";

const HOST_OBJECT_DECLS: &str = "@host(\"js\")\nextern fn global(name: str) -> JsValue\n\
     @host(\"js\")\nextern fn same(a: JsValue, b: JsValue) -> bool\n\
     @host(\"js\")\nextern fn tag_of(v: JsValue) -> str\n";

/// A host object crosses into Kite, is held, and goes back out unchanged.
///
/// This is the property the integer handle table could not provide. Two lookups
/// of the same thing pushed two entries and produced two different numbers, so
/// `same` answered false for one object — and every event handler ever written
/// asks exactly that question about `event.target`.
#[test]
fn a_host_object_keeps_its_identity_across_the_boundary() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = format!(
        "{}struct Element {{\n  raw: JsValue\n}}\n\
         fn main() {{\n\
         \x20 let a = global(\"document\")\n\
         \x20 let b = global(\"window\")\n\
         \x20 io.print(same(a, a))\n\
         \x20 io.print(same(a, b))\n\
         \x20 let e = Element{{ raw: a }}\n\
         \x20 io.print(same(e.raw, a))\n\
         \x20 io.print(tag_of(e.raw))\n}}\n",
        HOST_OBJECT_DECLS
    );
    let out = run_runner_under_node("hostobject", &src, HOST_OBJECT_RUNNER, &[]);
    assert_eq!(out, "true\nfalse\ntrue\ndoc\n");
}

/// A host object survives an `await`.
///
/// The reference is held across a suspension point, which means it is a field
/// of the state machine `asyncify` builds rather than a value on the stack. An
/// `externref` in a GC struct is exactly what makes that work without the
/// program doing anything.
#[test]
fn a_host_object_survives_a_suspension() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = format!(
        "use std/task\n{}async fn later(v: JsValue) -> str {{\n\
         \x20 await task.sleep(1)\n  return tag_of(v)\n}}\n\
         async fn main() {{\n\
         \x20 let a = global(\"document\")\n\
         \x20 io.print(await later(a))\n\
         \x20 io.print(same(a, global(\"document\")))\n}}\n",
        HOST_OBJECT_DECLS
    );
    let out = run_runner_under_node("hostawait", &src, HOST_OBJECT_RUNNER, &[]);
    assert_eq!(out, "doc\ntrue\n");
}

/// `JsValue` is refused by the targets that have no host, rather than lowered
/// to a number that would mean nothing there.
#[test]
fn a_host_object_is_refused_off_the_web() {
    let src = format!(
        "{}fn main() {{\n  io.print(same(global(\"a\"), global(\"a\")))\n}}\n",
        HOST_OBJECT_DECLS
    );
    // Checking is target-independent: this is valid Kite, and an editor must
    // not report it as broken.
    let checked = compile(Path::new("hostoff.kite"), &src, Emit::Check);
    assert!(!checked.failed(), "{}", checked.render_diagnostics());

    let bytecode = compile(Path::new("hostoff.kite"), &src, Emit::Kbc);
    assert!(bytecode.failed(), "the bytecode target has no host to refer to");
    let text = bytecode.render_diagnostics();
    assert!(text.contains("E0204"), "{}", text);
    assert!(text.contains("has no host"), "{}", text);
}

// ---- std/js ----------------------------------------------------------------

/// `std/js` needs no host of its own: the `js` group is supplied by the
/// generated glue, which is the whole claim being tested. The runner builds a
/// small world on `globalThis` for the program to reach.
const JS_RUNNER: &str = "import { readFile } from \"node:fs/promises\";\n\
     import { run, setWriter } from \"./app.js\";\n\
     globalThis.probe = {\n\
     \x20 name: \"kite\",\n\
     \x20 count: 7,\n\
     \x20 ratio: 1.5,\n\
     \x20 on: true,\n\
     \x20 missing: undefined,\n\
     \x20 items: [10, 20, 30],\n\
     \x20 twice: function (n) { return n * 2; },\n\
     \x20 boom: function () { throw new Error(\"no\"); },\n\
     };\n\
     const out = [];\n\
     setWriter((l) => out.push(l));\n\
     await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";

fn run_js_program(name: &str, body: &str) -> String {
    let src = format!("use std/js\n\nfn main() {{\n{}}}\n", body);
    run_runner_under_node(name, &src, JS_RUNNER, &[])
}

/// Reading properties, converting, indexing and calling.
#[test]
fn std_js_reads_and_calls() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = "use std/js\n\n\
        fn report() -> (str, error) {\n\
        \x20 let probe = js.global(\"probe\")\n\
        \x20 let (count, err) = js.get(probe, \"count\")\n\
        \x20 check err\n\
        \x20 let (n, nerr) = js.as_int(count)\n\
        \x20 check nerr\n\
        \x20 let (items, ierr) = js.get(probe, \"items\")\n\
        \x20 check ierr\n\
        \x20 let (len, lerr) = js.length(items)\n\
        \x20 check lerr\n\
        \x20 let (second, serr) = js.at(items, 1)\n\
        \x20 check serr\n\
        \x20 let (twenty, terr) = js.as_int(second)\n\
        \x20 check terr\n\
        \x20 let (doubled, derr) = js.call1(probe, \"twice\", js.of_num(21.0))\n\
        \x20 check derr\n\
        \x20 let (forty_two, ferr) = js.as_num(doubled)\n\
        \x20 check ferr\n\
        \x20 return \"\\(n) \\(len) \\(twenty) \\(forty_two)\", nil\n\
        }\n\
        fn main() {\n\
        \x20 let (line, err) = report()\n\
        \x20 if err != nil {\n    io.print(\"failed: \\(err.message())\")\n    return\n  }\n\
        \x20 io.print(line)\n\
        }\n";
    let out = run_runner_under_node("jsread", src, JS_RUNNER, &[]);
    assert_eq!(out, "7 3 20 42.0\n");
}

/// A host exception becomes an ordinary error, and the program carries on.
///
/// This is the property that makes the whole design survivable. Without it a
/// throw unwinds through the Wasm frames and aborts the scheduler, so one
/// mistyped method name stops every running task rather than failing one call.
#[test]
fn a_host_exception_becomes_an_error() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = "use std/js\n\n\
        fn main() {\n\
        \x20 let probe = js.global(\"probe\")\n\
        \x20 let (v, err) = js.call0(probe, \"boom\")\n\
        \x20 if err != nil {\n    io.print(err.message())\n  }\n\
        \x20 let (w, werr) = js.call0(probe, \"nosuchmethod\")\n\
        \x20 if werr != nil {\n    io.print(\"second failed too\")\n  }\n\
        \x20 io.print(js.str_or(probe, \"name\", \"?\"))\n\
        }\n";
    let out = run_runner_under_node("jsthrow", src, JS_RUNNER, &[]);
    assert_eq!(
        out,
        "the host threw: no\nsecond failed too\nkite\n",
        "a throw must not stop the program"
    );
}

/// `undefined` cannot become a number by accident.
///
/// JavaScript's commonest bug is a missing property arriving as `undefined` and
/// turning into `NaN` or `0` somewhere else entirely. Here it is a value the
/// taint analysis will not let a caller read until the error is tested.
#[test]
fn a_missing_property_is_an_error_and_not_a_zero() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = "use std/js\n\n\
        fn main() {\n\
        \x20 let probe = js.global(\"probe\")\n\
        \x20 let (v, err) = js.get(probe, \"missing\")\n\
        \x20 if err != nil {\n    io.print(\"unexpected\")\n    return\n  }\n\
        \x20 io.print(js.is_nothing(v))\n\
        \x20 let (n, nerr) = js.as_num(v)\n\
        \x20 if nerr != nil {\n    io.print(nerr.message())\n  }\n\
        \x20 io.print(js.num_or(probe, \"missing\", -1.0))\n\
        }\n";
    let out = run_runner_under_node("jsmissing", src, JS_RUNNER, &[]);
    assert_eq!(
        out,
        "true\nexpected a number, and the value is undefined\n-1.0\n"
    );
}

/// Numbers cross as `f64`, and the limit that implies is refused rather than
/// discovered as a value that came back different from the one that went out.
#[test]
fn an_integer_too_large_for_a_javascript_number_is_refused() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = "use std/js\n\n\
        fn main() {\n\
        \x20 let (ok, err) = js.of_int(9007199254740991)\n\
        \x20 io.print(err == nil)\n\
        \x20 let (bad, berr) = js.of_int(9007199254740993)\n\
        \x20 if berr != nil {\n    io.print(\"refused\")\n  }\n\
        \x20 let probe = js.global(\"probe\")\n\
        \x20 let (ratio, rerr) = js.get(probe, \"ratio\")\n\
        \x20 if rerr != nil {\n    return\n  }\n\
        \x20 let (whole, werr) = js.as_int(ratio)\n\
        \x20 if werr != nil {\n    io.print(werr.message())\n  }\n\
        }\n";
    let out = run_runner_under_node("jsnumbers", src, JS_RUNNER, &[]);
    assert_eq!(out, "true\nrefused\n1.5 is not a whole number\n");
}

// ---- the resident driver ---------------------------------------------------

/// A runner that hands the module to `resident` rather than running it to
/// completion, and waits long enough for a real timer to fire.
///
/// `run` is the batch driver: it returns when the last task finishes, on a
/// virtual clock that jumps to the next deadline. That is right for a test and
/// wrong for a page, and the difference is what these assert.
const RESIDENT_RUNNER: &str = "import { readFile } from \"node:fs/promises\";\n\
     import { instantiate, resident, setWriter } from \"./app.js\";\n\
     const out = [];\n\
     setWriter((l) => out.push(l));\n\
     const started = Date.now();\n\
     const exports = await instantiate(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
     exports.main();\n\
     resident(exports);\n\
     await new Promise((r) => setTimeout(r, 400));\n\
     out.push(\"elapsed at least 150: \" + (Date.now() - started >= 150));\n\
     process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";

/// A program outlives `main`, and its sleep takes real time.
///
/// Two things at once, because they are the same fault seen from two sides. The
/// batch driver returns when the last task finishes, so a click arriving later
/// would have nothing to run it; and its clock jumps to the next deadline, so
/// `task.sleep(150)` returns immediately. In a page both are wrong.
#[test]
fn a_resident_program_outlives_main_and_sleeps_for_real() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let src = "use std/task\n\n\
        async fn later() {\n\
        \x20 await task.sleep(150)\n\
        \x20 io.print(\"woke\")\n\
        }\n\
        fn main() {\n\
        \x20 io.print(\"started\")\n\
        \x20 later()\n\
        }\n";
    let out = run_runner_under_node("resident", src, RESIDENT_RUNNER, &[]);
    assert_eq!(
        out, "started\nwoke\nelapsed at least 150: true\n",
        "the task must run after main returned, and the sleep must be real"
    );
}

/// The batch driver is unchanged, and its clock still jumps.
///
/// The virtual clock is not a bug to be fixed: it is what lets three backends
/// be compared, because a scheduler racing real timers produces a different
/// interleaving every run. This asserts the two modes stayed different.
#[test]
fn the_batch_driver_still_runs_on_a_virtual_clock() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let runner = "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         const started = Date.now();\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         out.push(\"under 400ms: \" + (Date.now() - started < 400));\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";
    let src = "use std/task\n\n\
        async fn main() {\n\
        \x20 await task.sleep(5000)\n\
        \x20 io.print(\"slept\")\n\
        }\n";
    let out = run_runner_under_node("virtualclock", src, runner, &[]);
    assert_eq!(
        out, "slept\nunder 400ms: true\n",
        "a five-second sleep must cost no real time in the test mode"
    );
}

/// An idle resident program schedules nothing.
///
/// This is the third fault the phase set out to fix, and the one that decides
/// whether a Kite island is something you would put on a page you care about.
/// The old driver spun on a zero-delay timeout for as long as anything waited
/// on the host — a core burned for the lifetime of the tab. Counting timers is
/// a direct measurement of it: once there is no work, there must be no timer,
/// and the program must cost exactly nothing until something wakes it.
#[test]
fn an_idle_resident_program_schedules_nothing() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let runner = "import { readFile } from \"node:fs/promises\";\n\
         import { instantiate, resident, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         const realSetTimeout = globalThis.setTimeout;\n\
         let timers = 0;\n\
         globalThis.setTimeout = (fn, ms) => { timers += 1; return realSetTimeout(fn, ms); };\n\
         const exports = await instantiate(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         exports.main();\n\
         resident(exports);\n\
         await new Promise((r) => realSetTimeout(r, 50));\n\
         const settled = timers;\n\
         await new Promise((r) => realSetTimeout(r, 300));\n\
         out.push(\"idle timers: \" + (timers - settled));\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n";
    let src = "fn main() {\n  io.print(\"done\")\n}\n";
    let out = run_runner_under_node("residentidle", src, runner, &[]);
    assert_eq!(
        out, "done\nidle timers: 0\n",
        "an idle program must not schedule anything"
    );
}
