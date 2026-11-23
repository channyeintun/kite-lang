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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// `kitec --explain E0117 E0118` explained the first and silently ignored the
/// second.
#[test]
fn explain_takes_one_code() {
    let out = kitec(&["--explain", "E0117", "E0118"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("takes one diagnostic code"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
}

/// The list of known codes was in the order the codes were written down:
/// E0006 before E0005, E0405 before E0402.
#[test]
fn the_known_codes_are_listed_in_order() {
    let said = stderr(&kitec(&["--explain", "E9999"]));
    let list = said.lines().find_map(|l| l.strip_prefix("note: known codes: ")).expect("a list");
    let codes: Vec<&str> = list.split(", ").collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    assert_eq!(codes, sorted);
    assert!(codes.len() > 50);
}

/// The commonest codes were explained in a sentence that restated the
/// message. Each now says what the situations are and shows a fix; the check
/// here is that none has gone back to being a line, and that each names what
/// it covers.
#[test]
fn the_commonest_codes_are_explained_in_full() {
    for (code, mentions) in [
        ("E0100", &["comma", "`if`", "interpolation"][..]),
        ("E0111", &["scope", "variant", "label"][..]),
        ("E0204", &["infer", "dyn Display", "annotat"][..]),
        ("E0400", &["`std/", ".kite/vendor", "directory"][..]),
        ("E0401", &["`pub`", "variant", "field"][..]),
        ("E0402", &["chain", "third module"][..]),
        ("E0520", &["`var`", "closure", "`JsValue`"][..]),
        ("E0521", &["closure", "async"][..]),
    ] {
        let out = kitec(&["--explain", code]);
        assert!(out.status.success(), "{}", code);
        let said = stdout(&out);
        assert!(said.len() > 800, "{} is {} bytes", code, said.len());
        assert!(said.contains("Example"), "{} has no example:\n{}", code, said);
        for word in mentions {
            assert!(said.contains(word), "{} does not mention {}:\n{}", code, word, said);
        }
    }
}
