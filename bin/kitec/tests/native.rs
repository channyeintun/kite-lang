//! `kitec` itself, on the native backend: the binary a person runs, rather
//! than the library functions behind it.
//!
//! Two things only the binary can show. `kitec run --native` must behave like
//! a program — output as it is printed, in order with standard error, and none
//! of it lost when the program dies or never ends — which a harness that
//! collects output cannot see by construction. And `kitec build --emit native`
//! must produce an executable from nothing but the binary and a system `cc`,
//! which is the difference between the object-file path existing and it being
//! usable.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const KITEC: &str = env!("CARGO_BIN_EXE_kitec");

/// The native backend refuses Windows; see `kite_codegen_clif::supported_here`.
fn native_here() -> bool {
    if cfg!(all(windows, target_arch = "x86_64")) {
        eprintln!("skipping: the native backend does not run on Windows");
        return false;
    }
    true
}

fn cc_here() -> bool {
    let found = Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        eprintln!("skipping: `cc` is not on PATH");
    }
    found
}

/// A directory of this test's own, holding one Kite file.
fn program(test: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kitec-native-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create a work directory");
    let file = dir.join(format!("{}.kite", test));
    std::fs::write(&file, src).expect("write the program");
    (dir, file)
}

fn kitec(args: &[&str], file: &Path) -> Output {
    Command::new(KITEC).args(args).arg(file).output().expect("kitec runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Something the collector has to work for: a live list that outlasts far
/// more garbage than the nursery holds, structs, strings and a map.
const CHURN: &str = "\
struct P {
    x: int
    name: str
}

fn main() {
    var keep: [P] = []
    var seen: {str: int} = {}
    for i in 0..400 {
        keep.push(P{ x: i, name: \"p\\(i)\" })
        var scratch: [str] = []
        for j in 0..20 {
            scratch.push(\"junk \\(j)\")
        }
        seen[\"k\\(i % 17)\"] = scratch.len() + i
    }
    var sum = 0
    for p in keep {
        sum = sum + p.x
    }
    io.print(sum)
    io.print(keep[399].name)
    io.print(seen.len())
}
";

#[test]
fn an_executable_links_from_the_runtime_kitec_carries() {
    if !native_here() || !cc_here() {
        return;
    }
    let (dir, file) = program("linked", CHURN);
    let vm = kitec(&["run"], &file);
    assert!(vm.status.success(), "the VM run failed: {}", text(&vm.stderr));

    // No KITE_RT_LIB: the point is that nothing needs setting.
    let out = dir.join("out");
    let built = Command::new(KITEC)
        .args(["build", "--emit", "native", "--out"])
        .arg(&out)
        .arg(&file)
        .env_remove("KITE_RT_LIB")
        .output()
        .expect("kitec runs");
    assert!(built.status.success(), "the build failed: {}", text(&built.stderr));
    let exe = out.join("linked");
    assert!(
        exe.exists(),
        "no executable was linked; kitec said:\n{}",
        text(&built.stderr)
    );

    // Twice: once as built, and once with a nursery small enough that the
    // collector in the linked runtime has to run many times.
    for nursery in [None, Some("4096")] {
        let mut run = Command::new(&exe);
        if let Some(n) = nursery {
            run.env("KITE_NURSERY_BYTES", n);
        }
        let ran = run.output().expect("the executable runs");
        assert!(ran.status.success(), "the executable failed: {}", text(&ran.stderr));
        assert_eq!(text(&ran.stdout), text(&vm.stdout), "nursery {:?}", nursery);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a program prints reaches standard output when it prints it, so a
/// line on standard error written between two lines on standard output lands
/// between them. Collected output came out after everything on standard error.
#[cfg(unix)]
#[test]
fn run_native_interleaves_with_standard_error() {
    if !native_here() {
        return;
    }
    let (dir, file) = program(
        "interleave",
        "fn main() {\n    io.print(\"one\")\n    io.error(\"two\")\n    io.print(\"three\")\n}\n",
    );
    // One pipe for both streams, so the order is the order of the writes.
    let merged = Command::new("sh")
        .arg("-c")
        .arg("\"$0\" run --native \"$1\" 2>&1")
        .arg(KITEC)
        .arg(&file)
        .output()
        .expect("sh runs");
    assert!(merged.status.success(), "{}", text(&merged.stdout));
    assert_eq!(text(&merged.stdout), "one\ntwo\nthree\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program that dies still said what it said first. Native recursion is
/// bounded by the machine stack, so this one ends in a stack overflow — and
/// under collected output, the two lines before it were never seen.
#[test]
fn output_before_a_crash_is_not_lost() {
    if !native_here() {
        return;
    }
    let (dir, file) = program(
        "crash",
        "fn down(n: int) -> int {\n    if n == 0 {\n        return 0\n    }\n\
         \x20   return 1 + down(n - 1)\n}\n\n\
         fn main() {\n    io.print(\"before\")\n    io.print(down(1000))\n\
         \x20   io.print(down(100000000))\n}\n",
    );
    let ran = kitec(&["run", "--native"], &file);
    assert!(!ran.status.success(), "a hundred million frames fit on the stack?");
    assert_eq!(text(&ran.stdout), "before\n1000\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program that never ends is still heard from. Collected, its output grew
/// in memory and never appeared at all.
#[test]
fn a_program_that_never_ends_prints_as_it_goes() {
    if !native_here() {
        return;
    }
    let (dir, file) = program(
        "forever",
        "fn main() {\n    var i = 0\n    for {\n        io.print(\"tick \\(i)\")\n\
         \x20       i = i + 1\n    }\n}\n",
    );
    let mut child = Command::new(KITEC)
        .args(["run", "--native"])
        .arg(&file)
        .stdout(Stdio::piped())
        .spawn()
        .expect("kitec runs");
    let stdout = child.stdout.take().expect("piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let first = rx.recv_timeout(Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(first.expect("no line within a minute").as_str(), "tick 0\n");
}

/// `std/fs` natively: a host function answered by the runtime, and a
/// declaration of one that does not match what the host implements refused in
/// the VM's own words — the native alternative being an integer read as the
/// address of a string.
#[test]
fn a_host_declaration_that_lies_is_refused_like_the_vm_refuses_it() {
    if !native_here() {
        return;
    }
    let (dir, file) = program(
        "lie",
        "@host(\"fs\")\nextern fn read_text(path: int) -> str\n\n\
         fn main() {\n    io.print(\"before\")\n    io.print(read_text(42))\n}\n",
    );
    let vm = kitec(&["run"], &file);
    let native = kitec(&["run", "--native"], &file);
    assert_eq!(vm.status.code(), Some(1));
    assert_eq!(native.status.code(), Some(1));
    assert_eq!(text(&native.stdout), text(&vm.stdout));
    assert_eq!(text(&native.stderr), text(&vm.stderr));
    assert!(text(&native.stderr).contains("`fs.read` received a `not a str`"));
    let _ = std::fs::remove_dir_all(&dir);
}
