//! The native runtime, built into this binary.
//!
//! `kitec build --emit native` links the object file Cranelift wrote against
//! `kite-rt`'s `staticlib`. Cargo does build that archive — but as a
//! hash-named file under `deps/`, and next to `kitec` only when `kite-rt`
//! itself was one of the packages asked for. `cargo build -p kitec`, `cargo
//! install` and the release workflow all left nothing to find, so `--emit
//! native` wrote an object file and gave up. And an archive found lying on
//! disk may be from another build than the compiler looking for it, while the
//! two share an ABI that is a list of symbol names and argument orders with
//! nothing checking them against each other.
//!
//! So the runtime is compiled here, from the same source the JIT links, for
//! the target this `kitec` is being built for, and `main.rs` embeds it. Every
//! way of getting `kitec` — a release archive, npm, Homebrew, `cargo install`
//! — then carries the one runtime its code generator was written against.
//!
//! `kite-rt` has one dependency, `kite-float`, which has none — so this is two
//! `rustc` invocations rather than a nested Cargo build: the float crate as an
//! rlib, then the runtime against it. The runtime is built with fat LTO, which
//! reaches through the rlib, keeps only what the runtime's exported functions
//! reach, and makes the archive about a third of the size Cargo's is.
//!
//! **Linux with musl.** The released Linux `kitec` is built for musl so that it
//! runs anywhere, but the `cc` a user links with is almost always glibc's, and a
//! standard library compiled for musl calls glibc's functions by musl's names —
//! `strerror_r`, for one, is a different function in each, and it is how an
//! `io::Error` becomes the message `std/fs` returns. So a musl `kitec` built on
//! a glibc machine embeds the runtime for the matching glibc target; built on a
//! musl machine, where `cc` is musl's too, it embeds the musl one.
//! `KITE_RT_TARGET` overrides the choice.
//!
//! Nothing is embedded on Windows, where the native backend refuses to run,
//! nor when `KITE_RT_EMBED=0`; `kitec` then looks for `KITE_RT_LIB`, or an
//! archive next to itself. When the build fails — a target whose standard
//! library is not installed, say — that is a warning, unless
//! `KITE_RT_EMBED=require`, which the release workflow sets so that a release
//! can never ship a `kitec` whose `--emit native` cannot link.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The separator Cargo uses in `CARGO_ENCODED_RUSTFLAGS`.
const SEP: char = '\x1f';

fn main() {
    println!("cargo:rerun-if-env-changed=KITE_RT_EMBED");
    println!("cargo:rerun-if-env-changed=KITE_RT_TARGET");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let out = PathBuf::from(env::var("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let archive = out.join("libkite_rt.a");
    let link_args = out.join("kite_rt_link_args.txt");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets it"));
    let source = manifest.join("../../crates/kite-rt/src/lib.rs");
    let float = manifest.join("../../crates/kite-float/src/lib.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", float.display());

    let mode = env::var("KITE_RT_EMBED").unwrap_or_default();
    let required = mode == "require";
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Empty files are what `main.rs` reads as "nothing embedded".
    if mode == "0" || os == "windows" {
        write(&archive, b"");
        write(&link_args, b"");
        return;
    }
    if !source.exists() || !float.exists() {
        if required {
            panic!("KITE_RT_EMBED=require, and `{}` is not here", source.display());
        }
        write(&archive, b"");
        write(&link_args, b"");
        return;
    }

    match build(&source, &float, &out, &archive) {
        Ok(args) => write(&link_args, args.as_bytes()),
        Err(why) => {
            if required {
                panic!("KITE_RT_EMBED=require, and building the runtime failed: {}", why);
            }
            println!(
                "cargo:warning=kitec will not embed the native runtime, so `--emit native` \
                 will need KITE_RT_LIB: {}",
                why.lines().next().unwrap_or("")
            );
            write(&archive, b"");
            write(&link_args, b"");
        }
    }
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes)
        .unwrap_or_else(|e| panic!("cannot write `{}`: {}", path.display(), e));
}

/// A `rustc` for one of the runtime's crates, with everything the two share:
/// the edition, the target, the build's own flags, and frame pointers.
fn rustc_for(rustc: &str, target: &str) -> Command {
    let mut cmd = Command::new(rustc);
    cmd.args(["--edition", &workspace_edition()])
        .args(["--target", target])
        .args(["-C", "opt-level=3", "-C", "codegen-units=1"])
        .args(["-C", "debuginfo=0"])
        .args(["--cap-lints", "allow"]);
    // The same flags the rest of this build gets — a path remapping, a
    // target CPU — so the runtime is built the way `kite-rt` is.
    if let Ok(flags) = env::var("CARGO_ENCODED_RUSTFLAGS") {
        cmd.args(flags.split(SEP).filter(|f| !f.is_empty()));
    }
    // Last, and unconditionally: the collector walks frame pointers through
    // this code, and rustc takes the strongest request it is given, so no
    // flag above can turn this off. See `crates/kite-rt/build.rs`.
    cmd.args(["-C", "force-frame-pointers=yes"]);
    cmd
}

/// Compile the runtime and return the linker arguments its archive needs.
fn build(source: &Path, float: &Path, out: &Path, archive: &Path) -> Result<String, String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let target = runtime_target();

    // `kite-float` first, as the rlib the runtime is compiled against. Its
    // bitcode is kept, so the fat LTO below reaches into it.
    let float_lib = out.join("libkite_float.rlib");
    let mut cmd = rustc_for(&rustc, &target);
    cmd.arg(float)
        .args(["--crate-name", "kite_float", "--crate-type", "rlib"])
        .args(["-C", "embed-bitcode=yes"])
        .arg("-o")
        .arg(&float_lib);
    let ran = cmd.output().map_err(|e| format!("cannot run `{}`: {}", rustc, e))?;
    if !ran.status.success() {
        let stderr = String::from_utf8_lossy(&ran.stderr);
        let first = stderr.lines().find(|l| l.starts_with("error")).unwrap_or("");
        return Err(format!("`rustc --target {}` failed on kite-float: {}\n{}", target, first, stderr));
    }

    let mut cmd = rustc_for(&rustc, &target);
    cmd.arg(source)
        .args(["--crate-name", "kite_rt", "--crate-type", "staticlib"])
        .args(["-C", "lto=fat"])
        .arg("--extern")
        .arg(format!("kite_float={}", float_lib.display()))
        .args(["--print", "native-static-libs"])
        .arg("-o")
        .arg(archive);

    let ran = cmd.output().map_err(|e| format!("cannot run `{}`: {}", rustc, e))?;
    let stderr = String::from_utf8_lossy(&ran.stderr);
    if !ran.status.success() {
        // The first error first, so a one-line warning still says why.
        let first = stderr.lines().find(|l| l.starts_with("error")).unwrap_or("");
        return Err(format!("`rustc --target {}` failed: {}\n{}", target, first, stderr));
    }
    // What the archive needs from the system at link time, which rustc says
    // rather than leaving it to be guessed.
    let libs = stderr
        .lines()
        .find_map(|l| l.split_once("native-static-libs:").map(|(_, libs)| libs.trim()))
        .unwrap_or("")
        .to_string();
    Ok(libs)
}

/// The target to build the runtime for. See the module comment for why a
/// musl `kitec` built on a glibc machine embeds a glibc runtime.
fn runtime_target() -> String {
    if let Ok(t) = env::var("KITE_RT_TARGET") {
        if !t.is_empty() {
            return t;
        }
    }
    let target = env::var("TARGET").expect("Cargo sets TARGET");
    let host = env::var("HOST").unwrap_or_default();
    match target.strip_suffix("-linux-musl") {
        Some(arch_vendor) if !host.ends_with("-linux-musl") => {
            format!("{}-linux-gnu", arch_vendor)
        }
        _ => target,
    }
}

/// The workspace's edition, which `kite-rt` inherits. Read rather than written
/// down twice, so an edition change cannot leave the embedded runtime compiled
/// under the old one.
fn workspace_edition() -> String {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets it"));
    let workspace = manifest.join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", workspace.display());
    std::fs::read_to_string(&workspace)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| {
                let value = l.trim().strip_prefix("edition")?.trim().strip_prefix('=')?;
                Some(value.trim().trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| "2021".to_string())
}
