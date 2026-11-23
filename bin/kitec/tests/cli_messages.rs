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
        ("E0006", &["64 levels", "ceiling", "generated"][..]),
        ("E0114", &["`var`", "field", "`var self`", "not a place"][..]),
        ("E0116", &["warning", "`return`", "binding"][..]),
        ("E0205", &["extension methods", "`dyn Trait`", "`impl`"][..]),
        ("E0206", &["`self`", "generic", "bound"][..]),
        ("E0207", &["`Display`", "slice", "`error`"][..]),
        ("E0208", &["type argument", "bound", "`impl`"][..]),
        ("E0209", &["turbofish", "conflicting", "`Stack.empty()`"][..]),
        ("E0212", &["`bool as int`", "`parse_int`", "warning"][..]),
        ("E0302", &["`check`", "`_ = f()`", "`defer`", "`let (v, _) = f()`"][..]),
        ("E0405", &["quoted string", "`verison`", "UTF-8"][..]),
        ("E0600", &["crypto.equal", "timing", "warning"][..]),
        ("E0701", &["`Display`", "`Eq`", "by hand"][..]),
        ("E0702", &["`dyn Trait`", "generic", "by hand"][..]),
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

/// The files an example shows: a line `// name.kite` starts the file of that
/// name, and the first is the one checked. An example with no such line is one
/// file.
fn files_of(block: &str) -> Vec<(String, String)> {
    let marker = |l: &str| -> Option<String> {
        let name = l.strip_prefix("// ")?;
        let ok = name.ends_with(".kite")
            && name.chars().all(|c| c.is_ascii_alphanumeric() || "_/.-".contains(c));
        ok.then(|| name.to_string())
    };
    let mut files: Vec<(String, String)> = Vec::new();
    for line in block.lines() {
        if let Some(name) = marker(line) {
            files.push((name, String::new()));
        } else if let Some((_, body)) = files.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if files.is_empty() {
        files.push(("main.kite".to_string(), block.to_string()));
    }
    files
}

fn write_all(dir: &std::path::Path, files: &[(String, String)]) {
    for (name, body) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
        std::fs::write(path, body.trim_start_matches('\n')).expect("write");
    }
}

/// The codes no program can raise, or raises only at a size no text can show.
/// Each is named with why; its text says so, and shows the layout of the
/// report instead of a program.
const NO_EXAMPLE: [(&str, &str); 2] = [
    ("E0900", "the compiler's own invalid output: no source provokes it"),
    ("E0901", "the compiler's own internal error: no source provokes it"),
];

/// The programs of the two codes raised only by size, generated here: what
/// the text shows has `…` where the thousands of lines are.
fn generated(code: &str, fixed: bool) -> Option<String> {
    match code {
        "E0102" => {
            let depth = if fixed { 200 } else { 300 };
            Some(format!(
                "fn main() {{\n    let x = {}1{}\n    io.print(x)\n}}\n",
                "(".repeat(depth),
                ")".repeat(depth)
            ))
        }
        "E0902" => {
            let n = if fixed { 1000 } else { 70000 };
            let mut text = String::from("fn main() {\n");
            for i in 0..n {
                text.push_str(&format!("    let a{} = {}\n", i, i));
            }
            text.push_str("    io.print(a1)\n}\n");
            Some(text)
        }
        _ => None,
    }
}

/// Every diagnostic code's `--explain` text has an example that raises that
/// code and a fix that compiles, and the two are run from the texts
/// themselves, so they cannot drift from the compiler. E0204's fix called a
/// `len` Kite does not have; E0521's fix left out the `work` it called; E0117's
/// blocks were fragments; sixteen codes had prose only.
#[test]
fn every_code_has_an_example_that_raises_it_and_a_fix_that_compiles() {
    let dir = std::env::temp_dir().join(format!("kitec-explain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let listed = stderr(&kitec(&["--explain", "E9999"]));
    let list = listed.lines().find_map(|l| l.strip_prefix("note: known codes: ")).expect("a list");
    // Codes whose example is allowed to raise another code beside its own,
    // because the situation it shows cannot be built without it.
    let also: [&str; 0] = [];
    let mut walked = 0;
    for code in list.split(", ") {
        let said = stdout(&kitec(&["--explain", code]));
        if let Some((_, why)) = NO_EXAMPLE.iter().find(|(c, _)| *c == code) {
            assert!(said.contains("not an error a program can provoke"), "{}: {}", code, why);
            walked += 1;
            continue;
        }
        assert!(said.contains("Example"), "{} has no example:\n{}", code, said);
        assert!(said.contains("The fix"), "{} has no fix:\n{}", code, said);
        let at = dir.join(code);
        let fixed_at = dir.join(format!("{}_fix", code));
        let example_block = block_after(&said, "Example").unwrap_or_else(|| panic!("{}: no block", code));
        let fix_block = block_after(&said, "The fix").unwrap_or_else(|| panic!("{}: no fix block", code));
        let manifest = example_block.trim_start().starts_with("[package]");
        let (entry, fixed_entry);
        if manifest {
            // A manifest is the example: what is checked is whether it reads,
            // in a package whose program does nothing.
            for (path, toml) in [(&at, &example_block), (&fixed_at, &fix_block)] {
                std::fs::create_dir_all(path).expect("package dir");
                std::fs::write(path.join("kite.toml"), toml).expect("write");
                std::fs::write(path.join("main.kite"), "fn main() {\n    io.print(1)\n}\n")
                    .expect("write");
            }
            entry = at.join("main.kite");
            fixed_entry = fixed_at.join("main.kite");
        } else if let Some(program) = generated(code, false) {
            std::fs::create_dir_all(&at).expect("dir");
            std::fs::create_dir_all(&fixed_at).expect("dir");
            std::fs::write(at.join("main.kite"), program).expect("write");
            std::fs::write(fixed_at.join("main.kite"), generated(code, true).unwrap()).expect("write");
            entry = at.join("main.kite");
            fixed_entry = fixed_at.join("main.kite");
        } else {
            let files = files_of(&example_block);
            write_all(&at, &files);
            entry = at.join(&files[0].0);
            // A fix that names files replaces those of the example; one that
            // is a program on its own replaces the entry; one that shows only
            // `main` keeps the example's declarations.
            let fix_files = files_of(&fix_block);
            let named = fix_block.lines().any(|l| l.starts_with("// ") && l.ends_with(".kite"));
            write_all(&fixed_at, &files);
            if named {
                write_all(&fixed_at, &fix_files);
            } else {
                let only_main =
                    ["fn main", "async fn main"].iter().any(|m| fix_block.trim_start().starts_with(m));
                let text = match example_block.find("fn main") {
                    Some(i) if only_main => format!(
                        "{}{}",
                        example_block[..i].trim_end_matches("async "),
                        fix_block.trim_start()
                    ),
                    _ if fix_block.contains("fn main") => fix_block.clone(),
                    _ => format!("{}\nfn main() {{}}\n", fix_block),
                };
                std::fs::write(fixed_at.join(&files[0].0), text).expect("write");
            }
            fixed_entry = fixed_at.join(&files[0].0);
        }
        let out = kitec(&["check", entry.to_str().unwrap()]);
        // A warning is the code's own, too: E0116 and E0600 do not stop a build.
        let said_err = stderr(&out);
        assert!(
            said_err.contains(&format!("error[{}]", code)) || said_err.contains(&format!("warning[{}]", code)),
            "{} example:\n{}",
            code,
            said_err
        );
        if !also.contains(&code) {
            let others: Vec<String> = said_err
                .lines()
                .filter_map(|l| l.strip_prefix("error[").and_then(|l| l.get(..5)).map(str::to_string))
                .filter(|c| c != code)
                .collect();
            assert!(others.is_empty(), "{} example also gives {:?}:\n{}", code, others, said_err);
        }
        let out = kitec(&["check", fixed_entry.to_str().unwrap()]);
        // A warning's fix compiles with the warning still there: what is
        // checked is that the code is gone.
        assert!(out.status.success(), "{} fix:\n{}", code, stderr(&out));
        assert!(!stderr(&out).contains(&format!("[{}]", code)), "{} fix:\n{}", code, stderr(&out));
        walked += 1;
    }
    assert!(walked > 50 && walked == list.split(", ").count());
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
