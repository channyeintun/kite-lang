//! `kitec` whose reader has gone: `kitec --explain E0302 | head`.
//!
//! Every command that writes its own answer to standard output panicked with
//! "failed printing to stdout: Broken pipe" when the reader closed early,
//! because `print!` panics on a write that fails. A panic from the compiler
//! reads as a bug in it; a reader that stopped reading has had what it wanted,
//! and the command ends cleanly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const KITEC: &str = env!("CARGO_BIN_EXE_kitec");

/// Run `kitec`, closing the read end of its standard output before reading a
/// byte of it.
fn closed_early(args: &[&str]) -> Output {
    let mut child = Command::new(KITEC)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kitec starts");
    drop(child.stdout.take());
    child.wait_with_output().expect("kitec finishes")
}

fn assert_clean(args: &[&str], output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "kitec {:?} panicked:\n{}", args, stderr);
    assert_ne!(output.status.code(), Some(101), "kitec {:?}:\n{}", args, stderr);
    assert!(output.status.success(), "kitec {:?} failed ({}):\n{}", args, output.status, stderr);
}

fn hello() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kitec-stdout-closed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a work directory");
    let file = dir.join("hello.kite");
    std::fs::write(&file, "/// Says hello.\npub fn main() {\n    io.print(\"hello\")\n}\n")
        .expect("write the program");
    file
}

/// Each path that prints its own answer. A short answer may be written before
/// the reader has closed, which is a clean run too; the test below is the one
/// that cannot be.
#[test]
fn a_closed_stdout_is_not_a_panic() {
    let file = hello();
    let file = file.to_str().expect("a UTF-8 path");
    let commands: &[&[&str]] = &[
        &["--help"],
        &["--version"],
        &["--explain", "E0302"],
        &["doc", file],
        &["check", file, "--emit", "mir"],
        &["run", file, "--emit", "kbc"],
        &["build", file, "--emit", "hir"],
    ];
    for args in commands {
        assert_clean(args, &closed_early(args));
    }
    let _ = std::fs::remove_dir_all(Path::new(file).parent().expect("its directory"));
}

/// An answer larger than any pipe's buffer blocks until the reader closes, and
/// then fails to write, however the two processes are scheduled.
#[test]
fn a_long_answer_to_a_closed_stdout_is_not_a_panic() {
    let big = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/html.kite");
    let big = big.to_str().expect("a UTF-8 path");
    let args = ["check", big, "--emit", "ast"];
    let full = Command::new(KITEC).args(args).output().expect("kitec runs");
    assert!(full.status.success(), "{}", String::from_utf8_lossy(&full.stderr));
    assert!(full.stdout.len() > 256 * 1024, "only {} bytes", full.stdout.len());
    assert_clean(&args, &closed_early(&args));
}
