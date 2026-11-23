//! What `kitec` says when it is spoken to wrongly.

use std::process::{Command, Output};

const KITEC: &str = env!("CARGO_BIN_EXE_kitec");

fn kitec(args: &[&str]) -> Output {
    Command::new(KITEC).args(args).output().expect("kitec runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// `kitec frobnicate h.kite` said "unexpected argument `h.kite`" and
/// `kitec frobnicate` said "cannot read ./frobnicate": a word in the command's
/// place was taken for a file.
#[test]
fn an_unknown_command_is_called_one() {
    for args in [&["frobnicate", "h.kite"][..], &["frobnicate"][..]] {
        let out = kitec(args);
        assert!(!out.status.success());
        let said = stderr(&out);
        assert!(said.contains("unknown command `frobnicate`"), "{:?}: {}", args, said);
        assert!(said.contains("run, check, build"), "{:?}: {}", args, said);
    }
}

/// A path is still a path: with an extension, with a directory, or there.
#[test]
fn a_missing_file_is_still_a_missing_file() {
    for name in ["nowhere.kite", "dir/nowhere"] {
        let said = stderr(&kitec(&[name]));
        assert!(said.contains("cannot read"), "{}: {}", name, said);
        assert!(!said.contains("unknown command"), "{}: {}", name, said);
    }
}
