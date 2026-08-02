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

fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut server = server::Server::new();

    while let Some(message) = read_message(&mut input) {
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
            continue;
        };
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
                    &mut output,
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
                    &mut output,
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
                &mut output,
                &Json::object(vec![
                    ("jsonrpc", Json::str("2.0")),
                    ("method", Json::str(method)),
                    ("params", params),
                ]),
            );
        }
        if server.shutdown && method == "exit" {
            break;
        }
    }
}

/// Read one `Content-Length`-framed message.
fn read_message(input: &mut impl BufRead) -> Option<Json> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; length?];
    input.read_exact(&mut body).ok()?;
    json::parse(std::str::from_utf8(&body).ok()?)
}

fn write_message(output: &mut impl Write, message: &Json) {
    let body = message.to_text();
    let _ = write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests;
