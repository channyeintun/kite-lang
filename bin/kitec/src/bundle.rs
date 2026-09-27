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
//! program runs on the bytecode VM. The native backend exists — `kitec build
//! --native` writes machine code and links it — but it needs a linker and
//! refuses some hosts, and a bundle is for the machine that has neither.
//!
//! **What is appended is every file the build read**, not only the entry. A
//! bundle used to carry the one file it was pointed at, so a program with a
//! `use` compiled when it was bundled and failed with `cannot find module` on
//! the machine it was bundled for. The files go back through the same module
//! loader, from memory, so a `use` resolves in the bundle exactly as it did
//! beside the source — manifests and dependencies included.

use kite_driver::modules::Files;
use kite_driver::{compile_files, compile_with, Emit};
use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

/// What marks an appended program, and how its length is recorded.
///
/// The trailer goes at the very end because that is the one place an appended
/// payload can be found without parsing the executable format — and this has
/// to work on Mach-O, ELF and PE alike. It is sixteen bytes: the payload's
/// length, then this.
const MAGIC: &[u8; 8] = b"KITEBND2";

/// The directory a bundle's files are laid out under when they are read back.
/// It is never on a disk, so it only has to be a name nothing else is.
const ROOT: &str = "<bundled>";

/// A bundled program: whether it was built for release, where its entry is,
/// and every file it was compiled from.
pub struct Bundle {
    release: bool,
    entry: PathBuf,
    files: BTreeMap<PathBuf, String>,
}

/// The program appended to this executable, if there is one.
///
/// Only the trailer is read to find out. This runs at the start of every
/// `kitec` invocation, and reading the whole executable to look at its last
/// sixteen bytes cost ten megabytes of I/O per command.
pub fn embedded() -> Option<Bundle> {
    let path = std::env::current_exe().ok()?;
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.seek(SeekFrom::End(0)).ok()?;
    if size < 16 {
        return None;
    }
    let mut trailer = [0u8; 16];
    file.seek(SeekFrom::End(-16)).ok()?;
    file.read_exact(&mut trailer).ok()?;
    if &trailer[8..] != MAGIC {
        return None;
    }
    let length = u64::from_le_bytes(trailer[..8].try_into().ok()?);
    let start = (size - 16).checked_sub(length)?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut payload = vec![0u8; usize::try_from(length).ok()?];
    file.read_exact(&mut payload).ok()?;
    decode(&payload)
}

/// Run an embedded program.
pub fn run(bundle: &Bundle) -> ExitCode {
    let Some(source) = bundle.files.get(&bundle.entry).cloned() else {
        eprintln!("error: this bundle does not carry its own entry file");
        return ExitCode::FAILURE;
    };
    // Built the way it was bundled: a bundle of a debug build keeps its
    // `assert`s, and one built with `--release` drops them. It used to be a
    // release build whatever `kitec bundle` had been told.
    let compiled = compile_files(
        &bundle.entry,
        &source,
        Emit::Check,
        bundle.release,
        Files::Memory(bundle.files.clone()),
    );
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
    let payload = match encode(&compiled.inputs, release) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::FAILURE;
        }
    };

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

    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
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
        "wrote {} ({} KB, {} source file{}), which needs nothing installed",
        target.display(),
        bytes.len() / 1024,
        compiled.inputs.len(),
        if compiled.inputs.len() == 1 { "" } else { "s" }
    );
    ExitCode::SUCCESS
}

/// A path made absolute and folded, without asking the disk about links —
/// the loader builds its paths the same way, so the tree it walks in memory
/// is the one it walked on disk.
fn absolute(path: &Path) -> PathBuf {
    let path = if path.is_relative() {
        std::env::current_dir().map(|here| here.join(path)).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The payload: a flag, then every file as a relative path and its text.
///
/// ```text
/// u8   release
/// u32  count        the entry is the first file
/// for each:  u32 path length, path (`/`-separated), u32 text length, text
/// ```
///
/// Paths are relative to the deepest directory holding all of them, so a
/// bundle names nothing about the machine it was built on.
fn encode(inputs: &[(PathBuf, String)], release: bool) -> Result<Vec<u8>, String> {
    let absolute: Vec<(PathBuf, &String)> =
        inputs.iter().map(|(path, text)| (absolute(path), text)).collect();
    let mut common: Option<PathBuf> = None;
    for (path, _) in &absolute {
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        common = Some(match common {
            None => dir,
            Some(so_far) => {
                let shared: PathBuf = so_far
                    .components()
                    .zip(dir.components())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.as_os_str().to_owned())
                    .collect();
                shared
            }
        });
    }
    let common = common.ok_or("there is nothing to bundle")?;

    let mut out = vec![u8::from(release)];
    push_u32(&mut out, absolute.len())?;
    for (path, text) in &absolute {
        let relative = path.strip_prefix(&common).map_err(|_| {
            format!("`{}` is not under `{}`", path.display(), common.display())
        })?;
        let parts: Vec<String> =
            relative.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        let name = parts.join("/");
        push_u32(&mut out, name.len())?;
        out.extend_from_slice(name.as_bytes());
        push_u32(&mut out, text.len())?;
        out.extend_from_slice(text.as_bytes());
    }
    Ok(out)
}

fn push_u32(out: &mut Vec<u8>, n: usize) -> Result<(), String> {
    let n = u32::try_from(n).map_err(|_| "a bundled file is larger than 4 GB".to_string())?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

/// The payload [`encode`] wrote, laid out under [`ROOT`].
fn decode(bytes: &[u8]) -> Option<Bundle> {
    let (&release, mut rest) = bytes.split_first()?;
    let mut take = |n: usize| -> Option<&[u8]> {
        let (head, tail) = rest.split_at_checked(n)?;
        rest = tail;
        Some(head)
    };
    let count = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let mut files = BTreeMap::new();
    let mut entry = None;
    for _ in 0..count {
        let length = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let name = std::str::from_utf8(take(length)?).ok()?;
        let length = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let text = std::str::from_utf8(take(length)?).ok()?.to_string();
        let mut path = PathBuf::from(ROOT);
        for part in name.split('/') {
            path.push(part);
        }
        entry.get_or_insert_with(|| path.clone());
        files.insert(path, text);
    }
    Some(Bundle { release: release != 0, entry: entry?, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What goes in comes out, laid out under one root with the entry first.
    #[test]
    fn a_payload_round_trips() {
        let here = std::env::current_dir().expect("cwd");
        let inputs = vec![
            (here.join("app/src/main.kite"), "use helper\n".to_string()),
            (here.join("app/src/helper.kite"), "pub fn h() {\n}\n".to_string()),
            (here.join("app/kite.toml"), "[package]\n".to_string()),
            (here.join("app/src/../../md/md.kite"), "pub fn m() {\n}\n".to_string()),
        ];
        let bundle = decode(&encode(&inputs, true).expect("encodes")).expect("decodes");
        assert!(bundle.release);
        assert_eq!(bundle.entry, Path::new(ROOT).join("app/src/main.kite"));
        let names: Vec<String> =
            bundle.files.keys().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(
            names,
            vec![
                format!("{}/app/kite.toml", ROOT),
                format!("{}/app/src/helper.kite", ROOT),
                format!("{}/app/src/main.kite", ROOT),
                format!("{}/md/md.kite", ROOT),
            ]
        );
        assert_eq!(bundle.files[&bundle.entry], "use helper\n");
    }

    #[test]
    fn a_truncated_payload_is_not_a_bundle() {
        let here = std::env::current_dir().expect("cwd");
        let payload = encode(&[(here.join("a.kite"), "fn main() {\n}\n".to_string())], false)
            .expect("encodes");
        assert!(decode(&payload[..payload.len() - 3]).is_none());
    }
}
