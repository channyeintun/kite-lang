//! `kitec bundle` — one file that needs nothing installed.
//!
//! A compiler nobody can install is a compiler nobody uses, and the same is
//! true of the programs it produces: "install this runtime first" is where
//! most small tools lose their audience.
//!
//! A bundle is **this binary with the program appended to it**. Running it
//! finds the program, compiles it, and runs it — which takes about a
//! millisecond, because compiling is the fast part and there is no linker, no
//! runtime to unpack and no temporary file.
//!
//! Being honest about what this is: it is packaging, not code generation. The
//! program still runs on the bytecode VM. The native *backend* — Cranelift,
//! machine code, a precise collector — is Phase 9 and is not written; see
//! `docs/06-roadmap.md`, which says so in the same words.

use kite_driver::{compile_with, Emit};
use std::io::{self, Write};
use std::process::ExitCode;

/// What marks an appended program, and how its length is recorded.
///
/// The trailer goes at the very end because that is the one place an appended
/// payload can be found without parsing the executable format — and this has
/// to work on Mach-O, ELF and PE alike.
const MAGIC: &[u8; 8] = b"KITEBNDL";

/// The program appended to this executable, if there is one.
pub fn embedded() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < MAGIC.len() + 8 {
        return None;
    }
    let end = bytes.len() - MAGIC.len();
    if &bytes[end..] != MAGIC {
        return None;
    }
    let length_at = end - 8;
    let length = u64::from_le_bytes(bytes[length_at..end].try_into().ok()?) as usize;
    let start = length_at.checked_sub(length)?;
    String::from_utf8(bytes[start..length_at].to_vec()).ok()
}

/// Run an embedded program.
pub fn run(source: &str) -> ExitCode {
    // The name is what a diagnostic would say, and a bundle's diagnostics are
    // about a file the user may not have — so it says which program it is
    // rather than pretending to a path.
    let compiled = compile_with("<bundled>", source, Emit::Check, true);
    if compiled.failed() {
        eprint!("{}", compiled.render_diagnostics());
        return ExitCode::FAILURE;
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match compiled.run(&mut out) {
        Ok(_) => {
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Err(trap) => {
            let _ = out.flush();
            eprintln!("\nerror: {}", trap);
            ExitCode::FAILURE
        }
    }
}

/// Write a bundle: this binary, then the program, then how long it was.
pub fn write(path: &str, src: &str, out_dir: Option<&str>, release: bool) -> ExitCode {
    // It has to compile before it is packaged. A bundle that fails at startup
    // would move a compile error to the user's machine, which is exactly the
    // arrangement this is meant to avoid.
    let compiled = compile_with(path, src, Emit::Check, release);
    if !compiled.diags.is_empty() {
        eprint!("{}", compiled.render_diagnostics());
    }
    if compiled.failed() {
        return ExitCode::FAILURE;
    }
    if !compiled.is_runnable() {
        eprintln!("error: `{}` has no `main` to bundle", path);
        return ExitCode::FAILURE;
    }

    let Ok(compiler) = std::env::current_exe() else {
        eprintln!("error: cannot find this executable to copy");
        return ExitCode::FAILURE;
    };
    let Ok(mut bytes) = std::fs::read(&compiler) else {
        eprintln!("error: cannot read `{}`", compiler.display());
        return ExitCode::FAILURE;
    };

    let name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("program");
    let dir = out_dir.unwrap_or(".");
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("error: cannot create `{}`: {}", dir, e);
        return ExitCode::FAILURE;
    }
    let target = std::path::Path::new(dir).join(if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    });

    bytes.extend_from_slice(src.as_bytes());
    bytes.extend_from_slice(&(src.len() as u64).to_le_bytes());
    bytes.extend_from_slice(MAGIC);

    if let Err(e) = std::fs::write(&target, &bytes) {
        eprintln!("error: cannot write `{}`: {}", target.display(), e);
        return ExitCode::FAILURE;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    eprintln!(
        "wrote {} ({} KB), which needs nothing installed",
        target.display(),
        bytes.len() / 1024
    );
    ExitCode::SUCCESS
}
