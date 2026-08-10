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

/// The licence is stated in five places, and they have to agree.
///
/// A project whose manifests declare a licence it does not carry is one a
/// packager has to guess about, and Linguist will not vendor a grammar whose
/// repository does not state one at all.
#[test]
fn every_manifest_declares_the_licence_the_repository_carries() {
    let licence = read("LICENSE");
    assert!(
        licence.starts_with("MIT License"),
        "the repository's own LICENSE is not MIT"
    );
    assert!(
        read("Cargo.toml").contains("license = \"MIT\""),
        "the workspace declares a different licence from the one in LICENSE"
    );
    assert!(
        read("packaging/homebrew/kite.rb").contains("license \"MIT\""),
        "the formula declares a different licence"
    );
    assert!(
        read("packaging/scoop/kite.json").contains("\"license\": \"MIT\""),
        "the Scoop manifest declares a different licence"
    );
    assert!(
        read("packaging/aur/PKGBUILD").contains("license=('MIT')"),
        "the PKGBUILD declares a different licence"
    );
    // And it has to reach the people who download a build, not just the people
    // who read the repository.
    assert!(
        read(".github/workflows/release.yml").contains("SPECIFICATION.md LICENSE"),
        "the release archives do not carry the licence"
    );
}

/// The Linguist submission names real files.
///
/// `assemble.sh` copies samples out of the library and the examples at
/// submission time rather than keeping a second copy, which is the right call
/// — and it means a renamed example turns the submission into a broken script
/// at exactly the wrong moment.
#[test]
fn the_linguist_samples_all_exist() {
    let script = read("packaging/linguist/assemble.sh");
    let mut found = 0;
    for line in script.lines() {
        let Some(rest) = line.trim().strip_prefix('"') else { continue };
        let Some((path, _)) = rest.split_once('"') else { continue };
        if !path.ends_with(".kite") {
            continue;
        }
        assert!(
            repo().join(path).exists(),
            "assemble.sh offers `{}` as a Linguist sample and it does not exist",
            path
        );
        found += 1;
    }
    assert!(found >= 5, "only {} samples are named", found);
    // Linguist says outright that tutorial examples will not be accepted. The
    // check reads the sample list rather than the file, because the file
    // *explains* why `hello.kite` is absent and would otherwise fail for
    // saying so.
    let listed: Vec<&str> = script
        .lines()
        .skip_while(|l| !l.contains("samples=("))
        .take_while(|l| !l.trim().starts_with(')'))
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split_once('"').map(|(p, _)| p))
        .collect();
    assert!(
        !listed.iter().any(|p| p.ends_with("hello.kite")),
        "`hello.kite` is a tutorial example; Linguist rejects those"
    );
}

/// The grammar Linguist would vendor is the one the editor ships, and the
/// `languages.yml` entry has to name its scope or highlighting silently does
/// nothing.
#[test]
fn the_languages_entry_matches_the_grammar() {
    let grammar = read("editors/vscode/syntaxes/kite.tmLanguage.json");
    let entry = read("packaging/linguist/languages.yml.fragment");
    assert!(
        grammar.contains("\"scopeName\": \"source.kite\""),
        "the grammar's scope name changed"
    );
    assert!(
        entry.contains("tm_scope: source.kite"),
        "the Linguist entry names a scope the grammar does not define"
    );
    assert!(
        entry.contains("- \".kite\""),
        "the Linguist entry does not claim the extension"
    );
    // Read as data, not as text: the fragment's own comment explains why
    // `language_id` is absent, and a check on the raw file would fail for the
    // explanation.
    assert!(
        !entry
            .lines()
            .any(|l| l.trim_start().starts_with("language_id")),
        "`language_id` is Linguist's to allocate with `script/update-ids`"
    );
}

/// Every version in the tree is `0.1.N`, and stays there.
///
/// Kite's numbering does not climb: there is no 0.2, no 1.0, and no plan for
/// one. The patch number goes up — 0.1.1, 0.1.2, … 0.1.26 — and the first two
/// components never move.
///
/// The reason is the promise the language makes rather than modesty about it.
/// A major number is a licence to break things and an invitation to be asked
/// when the next one lands; a minor number implies a feature line that will be
/// superseded. Kite intends neither. A version here says only *which build*,
/// which is the only question a version has to answer once the language has
/// stopped moving.
///
/// The VS Code extension had drifted to 0.2.0 on its own, which is exactly the
/// drift a rule nobody checks invites.
#[test]
fn every_version_stays_on_the_one_line() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|e| panic!("read {}: {}", path, e))
    };

    // `version = "0.1.N"` in the workspace manifest, and `"version": "0.1.N"`
    // in each package.
    let mut found: Vec<(&str, String)> = Vec::new();
    let cargo = read("Cargo.toml");
    let line = cargo
        .lines()
        .find(|l| l.trim_start().starts_with("version = \""))
        .expect("the workspace declares a version");
    found.push(("Cargo.toml", line.split('"').nth(1).unwrap().to_string()));

    for path in [
        "packages/kite-cli/package.json",
        "packages/kite-wasm/package.json",
        "packages/vite-plugin-kite/package.json",
        "editors/vscode/package.json",
    ] {
        let text = read(path);
        let at = text.find("\"version\"").expect("a version");
        let value: String = text[at..]
            .split('"')
            .nth(3)
            .expect("a version string")
            .to_string();
        found.push((path, value));
    }

    // And they agree with each other, because numbers that drift apart are
    // numbers nobody can use to say what they are running.
    let first = found[0].1.clone();
    for (path, version) in &found {
        assert_eq!(
            *version, first,
            "{} is {} and Cargo.toml is {} — they name one build",
            path, version, first
        );
    }

    // Everywhere *else* the number is written down.
    //
    // The five above are the manifests, and they were the whole of this test
    // for a while — so the pins inside them and every mention in the prose
    // drifted with nothing watching. The release before this one needed a
    // commit of its own to catch up (`release: point the starter and the
    // install page at 0.1.3`), which is the shape of a rule nobody checks.
    //
    // The rule for these files is blunt on purpose: **no version but the
    // current one may appear in them at all**. None has any reason to name
    // another, and a blunt rule is one nobody has to remember the shape of.
    // `RELEASING.md` is deliberately not here — it shows `git tag v0.1.0` as
    // an example, which is the one place naming an old version is right.
    for path in [
        // The pins, which must not resolve to a build they were never tested
        // against — the reason they are exact rather than a range.
        "packages/kite-cli/package.json",
        "packages/vite-plugin-kite/package.json",
        "examples/vite-starter/package.json",
        // The prose, which says what the current release is.
        "README.md",
        "site/install.md",
        "site/index.html",
        "site/brand.html",
    ] {
        let text = read(path);
        let mut seen = 0;
        for (at, _) in text.match_indices("0.1.") {
            let rest = &text[at + 4..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            seen += 1;
            let named = format!("0.1.{}", digits);
            assert_eq!(
                named, first,
                "{} names {} and Cargo.toml is {} — a release moves every one of \
                 these together, and the ones nothing checks are the ones that \
                 get left behind",
                path, named, first
            );
        }
        assert!(
            seen > 0,
            "{} names no version at all — it used to, so either the reference \
             moved or this list is stale",
            path
        );
    }
}

/// Every link in `site/llms.txt` reaches something the site actually serves.
///
/// `llms.txt` is written by hand, because it is an index and an index wants
/// prose. Everything it points at is produced by `site/build.sh`, and nothing
/// connects the two — so a document renamed, a reference page added, or a
/// section of the site retired leaves a link that resolves to a 404 for
/// exactly the reader least able to recover from one. An agent that fetches a
/// missing page does not go looking; it proceeds without it.
///
/// The mapping below is from a URL the file names to the repository file whose
/// existence makes it servable, which is the honest thing to assert without a
/// built site to look at.
#[test]
fn every_link_in_llms_txt_reaches_something_the_site_serves() {
    let text = read("site/llms.txt");

    const LINK: &str = "](https://kite-lang.dev/";
    let mut checked = 0;
    // `match_indices` yields the *match*, not what follows it — reading the
    // URL off the match gave "" for every link, so every one of them resolved
    // to `index.html` and the test passed on a site with no documents at all.
    for (at, _) in text.match_indices(LINK) {
        let url = text[at + LINK.len()..].split(')').next().expect("a closed link");
        checked += 1;

        // Where `site/build.sh` gets each thing from. A directory URL is
        // served by the index the build writes into it.
        let source = match url {
            "" | "playground.html" | "reference.html" | "install.md" => url.to_string(),
            "llms-full.txt" => "skills/kite/SKILL.md".to_string(),
            "skill/SKILL.md" => "skills/kite/SKILL.md".to_string(),
            // Both indexes are generated by `site/build.sh`; a static asset
            // host has no directory listing, so `llms.txt` names them.
            "skill/references/index.md" => "skills/kite/references".to_string(),
            "docs/index.md" => "docs".to_string(),
            u if u.starts_with("docs/") => u.to_string(),
            u => u.to_string(),
        };
        let at = repo().join(if source.is_empty() { "site/index.html" } else { &source });
        let also = repo().join("site").join(&source);
        assert!(
            at.exists() || also.exists(),
            "site/llms.txt links to `{}`, and neither {} nor {} exists — the \
             build has nothing to serve there",
            url,
            at.display(),
            also.display()
        );
    }

    assert!(checked >= 8, "only {} links found; llms.txt used to have more", checked);

    // The claim the whole file rests on. If the package name or the
    // subcommand ever moves, the one instruction an agent will actually run
    // stops working.
    assert!(
        text.contains("npx --yes @kite-lang/compiler-wasm kitec check"),
        "llms.txt no longer tells an agent how to check its own work"
    );
}
