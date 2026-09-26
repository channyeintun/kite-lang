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

    // The very first run of a freshly written executable pays a one-time
    // platform cost — on macOS, Gatekeeper assesses an unsigned binary for
    // seconds before letting it start. That is the operating system's price,
    // not the bundle's, so the timing below measures the second run: the one
    // whose whole cost is compiling at startup, which is the claim under test.
    let warm = Command::new(&program).output().expect("the bundle runs");
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));

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

fn write(path: PathBuf, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create");
    }
    std::fs::write(path, text).expect("write");
}

/// A bundle carries every file its build read, not only the one it was
/// pointed at.
///
/// It carried the entry alone, so a program with a `use` bundled cleanly —
/// the check ran beside the sources — and then failed on the machine it was
/// for with `cannot find module`. This one has a sibling module, a package
/// dependency declared in a manifest above `src/`, and a module inside that
/// dependency; the sources are deleted before the bundle runs, from somewhere
/// else, so nothing on disk can be answering for it.
#[test]
fn a_bundle_carries_the_modules_its_program_uses() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary");
        return;
    }
    let dir = work_dir("modules");
    write(dir.join("md/kite.toml"), "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    write(dir.join("md/md.kite"), "use util\n\npub fn render() -> str {\n    return util.me()\n}\n");
    write(dir.join("md/util.kite"), "pub fn me() -> str {\n    return \"md-util\"\n}\n");
    write(
        dir.join("app/kite.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmd = { path = \"../md\" }\n",
    );
    write(dir.join("app/src/util.kite"), "pub fn me() -> str {\n    return \"app-util\"\n}\n");
    let source = dir.join("app/src/main.kite");
    write(
        source.clone(),
        "use md\nuse util\n\nfn main() {\n    io.print(md.render())\n    io.print(util.me())\n}\n",
    );
    let out = dir.join("out");
    let built = Command::new(&compiler)
        .args(["bundle", source.to_str().unwrap(), "--out", out.to_str().unwrap()])
        .output()
        .expect("kitec runs");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));

    let _ = std::fs::remove_dir_all(dir.join("app"));
    let _ = std::fs::remove_dir_all(dir.join("md"));
    let program = out.join(if cfg!(windows) { "main.exe" } else { "main" });
    let ran = Command::new(&program).current_dir(&out).output().expect("the bundle runs");
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "md-util\napp-util\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bundle is built the way it was asked for. It always compiled for
/// release, so a debug bundle silently dropped every `assert`.
#[test]
fn a_bundle_keeps_its_build_mode() {
    let compiler = kitec();
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary");
        return;
    }
    let dir = work_dir("mode");
    let source = dir.join("claim.kite");
    std::fs::write(
        &source,
        "fn main() {\n    assert(1 == 2, \"debug assert fired\")\n    io.print(\"skipped\")\n}\n",
    )
    .expect("write");
    for (flag, out, fires) in [(None, "debug", true), (Some("--release"), "release", false)] {
        let mut args = vec!["bundle", source.to_str().unwrap(), "--out"];
        let target = dir.join(out);
        args.push(target.to_str().unwrap());
        args.extend(flag);
        let built = Command::new(&compiler).args(&args).output().expect("kitec runs");
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let program = target.join(if cfg!(windows) { "claim.exe" } else { "claim" });
        let ran = Command::new(&program).output().expect("the bundle runs");
        assert_eq!(!ran.status.success(), fires, "{} bundle", out);
        if fires {
            assert!(String::from_utf8_lossy(&ran.stderr).contains("debug assert fired"));
        } else {
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "skipped\n");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
