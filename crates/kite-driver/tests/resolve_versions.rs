//! Version resolution over real manifests on disk.
//!
//! The registry here is the simplest one that exists: directories of path
//! dependencies, each offering exactly the version its own `kite.toml`
//! declares. It touches no network, which is the point — the solver is a
//! function of what exists, and these tests pin that down with nothing but a
//! temp directory. (`kitec pkg` layers git on the same trait; its own tests
//! live beside it.)

use kite_driver::manifest::{self, Manifest, Source};
use kite_driver::semver::Version;
use kite_driver::solve::{self, Registry, Resolved};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Directories as a registry: each name is one directory, and each directory
/// is one version. Loading a manifest teaches it where that manifest's own
/// dependencies live, exactly as `kitec pkg` learns them.
struct Dirs {
    known: BTreeMap<String, PathBuf>,
}

impl Dirs {
    fn load(&mut self, name: &str) -> Result<Manifest, String> {
        let dir = self
            .known
            .get(name)
            .cloned()
            .ok_or_else(|| format!("nothing says where `{}` comes from", name))?;
        let text = std::fs::read_to_string(dir.join("kite.toml"))
            .map_err(|e| format!("`{}`: {}", name, e))?;
        let parsed = manifest::parse(&text).map_err(|e| format!("`{}`: {}", name, e))?;
        for dep in &parsed.dependencies {
            if let Source::Path(path) = &dep.source {
                self.known.entry(dep.name.clone()).or_insert_with(|| dir.join(path));
            }
        }
        Ok(parsed)
    }
}

impl Registry for Dirs {
    fn versions(&mut self, name: &str) -> Result<Vec<Version>, String> {
        let manifest = self.load(name)?;
        Ok(vec![Version::parse(&manifest.version).map_err(|e| format!("`{}`: {}", name, e))?])
    }

    fn manifest(&mut self, name: &str, _version: &Version) -> Result<Manifest, String> {
        self.load(name)
    }
}

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-solve-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    dir
}

fn package(dir: &Path, name: &str, version: &str, dependencies: &str) {
    let package_dir = dir.join(name);
    std::fs::create_dir_all(&package_dir).expect("create");
    std::fs::write(
        package_dir.join("kite.toml"),
        format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\n\n[dependencies]\n{}\n",
            name, version, dependencies
        ),
    )
    .expect("write");
}

fn resolve_in(dir: &Path) -> Result<Vec<Resolved>, String> {
    let text = std::fs::read_to_string(dir.join("app/kite.toml")).expect("read");
    let root = manifest::parse(&text).expect("parses");
    let mut registry = Dirs { known: BTreeMap::new() };
    for dep in &root.dependencies {
        if let Source::Path(path) = &dep.source {
            registry.known.insert(dep.name.clone(), dir.join("app").join(path));
        }
    }
    solve::resolve(&root, &mut registry)
}

#[test]
fn a_diamond_of_path_dependencies_resolves_to_one_shared_version() {
    let dir = fixture("diamond");
    package(
        &dir,
        "app",
        "0.1.0",
        "a = { path = \"../a\", version = \"^1\" }\nb = { path = \"../b\" }",
    );
    package(&dir, "a", "1.2.0", "shared = { path = \"../shared\", version = \">=1.0\" }");
    package(&dir, "b", "0.3.0", "shared = { path = \"../shared\", version = \"<2.0\" }");
    package(&dir, "shared", "1.4.0", "");

    let resolved = resolve_in(&dir).expect("resolves");
    assert_eq!(
        resolved,
        vec![
            Resolved { name: "a".into(), version: Version::parse("1.2.0").unwrap() },
            Resolved { name: "b".into(), version: Version::parse("0.3.0").unwrap() },
            Resolved { name: "shared".into(), version: Version::parse("1.4.0").unwrap() },
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conflict_between_manifests_on_disk_names_them_both() {
    let dir = fixture("conflict");
    package(&dir, "app", "0.1.0", "a = { path = \"../a\" }\nb = { path = \"../b\" }");
    package(&dir, "a", "1.2.0", "shared = { path = \"../shared\", version = \">=2.0\" }");
    package(&dir, "b", "0.3.0", "shared = { path = \"../shared\", version = \"<2.0\" }");
    package(&dir, "shared", "1.4.0", "");

    let err = resolve_in(&dir).expect_err("conflicts");
    assert!(err.contains("no version of `shared` satisfies everyone"), "{}", err);
    assert!(err.contains("a 1.2.0"), "{}", err);
    assert!(err.contains("b 0.3.0"), "{}", err);
    assert!(err.contains(">=2.0.0"), "{}", err);
    assert!(err.contains("<2.0.0"), "{}", err);
    assert!(err.contains("the versions that exist: 1.4.0"), "{}", err);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_requirement_on_a_pinned_path_is_checked_not_assumed() {
    let dir = fixture("pinned");
    package(&dir, "app", "0.1.0", "a = { path = \"../a\", version = \"^2\" }");
    package(&dir, "a", "1.2.0", "");

    let err = resolve_in(&dir).expect_err("conflicts");
    assert!(err.contains("app 0.1.0"), "{}", err);
    assert!(err.contains("^2.0.0"), "{}", err);
    assert!(err.contains("1.2.0"), "{}", err);

    let _ = std::fs::remove_dir_all(&dir);
}
