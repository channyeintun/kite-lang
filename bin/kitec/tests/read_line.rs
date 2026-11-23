//! `io.read_line()` on a line that is not UTF-8.
//!
//! Both runtimes read standard input as text, so a line with a bad byte failed
//! to read and came back as `""`, the value that means end of input: the usual
//! `if line == "" { break }` loop stopped early, and the lines after it were
//! never seen. A `str` holds Unicode scalar values, so the bad bytes become
//! U+FFFD and the line is read like any other. The Wasm backend reads no
//! standard input, so the VM and native are the two that must agree.

use std::io::Write;
use std::process::{Command, Stdio};

const KITEC: &str = env!("CARGO_BIN_EXE_kitec");

fn run_with_stdin(flags: &[&str], program: &std::path::Path, input: &[u8]) -> String {
    let mut child = Command::new(KITEC)
        .arg("run")
        .args(flags)
        .arg(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kitec starts");
    child.stdin.take().expect("a stdin pipe").write_all(input).expect("write stdin");
    let out = child.wait_with_output().expect("kitec finishes");
    assert!(out.status.success(), "kitec run {:?}: {}", flags, String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("UTF-8 output")
}

#[test]
fn an_invalid_utf8_line_reads_lossily_not_as_end_of_input() {
    let dir = std::env::temp_dir().join(format!("kitec-read-line-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a work directory");
    let program = dir.join("lines.kite");
    std::fs::write(
        &program,
        "fn main() {\n    for i in 0..4 {\n        let l = io.read_line()\n        io.print(\"[\\(l)] \\(l.len())\")\n    }\n}\n",
    )
    .expect("write the program");
    let input = b"one\ntw\xffo\r\nthree\n";
    let want = "[one] 3\n[tw\u{fffd}o] 4\n[three] 5\n[] 0\n";
    for flags in [&[][..], &["--native"][..]] {
        assert_eq!(run_with_stdin(flags, &program, input), want, "kitec run {:?}", flags);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
