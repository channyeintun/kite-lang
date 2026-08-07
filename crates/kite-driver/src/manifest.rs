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

use crate::semver::Requirement;
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
    /// `version = "^1.2"` — what this package asks of the dependency's
    /// version, resolved by `kitec pkg` against every other requirement on
    /// the same name. Absent means any: a pinned source — a path, or a git
    /// tag — already offers exactly one version, and an unpinned one takes
    /// the highest that everyone naming it can live with.
    pub version: Option<Requirement>,
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
                "name" => {
                    let name = unquote(value, line_number)?;
                    check_name("package", &name, line_number)?;
                    manifest.name = name;
                }
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
                check_name("dependency", key, line_number)?;
                let (source, version) = if value.starts_with('"') {
                    (Source::Path(unquote(value, line_number)?), None)
                } else {
                    let fields = inline_table(value, line_number)?;
                    let version = match fields.get("version") {
                        None => None,
                        Some(text) => {
                            Some(Requirement::parse(text).map_err(|message| ManifestError {
                                line: line_number,
                                message: format!("dependency `{}`: {}", key, message),
                            })?)
                        }
                    };
                    let source = match (fields.get("path"), fields.get("git")) {
                        (Some(path), None) => Source::Path(path.clone()),
                        (None, Some(url)) => {
                            let tag = fields.get("tag").cloned();
                            if tag.is_some() && version.is_some() {
                                return Err(ManifestError {
                                    line: line_number,
                                    message: format!(
                                        "dependency `{}` has both a `tag` and a `version`; \
                                         pick one — a tag pins, a version resolves",
                                        key
                                    ),
                                });
                            }
                            Source::Git { url: url.clone(), tag }
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
                    };
                    (source, version)
                };
                manifest.dependencies.push(Dependency {
                    name: key.to_string(),
                    source,
                    version,
                });
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

/// A package name, checked before anything is built out of it.
///
/// A name is not only a label: `kitec pkg` joins it onto a path under
/// `.kite/vendor`, and the module loader joins it again at build time. A name
/// containing `/` or `..`, or an absolute one, therefore escapes that
/// directory — and since a *dependency's own* manifest introduces names
/// transitively, the name that escapes need never appear in the manifest
/// anybody wrote.
///
/// So it is checked here, once, at the only place a name enters the program.
/// The rule is deliberately narrow — ASCII letters, digits, `-` and `_` —
/// because a package name is written by hand, typed into a `use`, and turned
/// into a directory, and anything wider buys nothing for all three.
///
/// This is the guarantee `kitec pkg` is built on: the module's own header
/// claims the npm supply-chain surface is removed *by construction*, and a
/// name that can reach outside its directory would have put it back.
fn check_name(what: &str, name: &str, line: usize) -> Result<(), ManifestError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        return Ok(());
    }
    Err(ManifestError {
        line,
        message: format!(
            "`{}` is not a {} name\n  a name is ASCII letters, digits, `-` and `_`, at most 64 \
             of them, because it becomes a directory under `.kite/vendor` and a name in a `use`",
            name, what
        ),
    })
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
    /// The version resolution settled on — the one that satisfied every
    /// `version = "…"` requirement on the name, or the one a pinned source
    /// has.
    pub version: String,
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
         # Each hash is SHA-256 over every `.kite` file of that dependency, by\n\
         # name and contents in sorted order. A build that produces a different\n\
         # one is a build from different bytes, whatever the version says.\n\n",
    );
    for entry in entries {
        out.push_str(&format!(
            "[[locked]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{}\"\nhash = \"{}\"\n\n",
            entry.name, entry.version, entry.source, entry.hash
        ));
    }
    out
}

/// A content hash over a dependency's directory.
///
/// SHA-256 over the sorted file names and their bytes.
///
/// This was FNV-1a, on the stated grounds that it catches a dependency whose
/// contents changed under a fixed version and does not pretend to be a
/// signature. That was fair while nothing acted on the answer. It stopped being
/// fair when `kitec pkg` began to *fail* on a mismatch, because the party that
/// failure is aimed at is one who controls the dependency's bytes — a moved
/// tag, a re-pushed repository — and FNV-1a is a multiply by an odd constant,
/// which is invertible. A suffix that lands the digest on any chosen value is
/// solved for, not searched for. A gate that can be walked through is worse
/// than no gate, because a gate is believed.
///
/// Each field is length-prefixed, so a file named `ab` holding `c` and one
/// named `a` holding `bc` do not present the same bytes to the hash. FNV had
/// the same ambiguity and it was reachable by anyone choosing filenames.
///
/// Written out rather than taken from a crate. `kite-driver` has no external
/// dependencies and compiles to wasm32 for the playground, so a crate added
/// here is a crate added there; the algorithm is eighty lines and fixed
/// forever.
pub fn hash_directory(dir: &Path) -> std::io::Result<String> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_kite_files(dir, &mut files)?;
    files.sort();

    let mut hash = Sha256::new();
    for file in &files {
        let name = file.strip_prefix(dir).unwrap_or(file).to_string_lossy().to_string();
        hash.update(&(name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        let body = std::fs::read(file)?;
        hash.update(&(body.len() as u64).to_be_bytes());
        hash.update(&body);
    }
    let digest = hash.finish();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    Ok(out)
}

/// SHA-256, streaming, from FIPS 180-4.
///
/// Incremental rather than one buffer, so hashing a dependency does not hold
/// every one of its files in memory at once.
struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

/// The first thirty-two bits of the fractional parts of the cube roots of the
/// first sixty-four primes.
#[rustfmt::skip]
const ROUND: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Sha256 {
        Sha256 {
            // The fractional parts of the square roots of the first eight primes.
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        while !bytes.is_empty() {
            let want = 64 - self.filled;
            let take = want.min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == 64 {
                self.compress();
                self.filled = 0;
            }
        }
    }

    /// The padding is a `1` bit, then zeroes, then the message length in bits
    /// as a big-endian `u64` — landing the total on a multiple of the block.
    fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        // Written straight into the block: going through `update` again would
        // add these eight bytes to a length that has already been committed.
        self.block[56..64].copy_from_slice(&bits.to_be_bytes());
        self.compress();

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                self.block[i * 4],
                self.block[i * 4 + 1],
                self.block[i * 4 + 2],
                self.block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(ROUND[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
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
