//! `kitec pkg` — resolve what a manifest asks for, and write down what was
//! resolved.
//!
//! This is the only thing in the toolchain that fetches anything, and it does
//! so only when asked. A build never reaches the network: it reads
//! `.kite/vendor`, which this put there.
//!
//! There is no post-install script and no build-time code execution, because
//! there is nowhere to put one — a dependency is `.kite` files. That is the
//! npm supply-chain surface removed by construction rather than by policy.

use kite_driver::manifest::{self, Dependency, Locked, Manifest, Source};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where dependencies are put, relative to the manifest.
const VENDOR: &str = ".kite/vendor";

pub fn run(dir: &Path, offline: bool) -> ExitCode {
    let manifest_path = dir.join("kite.toml");
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        eprintln!(
            "error: no `kite.toml` in {}\n\nnote: a package is a directory with a manifest in it",
            dir.display()
        );
        return ExitCode::FAILURE;
    };
    let manifest = match manifest::parse(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::FAILURE;
        }
    };

    eprintln!("{} {}", manifest.name, manifest.version);
    let mut locked = Vec::new();
    for dependency in &manifest.dependencies {
        match resolve(dir, dependency, offline) {
            Ok(entry) => {
                eprintln!("  {} {}", entry.name, entry.hash);
                locked.push(entry);
            }
            Err(message) => {
                eprintln!("error: {}", message);
                return ExitCode::FAILURE;
            }
        }
    }

    let lock_path = dir.join("kite.lock");
    let text = manifest::lockfile(&locked);
    // A lockfile that changed is worth saying out loud: it is the difference
    // between a build from the same bytes and a build from different ones.
    let previous = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if let Err(e) = std::fs::write(&lock_path, &text) {
        eprintln!("error: cannot write `{}`: {}", lock_path.display(), e);
        return ExitCode::FAILURE;
    }
    if previous.is_empty() {
        eprintln!("wrote kite.lock");
    } else if previous != text {
        eprintln!("kite.lock changed — a dependency is not what it was");
    } else {
        eprintln!("kite.lock is unchanged");
    }
    check_entries(&manifest, dir)
}

/// Every target's entry must exist, because a manifest that names a file that
/// is not there is a manifest nobody has run.
fn check_entries(manifest: &Manifest, dir: &Path) -> ExitCode {
    let mut missing = Vec::new();
    for (name, target) in &manifest.targets {
        if !dir.join(&target.entry).exists() {
            missing.push(format!("  {} → {}", name, target.entry));
        }
    }
    if missing.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("error: a target names an entry that does not exist:\n{}", missing.join("\n"));
    ExitCode::FAILURE
}

fn resolve(dir: &Path, dependency: &Dependency, offline: bool) -> Result<Locked, String> {
    match &dependency.source {
        Source::Path(path) => {
            let source = dir.join(path);
            if !source.is_dir() {
                return Err(format!(
                    "`{}` points at `{}`, which is not a directory",
                    dependency.name,
                    source.display()
                ));
            }
            let hash = manifest::hash_directory(&source)
                .map_err(|e| format!("cannot read `{}`: {}", source.display(), e))?;
            Ok(Locked { name: dependency.name.clone(), source: path.clone(), hash })
        }

        Source::Git { url, tag } => {
            let vendor = dir.join(VENDOR).join(&dependency.name);
            if !vendor.exists() {
                if offline {
                    return Err(format!(
                        "`{}` is not in {} and `--offline` was given",
                        dependency.name, VENDOR
                    ));
                }
                clone(url, tag.as_deref(), &vendor)?;
            }
            let hash = manifest::hash_directory(&vendor)
                .map_err(|e| format!("cannot read `{}`: {}", vendor.display(), e))?;
            let source = match tag {
                Some(t) => format!("{}#{}", url, t),
                None => url.clone(),
            };
            Ok(Locked { name: dependency.name.clone(), source, hash })
        }
    }
}

/// Clone a dependency at one tag, without its history.
///
/// `git` is shelled out to rather than reimplemented: it is on every machine
/// that has a compiler, it knows about credentials and proxies, and a
/// hand-rolled fetch would be a second thing to keep secure.
fn clone(url: &str, tag: Option<&str>, into: &PathBuf) -> Result<(), String> {
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create vendor: {}", e))?;
    }
    let mut command = Command::new("git");
    command.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(tag) = tag {
        command.args(["--branch", tag]);
    }
    command.arg(url).arg(into);
    let out = command
        .output()
        .map_err(|e| format!("cannot run `git`: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "cloning {} failed:\n{}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
