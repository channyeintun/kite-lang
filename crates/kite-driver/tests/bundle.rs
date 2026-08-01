//! `kitec bundle` — one file that needs nothing installed.
//!
//! The test drives the real binary, because the thing being tested is what
//! happens when an executable is copied and appended to: that it still runs,
//! that it runs the *program* rather than the compiler, and that it starts
//! fast enough to be worth doing.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

fn kitec() -> PathBuf {
    // `cargo test` puts the test binary next to the compiler it built.
    let mut path = std::env::current_exe().expect("this test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "kitec.exe" } else { "kitec" })
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-bundle-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    dir
}

#[test]
fn a_bundle_runs_the_program_it_carries() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary at {}", compiler.display());
        return;
    }
    let dir = work_dir("runs");
    let source = dir.join("greet.kite");
    std::fs::write(
        &source,
        "fn main() {\n    io.print(\"from a bundle\")\n    io.print(6 * 7)\n}\n",
    )
    .expect("write source");

    let built = Command::new(&compiler)
        .args(["bundle", source.to_str().unwrap(), "--out", dir.to_str().unwrap()])
        .output()
        .expect("kitec runs");
    assert!(
        built.status.success(),
        "bundling failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let program = dir.join(if cfg!(windows) { "greet.exe" } else { "greet" });
    assert!(program.exists(), "no bundle at {}", program.display());

    let started = Instant::now();
    let out = Command::new(&program).output().expect("the bundle runs");
    let elapsed = started.elapsed();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from a bundle\n42\n");

    // The claim the roadmap makes about a native binary is that it starts
    // quickly. Compiling at startup is the whole cost, and it is small — a
    // second here would mean something is badly wrong rather than slightly.
    assert!(
        elapsed.as_millis() < 1000,
        "a bundle took {}ms to start and finish",
        elapsed.as_millis()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A bundle is a program, not a compiler: its arguments are its own.
#[test]
fn a_bundle_is_not_a_compiler() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary");
        return;
    }
    let dir = work_dir("args");
    let source = dir.join("quiet.kite");
    std::fs::write(&source, "fn main() {\n    io.print(\"ran\")\n}\n").expect("write");
    Command::new(&compiler)
        .args(["bundle", source.to_str().unwrap(), "--out", dir.to_str().unwrap()])
        .output()
        .expect("kitec runs");

    let program = dir.join(if cfg!(windows) { "quiet.exe" } else { "quiet" });
    let out = Command::new(&program).arg("--help").output().expect("runs");
    // `--help` is the compiler's flag, and a bundle is not the compiler.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program that does not compile is not packaged: a bundle that failed at
/// startup would move a compile error onto the user's machine, which is the
/// arrangement this exists to avoid.
#[test]
fn a_broken_program_is_not_bundled() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary");
        return;
    }
    let dir = work_dir("broken");
    let source = dir.join("broken.kite");
    std::fs::write(&source, "fn main() {\n    let x: int = \"s\"\n}\n").expect("write");
    let built = Command::new(&compiler)
        .args(["bundle", source.to_str().unwrap(), "--out", dir.to_str().unwrap()])
        .output()
        .expect("kitec runs");
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("E0200"));
    assert!(!dir.join("broken").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
