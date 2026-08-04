//! What answers `@host("…")` when a program runs under the bytecode VM.
//!
//! The VM declares a [`Host`] trait and calls it by `"namespace.name"`; until
//! now the driver passed `None`, so every `extern` was a trap. This is the
//! native side of `std/fs` — the one namespace a command-line program needs
//! and a browser cannot have.
//!
//! Failures cross as a string with a leading `\u{1}`, which is what `std/fs`
//! unwraps back into an `error`. A sentinel is used rather than a second
//! return value because one `str` is what an `extern` can carry, and no text
//! file begins with a control character.

use kite_vm::{Host, Trap, Value};
use std::rc::Rc;

/// Marks a returned string as a failure. Must match `fs.FAILURE_MARK`.
const FAILURE: char = '\u{1}';

pub struct NativeHost;

fn failure(message: impl std::fmt::Display) -> Value {
    Value::Str(Rc::from(format!("{}{}", FAILURE, message).as_str()))
}

fn ok(text: impl Into<String>) -> Value {
    Value::Str(Rc::from(text.into().as_str()))
}

fn path_of(args: &[Value], at: usize, name: &'static str) -> Result<String, Trap> {
    match args.get(at) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(Trap::TypeConfusion { op: name, found: "not a str" }),
    }
}

impl Host for NativeHost {
    fn call(&mut self, name: &str, args: &[Value]) -> Result<Value, Trap> {
        match name {
            "fs.read_text" => {
                let path = path_of(args, 0, "fs.read")?;
                Ok(match std::fs::read(&path) {
                    // Not `read_to_string`, so that "this file is not text" is
                    // a message rather than a panic — and not lossy decoding,
                    // which turns a binary file into plausible-looking rubbish.
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => ok(text),
                        Err(_) => failure("not valid UTF-8"),
                    },
                    Err(e) => failure(e),
                })
            }
            "fs.write_text" => {
                let path = path_of(args, 0, "fs.write")?;
                let body = path_of(args, 1, "fs.write")?;
                Ok(match std::fs::write(&path, body) {
                    Ok(()) => ok(""),
                    Err(e) => failure(e),
                })
            }
            "fs.list_dir" => {
                let path = path_of(args, 0, "fs.list")?;
                Ok(match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        let mut names = String::new();
                        for entry in entries {
                            match entry {
                                Ok(e) => {
                                    names.push_str(&e.file_name().to_string_lossy());
                                    names.push('\n');
                                }
                                Err(e) => return Ok(failure(e)),
                            }
                        }
                        ok(names)
                    }
                    Err(e) => failure(e),
                })
            }
            "fs.remove_path" => {
                let path = path_of(args, 0, "fs.remove")?;
                let meta = match std::fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) => return Ok(failure(e)),
                };
                // `remove_dir`, never `remove_dir_all`: deleting a tree is not
                // something a standard library should make a one-liner.
                let result = if meta.is_dir() {
                    std::fs::remove_dir(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                Ok(match result {
                    Ok(()) => ok(""),
                    Err(e) => failure(e),
                })
            }
            "fs.path_kind" => {
                let path = path_of(args, 0, "fs.kind")?;
                // 0 missing, 1 file, 2 directory — matching `fs.Kind`.
                Ok(Value::Int(match std::fs::metadata(&path) {
                    Ok(m) if m.is_dir() => 2,
                    Ok(_) => 1,
                    Err(_) => 0,
                }))
            }
            _ => Err(Trap::NoHostFunction { name: name.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &Value) -> String {
        match v {
            Value::Str(s) => s.to_string(),
            other => panic!("expected a str, got {:?}", other),
        }
    }

    fn arg(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    #[test]
    fn a_missing_file_reports_a_failure() {
        let mut host = NativeHost;
        let answer = host.call("fs.read_text", &[arg("/definitely/not/here")]).unwrap();
        assert!(text(&answer).starts_with(FAILURE));
    }

    #[test]
    fn kind_distinguishes_the_three_cases() {
        let mut host = NativeHost;
        let dir = std::env::temp_dir();
        let file = dir.join("kite-host-test.txt");
        std::fs::write(&file, "hello").unwrap();

        let as_int = |v: Value| match v {
            Value::Int(n) => n,
            other => panic!("expected an int, got {:?}", other),
        };
        assert_eq!(as_int(host.call("fs.path_kind", &[arg(dir.to_str().unwrap())]).unwrap()), 2);
        assert_eq!(as_int(host.call("fs.path_kind", &[arg(file.to_str().unwrap())]).unwrap()), 1);
        assert_eq!(as_int(host.call("fs.path_kind", &[arg("/nope/nope")]).unwrap()), 0);

        let read = host.call("fs.read_text", &[arg(file.to_str().unwrap())]).unwrap();
        assert_eq!(text(&read), "hello");
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn an_unknown_namespace_is_still_a_trap() {
        let mut host = NativeHost;
        assert!(host.call("nope.at_all", &[]).is_err());
    }
}
