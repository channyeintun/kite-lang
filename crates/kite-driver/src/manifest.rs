//! `kite.toml`, and the lockfile beside it.
//!
//! The manifest names the package, its entry points, and its dependencies. The
//! lockfile records the **content hash** of every dependency, so what is built
//! twice is built from the same bytes twice.
//!
//! Three things are absent by construction rather than by policy, and they are
//! the three that have repeatedly compromised npm:
//!
//! * **No post-install scripts.** Nothing in a dependency runs at install time.
//! * **No build-time code execution.** A dependency is `.kite` files; there is
//!   no build script to run and no plugin interface to run one through.
//! * **No transitive hoisting.** Each dependency is resolved to a directory and
//!   used from there, so what a module sees does not depend on what its
//!   siblings happen to need.
//!
//! The TOML reader is a subset: tables, string values, and nothing else. A
//! manifest that needs more than that is a manifest that has grown a
//! programming language, which is what this is avoiding.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A parsed `kite.toml`.
#[derive(Debug, Default, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// `[targets]` — `web = { entry = "src/main.kite", renderer = "dom" }`.
    pub targets: BTreeMap<String, Target>,
    /// `[dependencies]` — by name, in the order they were written.
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Target {
    pub entry: String,
    pub renderer: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub source: Source,
}

#[derive(Debug, PartialEq)]
pub enum Source {
    /// A directory, relative to the manifest.
    Path(String),
    /// A repository and a tag. Resolved by `kitec pkg`, which clones it into
    /// `.kite/vendor` — the one place anything is fetched, and never during a
    /// build.
    Git { url: String, tag: Option<String> },
}

/// What went wrong reading a manifest, with the line it was on.
#[derive(Debug, PartialEq)]
pub struct ManifestError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "kite.toml:{}: {}", self.line, self.message)
    }
}

/// Read a manifest.
pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let mut manifest = Manifest::default();
    let mut table = String::new();

    for (i, raw) in text.lines().enumerate() {
        let line_number = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ManifestError {
                line: line_number,
                message: format!("expected `key = value`, found `{}`", line),
            });
        };
        let key = key.trim();
        let value = value.trim();

        match table.as_str() {
            "package" => match key {
                "name" => manifest.name = unquote(value, line_number)?,
                "version" => manifest.version = unquote(value, line_number)?,
                other => {
                    return Err(ManifestError {
                        line: line_number,
                        message: format!("`[package]` has no `{}`; it has name and version", other),
                    })
                }
            },
            "targets" => {
                let fields = inline_table(value, line_number)?;
                let Some(entry) = fields.get("entry") else {
                    return Err(ManifestError {
                        line: line_number,
                        message: format!("target `{}` needs an `entry`", key),
                    });
                };
                manifest.targets.insert(
                    key.to_string(),
                    Target { entry: entry.clone(), renderer: fields.get("renderer").cloned() },
                );
            }
            "dependencies" => {
                let source = if value.starts_with('"') {
                    Source::Path(unquote(value, line_number)?)
                } else {
                    let fields = inline_table(value, line_number)?;
                    match (fields.get("path"), fields.get("git")) {
                        (Some(path), None) => Source::Path(path.clone()),
                        (None, Some(url)) => {
                            Source::Git { url: url.clone(), tag: fields.get("tag").cloned() }
                        }
                        _ => {
                            return Err(ManifestError {
                                line: line_number,
                                message: format!(
                                    "dependency `{}` needs exactly one of `path` or `git`",
                                    key
                                ),
                            })
                        }
                    }
                };
                manifest.dependencies.push(Dependency { name: key.to_string(), source });
            }
            "" => {
                return Err(ManifestError {
                    line: line_number,
                    message: "a key outside any table".to_string(),
                })
            }
            other => {
                return Err(ManifestError {
                    line: line_number,
                    message: format!("unknown table `[{}]`", other),
                })
            }
        }
    }

    if manifest.name.is_empty() {
        return Err(ManifestError {
            line: 1,
            message: "no `[package] name`".to_string(),
        });
    }
    Ok(manifest)
}

fn strip_comment(line: &str) -> &str {
    // A `#` inside a string is not a comment, and a manifest's strings are
    // paths and URLs, which contain them.
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str, line: usize) -> Result<String, ManifestError> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .map(|v| v.to_string())
        .ok_or(ManifestError {
            line,
            message: format!("expected a quoted string, found `{}`", trimmed),
        })
}

/// `{ entry = "src/main.kite", renderer = "dom" }`
fn inline_table(value: &str, line: usize) -> Result<BTreeMap<String, String>, ManifestError> {
    let inner = value
        .trim()
        .strip_prefix('{')
        .and_then(|v| v.strip_suffix('}'))
        .ok_or(ManifestError {
            line,
            message: format!("expected `{{ … }}`, found `{}`", value.trim()),
        })?;
    let mut out = BTreeMap::new();
    for field in split_fields(inner) {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        let Some((key, value)) = field.split_once('=') else {
            return Err(ManifestError {
                line,
                message: format!("expected `key = value`, found `{}`", field),
            });
        };
        out.insert(key.trim().to_string(), unquote(value, line)?);
    }
    Ok(out)
}

/// Split on commas that are not inside a string.
fn split_fields(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for c in inner.chars() {
        match c {
            '"' => {
                in_string = !in_string;
                current.push(c);
            }
            ',' if !in_string => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    out.push(current);
    out
}

// ---------------------------------------------------------------------------
// The lockfile
// ---------------------------------------------------------------------------

/// One resolved dependency: where it came from and what it contained.
#[derive(Debug, PartialEq)]
pub struct Locked {
    pub name: String,
    pub source: String,
    /// A hash over every `.kite` file in the dependency, in sorted order.
    pub hash: String,
}

/// The lockfile's text, which is written rather than parsed by anything but a
/// person: it exists to be committed and compared.
pub fn lockfile(entries: &[Locked]) -> String {
    let mut out = String::from(
        "# Generated by `kitec pkg`. Commit this.\n\
         #\n\
         # Each hash covers every `.kite` file of that dependency. A build that\n\
         # produces a different one is a build from different bytes, whatever the\n\
         # version says.\n\n",
    );
    for entry in entries {
        out.push_str(&format!(
            "[[locked]]\nname = \"{}\"\nsource = \"{}\"\nhash = \"{}\"\n\n",
            entry.name, entry.source, entry.hash
        ));
    }
    out
}

/// A content hash over a dependency's directory.
///
/// FNV-1a over the sorted file names and their bytes. Not a cryptographic
/// hash, and it does not claim to be: this catches a dependency whose contents
/// changed under a fixed version, which is the failure a lockfile exists to
/// notice. Signing is a separate thing and is not pretended at here.
pub fn hash_directory(dir: &Path) -> std::io::Result<String> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_kite_files(dir, &mut files)?;
    files.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    };
    for file in &files {
        let name = file.strip_prefix(dir).unwrap_or(file).to_string_lossy().to_string();
        mix(name.as_bytes());
        mix(&std::fs::read(file)?);
    }
    Ok(format!("{:016x}", hash))
}

fn collect_kite_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_kite_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "kite") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
