//! What answers `@host("…")` when a program runs under the bytecode VM.
//!
//! The VM declares a [`Host`] trait and calls it by `"namespace.name"`; until
//! now the driver passed `None`, so every `extern` was a trap. This is the
//! native side of `std/fs` — the one namespace a command-line program needs
//! and a browser cannot have.
//!
//! Failures cross as a string with a leading `\u{1}`, which is what `std/fs`
//! unwraps back into an `error`, and every other answer of the four calls that
//! can fail carries a leading `\u{2}`. One `str` is what an `extern` can carry,
//! so the answer is marked rather than paired — and it is marked both ways
//! because a file may begin with any character at all: with only a failure
//! mark, a file starting with U+0001 read back as an error whose message was
//! the rest of the file. `temp_path` cannot fail and is not marked.

use kite_vm::{Host, Trap, Value};
use std::rc::Rc;

/// Marks a returned string as a failure. Must match `fs.FAILURE_MARK`.
const FAILURE: char = '\u{1}';

/// Marks a returned string as a success. Must match `fs.SUCCESS_MARK`.
const SUCCESS: char = '\u{2}';

pub struct NativeHost;

fn failure(message: impl std::fmt::Display) -> Value {
    Value::Str(Rc::from(format!("{}{}", FAILURE, message).as_str()))
}

fn ok(text: impl Into<String>) -> Value {
    Value::Str(Rc::from(format!("{}{}", SUCCESS, text.into()).as_str()))
}

fn path_of(args: &[Value], at: usize, name: &'static str) -> Result<String, Trap> {
    match args.get(at) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(Trap::TypeConfusion { op: name, found: "not a str" }),
    }
}

/// What each function here reads and answers, and the name its traps give
/// it, in the encoding of `kite_mir::Program::extern_sigs`. This is
/// `kite-rt`'s `HOST_FUNCTIONS`, against which the native runtime checks a
/// program's declarations, and the VM checks them against this: the two
/// must say the same, or a declaration that one backend refuses the other
/// runs.
const SIGNATURES: &[(&str, &[u8], &str)] = &[
    ("fs.read_text", b"s:s", "fs.read"),
    ("fs.write_text", b"ss:s", "fs.write"),
    ("fs.list_dir", b"s:s", "fs.list"),
    ("fs.remove_path", b"s:s", "fs.remove"),
    ("fs.path_kind", b"s:i", "fs.kind"),
    ("fs.temp_path", b":s", "fs.temp_path"),
];

impl Host for NativeHost {
    fn signature(&self, name: &str) -> Option<(&'static [u8], &'static str)> {
        SIGNATURES.iter().find(|(n, _, _)| *n == name).map(|(_, sig, op)| (*sig, *op))
    }

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
            "fs.temp_path" => {
                // Whatever this platform calls it: `/tmp` on Unix, and
                // `%TEMP%` — usually under `AppData\Local` — on Windows.
                let dir = std::env::temp_dir();
                let text = dir.to_string_lossy();
                // Without a trailing separator, so a caller joins with one and
                // never gets two.
                Ok(Value::Str(Rc::from(text.trim_end_matches(['/', '\\']))))
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

        // A success is marked as a failure is, so a file's first character is
        // never taken for either.
        let read = host.call("fs.read_text", &[arg(file.to_str().unwrap())]).unwrap();
        assert_eq!(text(&read), format!("{}hello", SUCCESS));
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn an_unknown_namespace_is_still_a_trap() {
        let mut host = NativeHost;
        assert!(host.call("nope.at_all", &[]).is_err());
    }

    /// A program that declares a host function as something the host does
    /// not answer traps before the call, in the native runtime's words. The
    /// VM checked only what it read, so a `path_kind` declared `-> bool`
    /// printed `2` as a bool here while the native runtime trapped, and a
    /// `remove_path` declared to return nothing removed the file here and
    /// not there.
    #[test]
    fn a_wrong_declaration_traps_as_it_does_natively() {
        let file = std::env::temp_dir().join(format!("kite-host-decl-{}.txt", std::process::id()));
        std::fs::write(&file, "keep").unwrap();
        let path = file.to_string_lossy().replace('\\', "/");
        let cases = [
            (
                "extern fn path_kind(path: str) -> str".to_string(),
                "io.print(path_kind(\"/\"))".to_string(),
                "`fs.path_kind` is declared to return str, and the host returns int",
            ),
            (
                "extern fn path_kind(path: str) -> bool".to_string(),
                "io.print(path_kind(\"/\"))".to_string(),
                "`fs.path_kind` is declared to return bool, and the host returns int",
            ),
            (
                "extern fn remove_path(path: str)".to_string(),
                format!("remove_path(\"{}\")", path),
                "`fs.remove_path` is declared to return (), and the host returns str",
            ),
            (
                "extern fn read_text(path: int) -> str".to_string(),
                "io.print(read_text(1))".to_string(),
                "`fs.read` received a `not a str`",
            ),
        ];
        for (declaration, call, message) in cases {
            let src = format!(
                "@host(\"fs\")\n{}\n\nfn main() {{\n  io.print(\"start\")\n  {}\n  io.print(\"after\")\n}}\n",
                declaration, call
            );
            let c = crate::compile("decl.kite", &src, crate::Emit::Check);
            assert!(!c.failed(), "{}", c.render_diagnostics());
            let mut out = Vec::new();
            let trap = c.run(&mut out).expect_err("a wrong declaration traps");
            assert_eq!(trap.to_string(), message, "{}", declaration);
            assert_eq!(String::from_utf8(out).unwrap(), "start\n", "{}", declaration);
        }
        assert!(file.exists(), "the call ran before its declaration was refused");
        std::fs::remove_file(&file).unwrap();
    }
}
