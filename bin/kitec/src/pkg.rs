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
//!
//! Versions are resolved before anything is hashed. Every requirement on a
//! name — from the root manifest and from every dependency's own — is
//! satisfied by the one version the lockfile records; the solver lives in
//! `kite_driver::solve`, and this file is its [`Registry`]: paths on disk,
//! and tags read with `git ls-remote`. A candidate under consideration is
//! cloned into `.kite/vendor/<name>@<version>`; the version chosen is then
//! placed at `.kite/vendor/<name>`, the one directory per name a build reads,
//! which is the no-hoisting stance on disk.

use kite_driver::manifest::{self, Locked, Manifest, Source};
use kite_driver::semver::Version;
use kite_driver::solve::{self, Registry};
use std::collections::BTreeMap;
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
    let locked = match lock_dependencies(&manifest, dir, offline) {
        Ok(locked) => locked,
        Err(message) => {
            eprintln!("error: {}", message);
            return ExitCode::FAILURE;
        }
    };
    for entry in &locked {
        eprintln!("  {} {} {}", entry.name, entry.version, entry.hash);
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

/// Resolve every dependency, direct and transitive, and hash what resolution
/// chose. The returned entries are what the lockfile records.
fn lock_dependencies(root: &Manifest, dir: &Path, offline: bool) -> Result<Vec<Locked>, String> {
    let mut vendor = Vendor::new(dir, offline);
    for dep in &root.dependencies {
        vendor.register(&dep.name, Origin::of(&vendor.root, &dep.source))?;
    }
    let resolution = solve::resolve(root, &mut vendor)?;

    let mut locked = Vec::new();
    for resolved in &resolution {
        let build_dir = vendor.build_dir(&resolved.name, &resolved.version)?;
        let hash = manifest::hash_directory(&build_dir)
            .map_err(|e| format!("cannot read `{}`: {}", build_dir.display(), e))?;
        locked.push(Locked {
            name: resolved.name.clone(),
            version: resolved.version.to_string(),
            source: vendor.lock_source(&resolved.name, &resolved.version),
            hash,
        });
    }
    Ok(locked)
}

// ---------------------------------------------------------------------------
// The registry: paths on disk, and git tags
// ---------------------------------------------------------------------------

/// Where a dependency name comes from, learned from the manifests naming it.
#[derive(Clone, Debug, PartialEq)]
enum Origin {
    /// An absolute directory.
    Path(PathBuf),
    Git { url: String, tag: Option<String> },
}

impl Origin {
    /// A manifest's source, made absolute against the directory the manifest
    /// sits in — two manifests reaching one directory by different relative
    /// paths are naming the same place, and should be recognised as such.
    fn of(dir: &Path, source: &Source) -> Origin {
        match source {
            Source::Path(path) => {
                let joined = dir.join(path);
                Origin::Path(joined.canonicalize().unwrap_or(joined))
            }
            Source::Git { url, tag } => Origin::Git { url: url.clone(), tag: tag.clone() },
        }
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Path(path) => write!(f, "{}", path.display()),
            Origin::Git { url, tag: Some(tag) } => write!(f, "{}#{}", url, tag),
            Origin::Git { url, tag: None } => write!(f, "{}", url),
        }
    }
}

/// The real [`Registry`]: what exists on disk and at the ends of git URLs.
struct Vendor {
    /// The root package's directory, canonicalised so vendored paths display
    /// relative to it.
    root: PathBuf,
    offline: bool,
    origins: BTreeMap<String, Origin>,
    /// How each discovered version was spelled as a tag — `1.2.0` may be the
    /// tag `v1.2.0`, and the lockfile should quote what the repository says.
    tags: BTreeMap<(String, String), String>,
    /// Candidate checkouts already on disk, by (name, version).
    checkouts: BTreeMap<(String, String), PathBuf>,
    versions: BTreeMap<String, Vec<Version>>,
}

impl Vendor {
    fn new(dir: &Path, offline: bool) -> Vendor {
        Vendor {
            root: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
            offline,
            origins: BTreeMap::new(),
            tags: BTreeMap::new(),
            checkouts: BTreeMap::new(),
            versions: BTreeMap::new(),
        }
    }

    /// Learn where a name comes from. Registration is first-writer-wins and
    /// disagreement is an error rather than something backtracked around: a
    /// name that means two things depending on which manifest is read is
    /// exactly the ambiguity resolution exists to refuse.
    fn register(&mut self, name: &str, origin: Origin) -> Result<(), String> {
        let Some(existing) = self.origins.get(name) else {
            self.origins.insert(name.to_string(), origin);
            return Ok(());
        };
        if *existing == origin {
            return Ok(());
        }
        match (existing, &origin) {
            (Origin::Git { url: a, tag: first }, Origin::Git { url: b, tag: second }) if a == b => {
                Err(match (first, second) {
                    (Some(x), Some(y)) => format!(
                        "`{}` is pinned to two different tags: `{}` and `{}`\n\nnote: a tag \
                         pins for everyone naming `{}` — agree on one, or switch both to \
                         `version = \"…\"` and let resolution choose",
                        name, x, y, name
                    ),
                    _ => format!(
                        "`{}` is pinned to a tag by one manifest and left to resolve by \
                         another\n\nnote: a pin binds everyone naming `{}`; give both \
                         manifests the tag, or neither",
                        name, name
                    ),
                })
            }
            _ => Err(format!(
                "`{}` is named from two different places:\n    {}\n    {}\n\nnote: a name \
                 means one thing — one of the manifests must rename or repoint it",
                name, existing, origin
            )),
        }
    }

    fn origin(&self, name: &str) -> Result<Origin, String> {
        self.origins.get(name).cloned().ok_or_else(|| {
            format!("nothing says where `{}` comes from — no reachable kite.toml declares it", name)
        })
    }

    fn candidate_dir(&self, name: &str, version: &str) -> PathBuf {
        // A tag may contain `/`, which a directory name may not.
        let version = version.replace('/', "-");
        self.root.join(VENDOR).join(format!("{}@{}", name, version))
    }

    /// Read and parse a dependency's manifest, and learn where its own
    /// dependencies come from — their relative paths are relative to *it*.
    fn load_manifest(&mut self, name: &str, dir: &Path) -> Result<Manifest, String> {
        let path = dir.join("kite.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Err(format!(
                "`{}` has no kite.toml at {} — resolution reads a dependency's manifest for \
                 its version and its own dependencies",
                name,
                dir.display()
            ));
        };
        let parsed = manifest::parse(&text).map_err(|e| format!("`{}`: {}", name, e))?;
        for dep in &parsed.dependencies {
            self.register(&dep.name, Origin::of(dir, &dep.source))?;
        }
        Ok(parsed)
    }

    /// The version a manifest declares, which is the version the package has.
    fn declared_version(&self, name: &str, manifest: &Manifest) -> Result<Version, String> {
        if manifest.version.is_empty() {
            return Err(format!(
                "`{}`'s kite.toml declares no version, and resolution orders versions — add \
                 `version = \"0.1.0\"` under `[package]`",
                name
            ));
        }
        Version::parse(&manifest.version).map_err(|e| format!("`{}`'s kite.toml: {}", name, e))
    }

    fn not_vendored(&self, name: &str) -> String {
        format!(
            "`{}` is not in {} and `--offline` was given\n\nnote: run `kitec pkg` once \
             without `--offline` to fetch what resolution needs",
            name, VENDOR
        )
    }

    /// The one version a pinned tag has: clone it, read its manifest, and —
    /// when the tag itself spells a version — insist the two agree.
    fn pinned_version(&mut self, name: &str, url: &str, tag: &str) -> Result<Version, String> {
        let tagged = version_from_tag(tag);
        let dir = match &tagged {
            Some(v) => self.candidate_dir(name, &v.to_string()),
            None => self.candidate_dir(name, tag),
        };
        if !dir.exists() {
            if self.offline {
                return Err(self.not_vendored(name));
            }
            clone(url, Some(tag), &dir)?;
        }
        let manifest = self.load_manifest(name, &dir)?;
        let declared = self.declared_version(name, &manifest)?;
        if let Some(tagged) = tagged {
            if tagged != declared {
                return Err(format!(
                    "tag `{}` of `{}` contains a manifest declaring {} — the tag and the \
                     manifest disagree about the version",
                    tag, name, declared
                ));
            }
        }
        self.tags.insert((name.to_string(), declared.to_string()), tag.to_string());
        self.checkouts.insert((name.to_string(), declared.to_string()), dir);
        Ok(declared)
    }

    /// Which versions of an unpinned git dependency are already vendored —
    /// all `--offline` has to offer.
    fn vendored_versions(&mut self, name: &str) -> Vec<Version> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.root.join(VENDOR)) else { return found };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let Some(rest) = file_name.strip_prefix(&format!("{}@", name)) else { continue };
            if let Ok(version) = Version::parse(rest) {
                self.checkouts.insert((name.to_string(), version.to_string()), entry.path());
                found.push(version);
            }
        }
        found
    }

    /// The directory a build will read, which is what gets hashed. A path
    /// dependency is used where it is; a git one is placed at
    /// `.kite/vendor/<name>` — refreshed only when it is not already the
    /// chosen version, so an unchanged resolution touches nothing.
    fn build_dir(&mut self, name: &str, version: &Version) -> Result<PathBuf, String> {
        match self.origin(name)? {
            Origin::Path(dir) => Ok(dir),
            Origin::Git { .. } => {
                let key = (name.to_string(), version.to_string());
                let candidate = self
                    .checkouts
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| self.candidate_dir(name, &version.to_string()));
                let placed = self.root.join(VENDOR).join(name);
                if is_version(&placed, version)
                    && manifest::hash_directory(&placed).ok()
                        == manifest::hash_directory(&candidate).ok()
                {
                    return Ok(placed);
                }
                if placed.exists() {
                    std::fs::remove_dir_all(&placed)
                        .map_err(|e| format!("cannot clear `{}`: {}", placed.display(), e))?;
                }
                copy_dir(&candidate, &placed)
                    .map_err(|e| format!("cannot place `{}`: {}", placed.display(), e))?;
                Ok(placed)
            }
        }
    }

    /// The `source` line the lockfile shows for a resolved package.
    fn lock_source(&self, name: &str, version: &Version) -> String {
        match self.origins.get(name) {
            Some(Origin::Path(dir)) => relative_to(&self.root, dir),
            Some(Origin::Git { url, tag: Some(tag) }) => format!("{}#{}", url, tag),
            Some(Origin::Git { url, tag: None }) => {
                let key = (name.to_string(), version.to_string());
                match self.tags.get(&key) {
                    Some(tag) => format!("{}#{}", url, tag),
                    // Resolved offline from a vendored checkout, so the
                    // spelling of the tag was never seen; ask the checkout.
                    None => match self.checkouts.get(&key).and_then(|dir| describe_tag(dir)) {
                        Some(tag) => format!("{}#{}", url, tag),
                        None => format!("{}#{}", url, version),
                    },
                }
            }
            None => String::new(),
        }
    }
}

impl Registry for Vendor {
    fn versions(&mut self, name: &str) -> Result<Vec<Version>, String> {
        if let Some(known) = self.versions.get(name) {
            return Ok(known.clone());
        }
        let found = match self.origin(name)? {
            Origin::Path(dir) => {
                let manifest = self.load_manifest(name, &dir)?;
                let version = self.declared_version(name, &manifest)?;
                self.checkouts.insert((name.to_string(), version.to_string()), dir);
                vec![version]
            }
            Origin::Git { url, tag: Some(tag) } => vec![self.pinned_version(name, &url, &tag)?],
            Origin::Git { url, tag: None } => {
                if self.offline {
                    let vendored = self.vendored_versions(name);
                    if vendored.is_empty() {
                        return Err(self.not_vendored(name));
                    }
                    vendored
                } else {
                    let listed = tag_versions(&ls_remote(&url)?);
                    if listed.is_empty() {
                        return Err(format!(
                            "`{}` has no version tags at {}\n\nnote: resolution reads `git \
                             ls-remote --tags`; a version is a tag like `v1.2.0` or `1.2.0`, \
                             and a tag that is neither can be pinned with `tag = \"…\"`",
                            name, url
                        ));
                    }
                    let mut found = Vec::new();
                    for (version, tag) in listed {
                        self.tags.insert((name.to_string(), version.to_string()), tag);
                        found.push(version);
                    }
                    found
                }
            }
        };
        self.versions.insert(name.to_string(), found.clone());
        Ok(found)
    }

    fn manifest(&mut self, name: &str, version: &Version) -> Result<Manifest, String> {
        let key = (name.to_string(), version.to_string());
        match self.origin(name)? {
            Origin::Path(dir) => self.load_manifest(name, &dir),
            Origin::Git { url, .. } => {
                let dir = self
                    .checkouts
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| self.candidate_dir(name, &version.to_string()));
                if !dir.exists() {
                    if self.offline {
                        return Err(self.not_vendored(name));
                    }
                    let tag = self
                        .tags
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| version.to_string());
                    clone(&url, Some(&tag), &dir)?;
                }
                self.checkouts.insert(key.clone(), dir.clone());
                let manifest = self.load_manifest(name, &dir)?;
                let declared = self.declared_version(name, &manifest)?;
                if declared != *version {
                    let tag = self.tags.get(&key).cloned().unwrap_or_else(|| version.to_string());
                    return Err(format!(
                        "tag `{}` of `{}` contains a manifest declaring {} — the tag and the \
                         manifest disagree about the version",
                        tag, name, declared
                    ));
                }
                Ok(manifest)
            }
        }
    }
}

/// `dir`, written the way a manifest would write it: relative to the root
/// package, climbing with `..` where it must. A lockfile is committed, and an
/// absolute path would make it differ per machine. Both paths arrive
/// canonicalised, so their components compare honestly.
fn relative_to(root: &Path, dir: &Path) -> String {
    let root_parts: Vec<_> = root.components().collect();
    let dir_parts: Vec<_> = dir.components().collect();
    let mut shared = 0;
    while shared < root_parts.len().min(dir_parts.len())
        && root_parts[shared] == dir_parts[shared]
    {
        shared += 1;
    }
    if shared == 0 {
        // Nothing in common — a different drive. Absolute is the truth then.
        return dir.display().to_string();
    }
    // Joined with `/` rather than by the platform's separator. A lockfile is
    // committed, and `../shared` on one machine and `..\shared` on another is
    // the same file disagreeing with itself — which is the whole thing writing
    // a relative path was meant to avoid. Every target reads `/` in a manifest,
    // so this is what a person would have written too.
    let mut parts: Vec<String> = Vec::new();
    for _ in shared..root_parts.len() {
        parts.push("..".to_string());
    }
    for part in &dir_parts[shared..] {
        parts.push(part.as_os_str().to_string_lossy().to_string());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `v1.2.3` and `1.2.3` both spell a version; anything else is just a tag.
fn version_from_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The versions `git ls-remote --tags` shows, with the tag each was spelled
/// as. Peeled refs (`…^{}`) repeat the tag they peel and are skipped, as is
/// any tag that does not spell a version.
fn tag_versions(output: &str) -> Vec<(Version, String)> {
    let mut found: Vec<(Version, String)> = Vec::new();
    for line in output.lines() {
        let Some((_, reference)) = line.split_once('\t') else { continue };
        let Some(tag) = reference.trim().strip_prefix("refs/tags/") else { continue };
        if tag.ends_with("^{}") {
            continue;
        }
        if let Some(version) = version_from_tag(tag) {
            if !found.iter().any(|(v, _)| *v == version) {
                found.push((version, tag.to_string()));
            }
        }
    }
    found
}

/// The one network read resolution performs, and only for a git dependency
/// whose version is left to be chosen.
fn ls_remote(url: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", url])
        .output()
        .map_err(|e| format!("cannot run `git`: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "listing tags of {} failed:\n{}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// What tag a checkout was cloned from — a local question, answerable under
/// `--offline`.
fn describe_tag(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(["describe", "--tags", "--exact-match"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tag = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tag.is_empty() { None } else { Some(tag) }
}

/// Whether the placed checkout already declares the chosen version.
fn is_version(dir: &Path, version: &Version) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("kite.toml")) else { return false };
    let Ok(parsed) = manifest::parse(&text) else { return false };
    Version::parse(&parsed.version).is_ok_and(|declared| declared == *version)
}

/// Copy a checkout into place, leaving `.git` behind: the build wants the
/// files, and the history is the candidate directory's to keep.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_remote_output_reads_as_versions_and_their_spellings() {
        let output = "abc1\trefs/tags/v1.2.0\n\
                      abc2\trefs/tags/v1.2.0^{}\n\
                      abc3\trefs/tags/2.0.0\n\
                      abc4\trefs/tags/release-final\n\
                      abc5\trefs/tags/v2.1.0-rc.1\n\
                      abc6\trefs/heads/main\n";
        let found = tag_versions(output);
        assert_eq!(
            found,
            vec![
                (Version::parse("1.2.0").unwrap(), "v1.2.0".to_string()),
                (Version::parse("2.0.0").unwrap(), "2.0.0".to_string()),
                (Version::parse("2.1.0-rc.1").unwrap(), "v2.1.0-rc.1".to_string()),
            ]
        );
    }

    // ---- path-only resolution, end to end, with no network ----------------

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kite-pkg-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create");
        dir
    }

    fn write(path: PathBuf, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create");
        }
        std::fs::write(path, text).expect("write");
    }

    fn package(dir: &Path, name: &str, version: &str, dependencies: &str) {
        write(
            dir.join(name).join("kite.toml"),
            &format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\n\n[dependencies]\n{}\n",
                name, version, dependencies
            ),
        );
        write(dir.join(name).join("src/lib.kite"), &format!("fn {}() {{\n}}\n", name));
    }

    #[test]
    fn path_dependencies_resolve_transitively_and_lock_with_versions() {
        let dir = fixture("resolves");
        package(
            &dir,
            "app",
            "0.1.0",
            "a = { path = \"../a\", version = \"^1\" }\nb = { path = \"../b\" }",
        );
        package(&dir, "a", "1.2.0", "shared = { path = \"../shared\", version = \">=1.0\" }");
        package(&dir, "b", "0.3.0", "shared = { path = \"../shared\", version = \"<2.0\" }");
        package(&dir, "shared", "1.4.0", "");

        let root_dir = dir.join("app");
        let text = std::fs::read_to_string(root_dir.join("kite.toml")).expect("read");
        let root = manifest::parse(&text).expect("parses");
        // Paths never fetch, so `--offline` resolves them too.
        let locked = lock_dependencies(&root, &root_dir, true).expect("resolves");

        let summary: Vec<(String, String)> =
            locked.iter().map(|l| (l.name.clone(), l.version.clone())).collect();
        assert_eq!(
            summary,
            vec![
                ("a".to_string(), "1.2.0".to_string()),
                ("b".to_string(), "0.3.0".to_string()),
                ("shared".to_string(), "1.4.0".to_string()),
            ]
        );
        let lock = manifest::lockfile(&locked);
        assert!(lock.contains("version = \"1.4.0\""), "{}", lock);
        // A committed file must read the same on every machine: paths are
        // written relative to the root package, not absolute.
        assert!(lock.contains("source = \"../shared\""), "{}", lock);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transitive_conflict_names_both_requirers() {
        let dir = fixture("conflicts");
        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }\nb = { path = \"../b\" }");
        package(&dir, "a", "1.2.0", "shared = { path = \"../shared\", version = \">=2.0\" }");
        package(&dir, "b", "0.3.0", "shared = { path = \"../shared\", version = \"<2.0\" }");
        package(&dir, "shared", "1.4.0", "");

        let root_dir = dir.join("app");
        let text = std::fs::read_to_string(root_dir.join("kite.toml")).expect("read");
        let root = manifest::parse(&text).expect("parses");
        let err = lock_dependencies(&root, &root_dir, true).expect_err("conflicts");

        assert!(err.contains("no version of `shared`"), "{}", err);
        assert!(err.contains("a 1.2.0"), "{}", err);
        assert!(err.contains("b 0.3.0"), "{}", err);
        assert!(err.contains(">=2.0.0"), "{}", err);
        assert!(err.contains("<2.0.0"), "{}", err);
        assert!(err.contains("1.4.0"), "{}", err);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_name_from_two_places_is_refused() {
        let dir = fixture("two-places");
        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }\nb = { path = \"../b\" }");
        package(&dir, "a", "1.0.0", "shared = { path = \"../s1\" }");
        package(&dir, "b", "1.0.0", "shared = { path = \"../s2\" }");
        package(&dir, "s1", "1.0.0", "");
        package(&dir, "s2", "1.0.0", "");
        // `shared` must point at s1 for a and s2 for b — which is two things.
        write(dir.join("s1/kite.toml"), "[package]\nname = \"shared\"\nversion = \"1.0.0\"\n");
        write(dir.join("s2/kite.toml"), "[package]\nname = \"shared\"\nversion = \"1.0.0\"\n");

        let root_dir = dir.join("app");
        let text = std::fs::read_to_string(root_dir.join("kite.toml")).expect("read");
        let root = manifest::parse(&text).expect("parses");
        let err = lock_dependencies(&root, &root_dir, true).expect_err("two places");
        assert!(err.contains("two different places"), "{}", err);
        assert!(err.contains("a name means one thing"), "{}", err);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dependency_without_a_version_cannot_be_ordered() {
        let dir = fixture("versionless");
        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }");
        write(dir.join("a/kite.toml"), "[package]\nname = \"a\"\n");

        let root_dir = dir.join("app");
        let text = std::fs::read_to_string(root_dir.join("kite.toml")).expect("read");
        let root = manifest::parse(&text).expect("parses");
        let err = lock_dependencies(&root, &root_dir, true).expect_err("no version");
        assert!(err.contains("declares no version"), "{}", err);
        assert!(err.contains("version = \"0.1.0\""), "{}", err);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
