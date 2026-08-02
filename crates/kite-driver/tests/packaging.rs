//! The release's artefacts, and the four files that have to agree about them.
//!
//! A release produces archives named after a target triple. Five other files
//! name those same triples: the install script, the Homebrew formula, the
//! Scoop manifest, the AUR `PKGBUILD`, and the script that fills the last
//! three in. Nothing forces them to agree, and a packaging manifest that names
//! an archive no release produces fails at the worst possible moment — in
//! somebody else's install, long after the change that broke it.
//!
//! So the agreement is checked here. This is the same argument the brand-asset
//! drift test makes: a copy nobody checks is a copy that drifts.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

/// The targets the release workflow actually builds, read from its matrix.
fn released_targets() -> Vec<String> {
    let workflow = read(".github/workflows/release.yml");
    let mut out = Vec::new();
    for line in workflow.lines() {
        let Some(rest) = line.split("target:").nth(1) else {
            continue;
        };
        let target = rest.trim().trim_end_matches('}').trim();
        // The `target:` in the toolchain step is an interpolation, not a name.
        if target.starts_with("${{") || target.is_empty() {
            continue;
        }
        out.push(target.to_string());
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn the_release_builds_the_five_targets_it_claims_to() {
    assert_eq!(
        released_targets(),
        vec![
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-musl",
        ],
        "the release matrix changed; the packaging manifests below name these \
         triples and have to change with it"
    );
}

#[test]
fn every_released_target_can_be_installed() {
    let script = read("install.sh");
    // The script builds a triple from `uname`, so what it must contain is the
    // two halves it joins, not the joined form.
    for target in released_targets() {
        if target.contains("windows") {
            // `install.sh` is a POSIX shell script and says so: Windows is
            // Scoop's job, and the script refuses rather than guessing.
            continue;
        }
        let (arch, os) = target.split_once('-').expect("a triple has an arch");
        assert!(
            script.contains(arch),
            "install.sh never mentions `{}`, so it cannot install {}",
            arch,
            target
        );
        assert!(
            script.contains(os),
            "install.sh never mentions `{}`, so it cannot install {}",
            os,
            target
        );
    }
}

#[test]
fn the_homebrew_formula_names_every_unix_archive() {
    let formula = read("packaging/homebrew/kite.rb");
    for target in released_targets() {
        if target.contains("windows") {
            continue;
        }
        assert!(
            formula.contains(&format!("{}.tar.gz", target)),
            "the formula has no archive for {}",
            target
        );
    }
    assert!(
        !formula.contains("windows"),
        "Homebrew does not install Windows binaries; Scoop does"
    );
}

#[test]
fn the_scoop_manifest_names_the_windows_archive() {
    let manifest = read("packaging/scoop/kite.json");
    assert!(
        manifest.contains("x86_64-pc-windows-msvc.zip"),
        "the Scoop manifest has no Windows archive"
    );
    // Scoop takes the hash from the release's own checksum file rather than
    // from a download it cannot vouch for.
    assert!(
        manifest.contains("SHA256SUMS"),
        "autoupdate should read the release's checksum file"
    );
}

#[test]
fn the_pkgbuild_names_both_linux_archives() {
    let pkgbuild = read("packaging/aur/PKGBUILD");
    for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
        assert!(
            pkgbuild.contains(target),
            "the PKGBUILD has no source for {}",
            target
        );
    }
}

/// The manifests are checked in with placeholder checksums so they can be read
/// and reviewed. `render.sh` replaces those placeholders, so a manifest that
/// stopped carrying one would silently stop being rendered.
#[test]
fn every_manifest_still_carries_the_placeholder_render_replaces() {
    const PLACEHOLDER: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";
    for file in [
        "packaging/homebrew/kite.rb",
        "packaging/scoop/kite.json",
        "packaging/aur/PKGBUILD",
    ] {
        assert!(
            read(file).contains(PLACEHOLDER),
            "{} has no placeholder checksum for `render.sh` to replace — if a \
             real one was committed, that is a checksum nobody can verify",
            file
        );
    }
    let render = read("packaging/render.sh");
    for target in released_targets() {
        let suffix = if target.contains("windows") { "zip" } else { "tar.gz" };
        assert!(
            render.contains(&format!("{}.{}", target, suffix)),
            "render.sh never looks up a checksum for {}",
            target
        );
    }
}

/// Signing is the part of Phase 15 that was missing, and it has to be reachable
/// from both ends: the workflow signs, and the installer checks.
#[test]
fn the_release_is_signed_and_the_installer_checks_it() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains("id-token: write"),
        "Sigstore's keyless signing needs the workflow's OIDC token"
    );
    assert!(
        workflow.contains("cosign sign-blob"),
        "nothing in the release signs anything"
    );
    assert!(
        workflow.contains("SHA256SUMS.sigstore.json"),
        "the signature bundle is not attached to the release"
    );
    let script = read("install.sh");
    assert!(
        script.contains("cosign verify-blob"),
        "install.sh downloads a signed release and never checks the signature"
    );
}

/// The playground is the compiler; the release should hand out the same module
/// rather than making anyone build the site to get it.
#[test]
fn the_release_attaches_the_compiler_as_webassembly() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains("kite_playground.wasm"),
        "the release does not build `kitec` for WebAssembly"
    );
    assert!(
        workflow.contains("dist/*.wasm"),
        "the module is built and then not attached"
    );
}
