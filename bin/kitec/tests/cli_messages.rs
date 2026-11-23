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
        ("E0001", &["block string", "line"][..]),
        ("E0002", &["`;`", "`#`"][..]),
        ("E0003", &["`\\u{", "unknown"][..]),
        ("E0005", &["`*/`", "line comment"][..]),
        ("E0101", &["`{`", "indentation"][..]),
        ("E0110", &["`var`", "every path"][..]),
        ("E0113", &["named argument", "struct"][..]),
        ("E0115", &["closure", "`return`"][..]),
        ("E0119", &["`A → B → A`", "constant"][..]),
        ("E0200", &["as float", "`Option<T>`", "`E0201`"][..]),
        ("E0201", &["overloading", "`as`", "`E0212`"][..]),
        ("E0202", &["truthiness", "`match` guard", "`assert`"][..]),
        ("E0203", &["bare `return`", "(T, error)"][..]),
        ("E0210", &["`_`", "optional", "guard"][..]),
        ("E0303", &["`main`", "closure"][..]),
        ("E0700", &["`E0701`", "`impl`"][..]),
    ] {
        let out = kitec(&["--explain", code]);
        assert!(out.status.success(), "{}", code);
        let said = stdout(&out);
        assert!(said.len() > 700, "{} is {} bytes", code, said.len());
        assert!(said.contains("Example"), "{} has no example:\n{}", code, said);
        for word in mentions {
            assert!(said.contains(word), "{} does not mention {}:\n{}", code, word, said);
        }
    }
}

/// The indented block that follows the line starting with `lead`, unindented.
fn block_after(text: &str, lead: &str) -> Option<String> {
    let mut out = String::new();
    for line in text.lines().skip_while(|l| !l.starts_with(lead)).skip(1) {
        if let Some(code) = line.strip_prefix("    ") {
            out.push_str(code);
            out.push('\n');
        } else if line.trim().is_empty() {
            out.push('\n');
        } else {
            break;
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// An example that is a whole program is checked: it fails with the code it
/// illustrates, and its fix compiles. E0204's fix called a `len` Kite does not
/// have, which a reader copying it would have found out the hard way.
#[test]
fn a_whole_program_example_and_its_fix_are_what_they_say() {
    let dir = std::env::temp_dir().join(format!("kitec-explain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // The codes whose example must produce that code and no other: the ones
    // written as a whole explanation of one situation each.
    let exactly = [
        "E0001", "E0002", "E0003", "E0005", "E0101", "E0110", "E0113", "E0115", "E0119", "E0200",
        "E0201", "E0202", "E0203", "E0210", "E0303", "E0700",
    ];
    let mut codes = vec!["E0100", "E0111", "E0204", "E0520", "E0521"];
    codes.extend(exactly);
    for code in codes {
        let said = stdout(&kitec(&["--explain", code]));
        let example = block_after(&said, "Example").expect("an example");
        let mut checked = 0;
        if example.contains("fn main") {
            let file = dir.join(format!("{}.kite", code));
            std::fs::write(&file, &example).expect("write");
            let out = kitec(&["check", file.to_str().unwrap()]);
            assert!(stderr(&out).contains(&format!("error[{}]", code)), "{}:\n{}", code, stderr(&out));
            if exactly.contains(&code) {
                let others: Vec<String> = stderr(&out)
                    .lines()
                    .filter_map(|l| l.strip_prefix("error[").and_then(|l| l.get(..5)).map(str::to_string))
                    .filter(|c| c != code)
                    .collect();
                assert!(others.is_empty(), "{} example also gives {:?}:\n{}", code, others, stderr(&out));
            }
            checked += 1;
        }
        if let Some(fix) = block_after(&said, "The fix is") {
            // A fix that shows only `main` keeps the example's declarations.
            let only_main =
                ["fn main", "async fn main"].iter().any(|m| fix.trim_start().starts_with(m));
            let fix = match example.find("fn main") {
                Some(at) if only_main => {
                    format!("{}{}", example[..at].trim_end_matches("async "), fix.trim_start())
                }
                _ if fix.contains("fn main") => fix,
                _ => format!("{}\nfn main() {{}}\n", fix),
            };
            let file = dir.join(format!("{}_fix.kite", code));
            std::fs::write(&file, &fix).expect("write");
            let out = kitec(&["check", file.to_str().unwrap()]);
            assert!(out.status.success(), "{} fix:\n{}\n{}", code, fix, stderr(&out));
            checked += 1;
        }
        assert!(checked > 0, "{}: nothing checked", code);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `kitec fix main.kite` leaves the modules it imports alone, and then said
/// their errors were ones it "cannot fix" — after showing a fix for each. They
/// are in another file: the closing line says so and names it.
#[test]
fn fix_names_the_other_file_whose_errors_it_left() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fix_other_file");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("m.kite"),
        "pub fn f() -> int {\n    let k = 1\n    k = 2\n    return k\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.kite"),
        "use m\n\nfn main() {\n    let a = 1\n    a = 3\n    io.print(m.f() + a)\n}\n",
    )
    .unwrap();
    let out = kitec(&["fix", dir.join("main.kite").to_str().unwrap()]);
    assert!(!out.status.success());
    let said = stderr(&out);
    assert!(said.contains("applied 1 fix"), "{}", said);
    assert!(said.contains("in other files has fixes of its own"), "{}", said);
    assert!(said.contains("m.kite"), "{}", said);
    assert!(!said.contains("cannot fix"), "{}", said);
    // Run on the module itself, the fix applies and nothing is left.
    let out = kitec(&["fix", dir.join("m.kite").to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// `kitec build --emit wasm` named `app.wasm`, `app.js` and `index.html` and
/// left out the source map it also wrote.
#[test]
fn build_names_every_file_it_wrote() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("build_names");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("h.kite");
    std::fs::write(&src, "fn main() {\n    io.print(1)\n}\n").unwrap();
    let out_dir = dir.join("out");
    let out = kitec(&["build", src.to_str().unwrap(), "--emit", "wasm", "--out", out_dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stderr(&out);
    let mut written: Vec<String> = std::fs::read_dir(&out_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    written.sort();
    assert!(written.contains(&"app.wasm.map".to_string()), "{:?}", written);
    for name in &written {
        assert!(said.contains(name.as_str()), "{} is written and not named: {}", name, said);
    }
}
