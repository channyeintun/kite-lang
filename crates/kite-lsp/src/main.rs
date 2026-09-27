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
use std::io::{BufRead, Write};
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
                write_message(
                    output,
                    &Json::object(vec![
                        ("jsonrpc", Json::str("2.0")),
                        // The id is in the message that could not be read.
                        ("id", Json::Null),
                        (
                            "error",
                            Json::object(vec![
                                ("code", Json::number(-32700)),
                                ("message", Json::str("the message is not valid JSON")),
                            ]),
                        ),
                    ]),
                );
                continue;
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
    /// The editor closed the stream, or it ended inside a message.
    Closed,
}

/// Read one `Content-Length`-framed message.
fn read_message(input: &mut impl BufRead) -> Incoming {
    let mut length = None;
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return Incoming::Closed,
            Ok(_) => {}
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else { return Incoming::Malformed };
    let mut body = vec![0u8; length];
    if input.read_exact(&mut body).is_err() {
        return Incoming::Closed;
    }
    match std::str::from_utf8(&body).ok().and_then(json::parse) {
        Some(message) => Incoming::Message(message),
        None => Incoming::Malformed,
    }
}

fn write_message(output: &mut impl Write, message: &Json) {
    let body = message.to_text();
    let _ = write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests;
