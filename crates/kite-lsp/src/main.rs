//! `kite-lsp` — the language server.
//!
//! An editor extension that implements its own analysis is one that only ever
//! works in that editor, and one whose answers drift from the compiler's. This
//! runs the same passes `kitec` runs, over stdio, in the protocol every editor
//! already speaks.
//!
//! Messages are `Content-Length`-framed JSON. Both halves of that are written
//! by hand here — the framing is six lines and the JSON is two hundred — which
//! keeps the compiler's dependency list at nothing a build has to fetch.

mod json;
mod server;

use json::Json;
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    // A document is compiled the moment it is opened, whatever it holds, and
    // the compiler's passes recurse as deep as a chain in it is long: on a
    // main thread's stack a chain the parser accepts could end the session.
    ExitCode::from(kite_driver::on_compiler_stack(|| {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        serve(&mut stdin.lock(), &mut stdout.lock())
    }))
}

/// Answer messages until the editor says to stop or goes away, and say with
/// what exit status.
fn serve(input: &mut impl BufRead, output: &mut impl Write) -> u8 {
    let mut server = server::Server::new();

    loop {
        let message = match read_message(input) {
            Incoming::Closed => return 0,
            // One bad message is one bad message. It used to end the loop,
            // and the session with it, exiting as if asked to — the editor
            // saw its server vanish over a request it would have forgotten.
            Incoming::Malformed => {
                write_message(output, &unreadable("the message is not valid JSON"));
                continue;
            }
            // **Never allocated as stated.** The body buffer was made the size
            // the header said, so `Content-Length: 18446744073709551615`
            // panicked on the capacity and a trillion aborted on the
            // allocation — the server gone over one header. The body is read
            // and dropped instead, which keeps the stream framed the way the
            // header framed it; a stream that ends first has ended.
            Incoming::TooLarge(length) => {
                write_message(
                    output,
                    &unreadable(&format!(
                        "the message is {} bytes, and nothing this protocol sends is over {}",
                        length, MAX_BODY
                    )),
                );
                match std::io::copy(&mut input.by_ref().take(length), &mut std::io::sink()) {
                    Ok(n) if n == length => continue,
                    _ => return 0,
                }
            }
            Incoming::Message(message) => message,
        };
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
            continue;
        };
        // The protocol's rule: 0 after a `shutdown`, 1 without one, so an
        // editor that stopped its server without asking is told it did.
        if method == "exit" {
            return if server.shutdown { 0 } else { 1 };
        }
        let id = message.get("id").cloned();
        let reply = server.handle(method, &message);

        // A request has an id and expects an answer; a notification has
        // neither, and answering one is a protocol error.
        if let Some(id) = id {
            if let Some(message) = reply.error {
                // A refusal with its reason. -32803 is the protocol's
                // RequestFailed: the request was understood, and the answer
                // is no — which the editor shows, where an empty result
                // would just look like nothing happening.
                write_message(
                    output,
                    &Json::object(vec![
                        ("jsonrpc", Json::str("2.0")),
                        ("id", id),
                        (
                            "error",
                            Json::object(vec![
                                ("code", Json::number(-32803)),
                                ("message", Json::str(message)),
                            ]),
                        ),
                    ]),
                );
            } else if let Some(result) = reply.result {
                write_message(
                    output,
                    &Json::object(vec![
                        ("jsonrpc", Json::str("2.0")),
                        ("id", id),
                        ("result", result),
                    ]),
                );
            }
        }
        for (method, params) in reply.notifications {
            write_message(
                output,
                &Json::object(vec![
                    ("jsonrpc", Json::str("2.0")),
                    ("method", Json::str(method)),
                    ("params", params),
                ]),
            );
        }
    }
}

/// What arrived on the input.
enum Incoming {
    Message(Json),
    /// Framed, and not something this can read: not JSON, not UTF-8, or
    /// headers without a length. The next message is still readable.
    Malformed,
    /// Framed with a length no message of this protocol has, and not yet read.
    TooLarge(u64),
    /// The editor closed the stream, or it ended inside a message.
    Closed,
}

/// The largest body read into memory. The protocol's messages are a file's
/// text and change at most, and a length past this is not one of them.
const MAX_BODY: u64 = 64 << 20;

/// Read one `Content-Length`-framed message.
fn read_message(input: &mut impl BufRead) -> Incoming {
    let mut length = None;
    loop {
        // Bytes, not a `String`: a header line that is not UTF-8 made
        // `read_line` fail, which read as the editor closing the stream, and
        // the session ended with no answer to anything. Headers are ASCII, so
        // such a line is not `Content-Length` and is passed over like any
        // other header this does not read.
        let mut line = Vec::new();
        match input.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return Incoming::Closed,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&line);
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse::<u64>().ok();
        }
    }
    let Some(length) = length else { return Incoming::Malformed };
    if length > MAX_BODY {
        return Incoming::TooLarge(length);
    }
    let mut body = vec![0u8; length as usize];
    if input.read_exact(&mut body).is_err() {
        return Incoming::Closed;
    }
    match std::str::from_utf8(&body).ok().and_then(json::parse) {
        Some(message) => Incoming::Message(message),
        None => Incoming::Malformed,
    }
}

/// The protocol's parse error, for a message that could not be read — whose
/// id is in the part that could not be read.
fn unreadable(why: &str) -> Json {
    Json::object(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", Json::Null),
        (
            "error",
            Json::object(vec![("code", Json::number(-32700)), ("message", Json::str(why))]),
        ),
    ])
}

fn write_message(output: &mut impl Write, message: &Json) {
    let body = message.to_text();
    let _ = write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests;
