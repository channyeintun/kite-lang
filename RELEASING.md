# Releasing Kite

Everything that has to happen for a version to exist, in the order it has to
happen in. Each step says what breaks if it is skipped, because most of these
fail quietly rather than loudly.

There are **six** things that carry a version, and they are not published by
the same mechanism:

| What | Where it goes | By |
|---|---|---|
| `kitec`, `kite-lsp` | GitHub release, signed | CI, on a tag |
| `@kite-lang/cli-*` | npm, one per platform | by hand, from the release's binaries |
| `@kite-lang/cli` | npm, the meta-package | by hand, **after** the platform ones |
| `@kite-lang/compiler-wasm` | npm, one for every platform | by hand, after `build.sh` |
| The site | kite-lang.dev | `wrangler deploy` |
| The VS Code extension | Marketplace | `vsce publish` |

---

## 1. Before anything

```bash
cargo test --workspace --all-targets     # 798, and all of them
cargo clippy --workspace --all-targets -- -D warnings
for f in $(git ls-files '*.kite'); do ./target/release/kitec fmt --check "$f"; done
```

CI runs all three, and the last one is the reason a formatter exists.

**Check the numbers in the prose.** The README states a test count, and
`crates/kite-driver/tests/size.rs` records what each program costs *today* in a
comment beside its budget. Both drift silently, because nothing compares a
sentence to a measurement:

```bash
cargo test -p kite-driver --test size -- --nocapture
```

## 2. The version

**It is always `0.1.N`.** There is no 0.2, no 1.0, and no plan for one. The
patch number goes up — 0.1.1, 0.1.2, … 0.1.26 — and the first two components
never move. `every_version_stays_on_the_one_line` in
`crates/kite-driver/tests/packaging.rs` fails the build if one of them does, and
if the four numbers stop agreeing with each other.

That is about the promise rather than modesty about it. A major number is a
licence to break things and an invitation to be asked when the next one lands;
a minor number implies a feature line that something later supersedes. Kite
intends neither. Once the language has stopped moving, the only question a
version has to answer is *which build*, and one climbing number answers it.

One number, in five places:

```
Cargo.toml                               [workspace.package] version
packages/kite-cli/package.json           version, and every optionalDependency
packages/kite-wasm/package.json          version
packages/vite-plugin-kite/package.json   version, and its compiler-wasm dependency
editors/vscode/package.json              version
```

`vite-plugin-kite` depends on `@kite-lang/compiler-wasm`, so the two move
together. A plugin resolving to an older compiler than the one it was tested
against is the same hazard the pinned `optionalDependencies` below avoid.

The `optionalDependencies` in `@kite-lang/cli` pin **exact** versions of the
platform packages — not a range. A range would let a meta-package resolve to a
compiler it was never tested against.

Commit it, then:

```bash
git push origin main
git tag v0.1.0 && git push origin v0.1.0
```

**Push before tagging.** A tag whose commits are not on the remote makes CI
build something nobody can check out.

## 3. The binaries, from CI

The tag starts `.github/workflows/release.yml`, which builds five targets on
their own runners, checksums them, and signs `SHA256SUMS` with Sigstore —
keyless, so the workflow's own identity is the signature and there is no key to
lose.

Wait for it. Then download the release's artefacts into a directory.

> **Do not publish binaries built on your machine.** `build.sh --cross` really
> does make all five — `cargo zigbuild` supplies the linker for the musl and
> Windows targets, so this is a working cross-build rather than an
> approximation of one. It is for trying the packaging, not for shipping, and
> for two reasons that have nothing to do with how well it builds.
>
> They are not signed. The Sigstore signature is the release workflow's own
> identity, and a binary built here cannot have one.
>
> And the Windows target is not the same target. `cargo zigbuild` links through
> `zig cc`, which implements the GNU ABI for Windows, so `build.sh --cross`
> produces `x86_64-pc-windows-gnu` while the release builds
> `x86_64-pc-windows-msvc` on a Windows runner. That is a property of the
> toolchain rather than a setting to change: zig has no MSVC ABI to target.
> The two differ in C runtime and in unwinding, so the one you can test here is
> not the one users install.

## 4. npm

```bash
./packages/kite-cli/build.sh path/to/downloaded/release/
for d in packages/kite-cli/platforms/*/; do (cd "$d" && npm publish); done
npm publish ./packages/kite-cli

# The compiler as WebAssembly: one module, no platform matrix. `build.sh`
# writes the `.wasm`, which is a build artefact and not in the tree — publish
# without running it and the package ships without a compiler in it.
./packages/kite-wasm/build.sh
npm publish ./packages/kite-wasm

# The plugin is unscoped and published under the user rather than the org, so
# it is easy to forget — and a plugin left behind resolves a compiler it was
# never tested against, which is what the pinned dependency exists to prevent.
npm publish ./packages/vite-plugin-kite
```

Four things here have each cost an attempt:

**The path needs a `./`.** `npm publish packages/kite-cli` does not publish that
directory: npm reads a bare `owner/name` as a GitHub shorthand and goes looking
for `github.com/packages/kite-cli`, which fails with `code 128` and
`Repository not found` — an error that says nothing about the real mistake. A
leading `./` makes it a path again.

**The platform packages go first.** npm resolves `optionalDependencies` at
install time and *skips a missing one without a word*. A meta-package published
first installs perfectly cleanly and then cannot find a compiler.

**A scoped package is private by default**, and npm reports that as
`402 Payment Required`, which reads as a billing problem. Every manifest here
carries `publishConfig.access = public` so the flag cannot be forgotten.

**The scope has to exist.** `@kite-lang` is an npm organisation; publishing into
one that does not exist gives `404 Scope not found`.

Then check it the way a stranger would — from the registry, not from a local
path, and with no `kitec` on the `PATH`:

```bash
cd $(mktemp -d) && npm init -y && npm install --save-dev @kite-lang/cli
./node_modules/.bin/kitec --version
```

And the WebAssembly compiler the same way, which is the one a bundler resolves.
Check it *builds* rather than that it exists: the `.wasm` is added by
`build.sh`, so a package published without running it installs cleanly and
then fails at the first `.kite` import.

```bash
cd $(mktemp -d) && npm init -y && npm install --save-dev @kite-lang/compiler-wasm
printf 'pub fn add(a: int, b: int) -> int {\n    return a + b\n}\n' > add.kite
./node_modules/.bin/kitec build add.kite --out out && ls out
```

A freshly published package takes a minute or two to become fetchable. A 404
straight after publishing is propagation, not failure.

## 5. The site

```bash
./site/build.sh          # the reference, the pages, the demo, the playground
npx wrangler deploy      # from the repository root, never from inside site/
```

`site/build.sh` regenerates the reference from the library's own doc comments
and renders every document to HTML, so **a change to a `///` comment is not on
the site until this runs**. Deploying from inside `site/` publishes wrangler's
own state directory; `.assetsignore` is the second line of defence.

Verify against the deployed bytes rather than the browser, which caches:

```bash
curl -s "https://kite-lang.dev/SPECIFICATION.md?v=$RANDOM" | diff - SPECIFICATION.md
```

## 6. The VS Code extension

```bash
./editors/vscode/render-icon.sh                 # only if the mark changed
cd editors/vscode && npx @vscode/vsce publish
```

The Marketplace will not take an SVG, so `icon.png` is a rendering that has to
exist as a file; `brand_assets.rs` fails if it is missing or has drifted from
`site/kite-mark.svg`.

`editors/vscode/PUBLISHING.md` is the long version, and it is worth reading
before the first publish rather than after: the Marketplace runs on Azure
DevOps identity, so `vsce` answers a mis-scoped token with an error code that
names neither, and the publisher — whose ID is permanent and must match
`package.json` — can only be created in a browser. There is also a web upload
route that needs no token at all.

## 7. Afterwards

- `install.sh` needs no change: it reads `releases/latest` and the release's own
  `SHA256SUMS`.
- Homebrew, Scoop and the AUR manifests keep placeholder checksums in the tree
  so they stay reviewable. `packaging/render.sh <version>` fills them in from
  the release's own `SHA256SUMS`, which is the only place they should come
  from.

---

## What a patch release skips

A fix that touches neither the compiler nor the library — a page, a document, a
README — needs only step 5. It does not need a tag, and it should not get one:
a version that names no binary change is a version somebody will try to install.
