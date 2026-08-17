# Releasing Kite

Everything that has to happen for a version to exist, in the order it has to
happen in. Each step says what breaks if it is skipped, because most of these
fail quietly rather than loudly.

This is the runbook as it was actually walked for 0.1.9, corrected where the
previous version of this document was wrong about its own process.

**Ten artefacts carry a version**, published four different ways:

| What | Where it goes | By |
|---|---|---|
| `kitec`, `kite-lsp` | GitHub release, signed | CI, on a tag |
| `@kite-lang/cli-*` — five of them | npm, one per platform | by hand, from the release's binaries |
| `@kite-lang/cli` | npm, the meta-package | by hand, **after** the platform ones |
| `@kite-lang/compiler-wasm` | npm | by hand, after `build.sh` |
| `vite-plugin-kite` | npm, unscoped | by hand, with the compiler |
| The VS Code extension | Marketplace | a `.vsix`, uploaded |
| The site | kite-lang.dev | `wrangler deploy` |

---

## 1. Before anything

Three gates, and CI runs all three:

```bash
cargo test --workspace --all-targets
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p kite-playground --target wasm32-unknown-unknown -- -D warnings
```

```bash
cargo build --release -p kitec -p kite-lsp
for f in $(git ls-files '*.kite'); do ./target/release/kitec fmt --check "$f"; done
```

The third is the reason a formatter exists. The second is not a formality:
0.1.4 was written, tested and reviewed before clippy found a collapsible `if`
in it, and `-D warnings` means that is a red build rather than a note.

**`-p kite-lsp` is in that line for a reason that has nothing to do with
releasing.** A development machine usually has `kite-lsp` on its `PATH` as a
symlink into `target/release`, so the editor runs whatever was last built
there — and building only `kitec` leaves the language server behind on every
compiler change. It is not a quiet staleness either: a server that predates a
new standard module reports `no standard module 'window'` for a correct `use`,
which puts a confident red squiggle on the user's line and says nothing about
the tooling. It cost an hour of `kitec check` passing while the editor
insisted the same file was broken. Two words in the build line, and the two
cannot drift.

**Check the numbers in the prose.** The README states a test count and
`crates/kite-driver/tests/size.rs` records what each program costs *today* in a
comment beside its budget. Nothing compares a sentence to a measurement, so
both drift:

```bash
cargo test -p kite-driver --test size -- --nocapture
```

The count was 795 in two documents through two releases in which it was not
795.

## 2. The version

**It is always `0.1.N`.** There is no 0.2, no 1.0, and no plan for one. The
patch number goes up — 0.1.1, 0.1.2, … 0.1.26 — and the first two components
never move.

That is about the promise rather than modesty about it. A major number is a
licence to break things and an invitation to be asked when the next one lands;
a minor number implies a feature line that something later supersedes. Kite
intends neither. Once the language has stopped moving, the only question a
version has to answer is *which build*, and one climbing number answers it.

One number, in **ten** files. This document said five for a long time and the
other five drifted behind — the release before 0.1.4 needed a commit of its own
to catch the starter and the install page up, which is what a rule nobody
checks looks like:

```
Cargo.toml                               [workspace.package] version
packages/kite-cli/package.json           version, and all five optionalDependencies
packages/kite-wasm/package.json          version
packages/vite-plugin-kite/package.json   version, and its compiler-wasm dependency
editors/vscode/package.json              version
examples/vite-starter/package.json       both dependencies
README.md                                "The current release is v0.1.N"
site/install.md                          the same sentence
site/index.html                          the release-notes link, twice
site/brand.html                          the version beside the mark
```

`every_version_stays_on_the_one_line` in `crates/kite-driver/tests/packaging.rs`
now checks **all ten** and fails the build if any disagrees. For the five that
are not manifests the rule is blunt: no version but the current one may appear
in those files at all. None has a reason to name another, and a blunt rule is
one nobody has to remember the shape of. This document is exempt, because the
line above counts `0.1.1, 0.1.2, …` to explain the scheme and would fail its
own rule.

The `optionalDependencies` in `@kite-lang/cli` pin **exact** versions, not a
range, and `vite-plugin-kite` pins the compiler it was tested against. A range
would let a package resolve to a build nobody tried it with.

Commit it, then:

```bash
git push origin main
```

```bash
git tag v0.1.9 && git push origin v0.1.9
```

**Push before tagging.** A tag whose commits are not on the remote makes CI
build something nobody can check out.

## 3. The binaries, from CI

The tag starts `.github/workflows/release.yml`, which builds five targets on
their own runners, checksums them, and signs `SHA256SUMS` with Sigstore —
keyless, so the workflow's own identity is the signature and there is no key to
lose.

```bash
gh run watch $(gh run list --workflow=Release --limit 1 --json databaseId --jq '.[0].databaseId')
```

```bash
mkdir -p /tmp/kite-0.1.9
gh release download v0.1.9 --dir /tmp/kite-0.1.9 --pattern '*.tar.gz' --pattern '*.zip'
```

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

## 4. npm, all eight

### The five platform packages

```bash
rm -rf packages/kite-cli/platforms
./packages/kite-cli/build.sh /tmp/kite-0.1.9/
```

**The `rm -rf` is not tidiness.** `platforms/` is a build artefact and is
gitignored, so it survives from whatever was last run there — a `build.sh` with
no arguments leaves one directory for *this* machine, at the *old* version, and
the next run rewrites the ones it has and silently leaves that one alone.
Deleting first is what makes the output the release rather than the release
plus a souvenir.

Then look at it before publishing, because everything below this point is
irreversible:

```bash
ls packages/kite-cli/platforms/
grep -h '"version"' packages/kite-cli/platforms/*/package.json
for d in packages/kite-cli/platforms/*/; do echo "$d: $(ls $d/bin | tr '\n' ' ')"; done
```

Five directories, five identical versions matching `Cargo.toml`, and `kitec`
plus `kite-lsp` in each — `.exe` on the Windows one. A `skipped … (no binary)`
line during the build means an archive was missing from the download, and the
package it belonged to will simply not exist.

```bash
for d in packages/kite-cli/platforms/*/; do (cd "$d" && npm publish); done
```

Re-running that loop is safe: a version already published fails loudly with
`403` rather than overwriting.

### The meta-package, after them

```bash
npm publish ./packages/kite-cli
```

**The order is the whole point.** npm resolves `optionalDependencies` at
install time and *skips a missing one without a word*. A meta-package published
first installs perfectly cleanly and then cannot find a compiler.

### The compiler as WebAssembly

```bash
./packages/kite-wasm/build.sh && npm publish ./packages/kite-wasm
```

The `.wasm` is a build artefact and is not in the tree. Publish without running
`build.sh` and the package installs cleanly and then fails at the first `.kite`
import. The tell is in npm's own output — the tarball listing should show
`kite-compiler.wasm` at about 2.2 MB, and five files in total. The size climbs
with the compiler, so treat it as an order of magnitude rather than a constant —
what matters is that the file is there at all.

### The Vite plugin, with it

```bash
npm publish ./packages/vite-plugin-kite
```

Unscoped and published under the user rather than the org, so it is the easy
one to forget — and a plugin left behind resolves a compiler it was never
tested against, which is what the pinned dependency exists to prevent. When a
release changes the compiler, these two move together or not at all.

### What each failure actually means

Four things have each cost an attempt:

**`code 128` / `Repository not found`.** The path needs a `./`. `npm publish
packages/kite-cli` does not publish that directory: npm reads a bare
`owner/name` as a GitHub shorthand and goes looking for
`github.com/packages/kite-cli`. A leading `./` makes it a path again.

**`402 Payment Required`.** A scoped package is private by default, and npm
reports that as a billing problem. Every manifest here carries
`publishConfig.access = public` — including the generated platform ones — so
the flag cannot be forgotten.

**`404 Scope not found`.** `@kite-lang` is an npm organisation and publishing
into one that does not exist says so. It has to be created in the browser
first.

**`403 cannot publish over previously published version`.** Expected when
re-running a loop that partly succeeded. Not expected otherwise: npm never lets
a version be replaced, so a genuine one means the number did not move.

### Verify as a stranger would

From the registry, not a local path, and with no `kitec` on the `PATH`:

```bash
cd $(mktemp -d) && npm init -y && npm install --save-dev @kite-lang/cli
./node_modules/.bin/kitec --version
```

And the WebAssembly compiler the same way, which is the one a bundler resolves.
Check it *builds* rather than that it exists:

```bash
cd $(mktemp -d) && npm init -y && npm install --save-dev @kite-lang/compiler-wasm
printf 'pub fn add(a: int, b: int) -> int {\n    return a + b\n}\n' > add.kite
./node_modules/.bin/kitec build add.kite --out out && ls out
```

A freshly published package takes a minute or two to become fetchable. A 404
straight after publishing is propagation, not failure.

## 5. The VS Code extension

**Build a `.vsix` and upload it.** Not `vsce publish` — that is the token
route, and it is the wrong one to reach for:

```bash
cd editors/vscode && npx @vscode/vsce package
```

Then *Publisher → New extension → Visual Studio Code* on the Marketplace, and
drop the file in. That is the whole of it, and it needs no Personal Access
Token at all.

The token route exists and `editors/vscode/PUBLISHING.md` documents it, but the
web route is better here for a reason worth stating: the Marketplace runs on
Azure DevOps identity, so `vsce` answers a mis-scoped token with `TF400813` — an
Azure error code, in a tool that never mentions Azure, that reads like a
problem with your account rather than a dropdown chosen wrong. Publisher
problems surface in the interface instead.

Two things that are already handled, so do not go looking for them:

- **The version.** Step 2 moved it, and the test enforces it. Never bump it in
  this directory alone.
- **The icon.** `render-icon.sh` is needed only when the mark itself changes,
  and `brand_assets.rs` fails the suite if `icon.png` is missing or has drifted
  from `site/kite-mark.svg` — so a green step 1 means the icon is current.

The publisher must exist first, with ID exactly `kite-lang`, and **its ID
cannot be changed afterwards**. It is created in the browser; there is no CLI.
`PUBLISHING.md` has the long version, including that the create form fails
silently — clicking *Create* with an invalid field does nothing at all, and the
error sits beside the field several screens up.

```bash
npx @vscode/vsce show kite-lang.kite-lang
```

`not found` in the first few minutes is propagation. The same message an hour
later is not.

## 6. The site

```bash
./site/build.sh
```

```bash
npx wrangler deploy
```

`site/build.sh` regenerates the reference from the library's own doc comments
and renders every document to HTML, so **a change to a `///` comment is not on
the site until this runs**. Deploy from the repository root, never from inside
`site/` — that publishes wrangler's own state directory, and `.assetsignore` is
the second line of defence.

Verify against the deployed bytes rather than the browser, which caches:

```bash
curl -s "https://kite-lang.dev/SPECIFICATION.md?v=$RANDOM" | diff - SPECIFICATION.md
```

## 7. Afterwards

```bash
./packaging/render.sh v0.1.9
```

Homebrew, Scoop and the AUR manifests keep placeholder checksums in the tree so
they stay reviewable; this fills them in from the release's own `SHA256SUMS`,
which is the only place they should come from.

**The `v` is not optional.** The argument is used verbatim as the tag, so a bare
`0.1.9` fetches `releases/download/0.1.9/SHA256SUMS` — a tag that does not
exist — and then fails looking for `kite-0.1.9-…` among archives named
`kite-v0.1.9-…`. This line said `0.1.8` without it for a whole release and
nothing noticed, because CI passes `github.ref_name` and never takes this
path. `install.sh` needs no change —
it reads `releases/latest`.

---

## The three copies of the compiler

Changing the compiler leaves three checked-in WebAssembly builds of it behind,
and one of them fails the suite until it is rebuilt:

- `packages/kite-wasm/kite-compiler.wasm` — what `vite-plugin-kite` depends on.
  `crates/kite-driver/tests/wasm_compiler.rs` builds `examples/vite-starter`
  with the native `kitec` *and* with this module and compares the artefacts byte
  for byte. Any compiler change fails that test until
  `./packages/kite-wasm/build.sh` is rerun. That is the intended behaviour: the
  plugin's whole claim is that it is not a second compiler.
- `site/kite_playground.wasm` — rebuilt by `site/build.sh`.
- `~/Documents/next-editor` — a separate repository holding its own copy.

A practical note that costs a confusing half hour: **do not run `cargo build`
while `cargo test --workspace` is running.** It invalidates artefacts mid-run
and the suite dies with `E0460: found possibly newer version of crate …`, which
reads exactly like a real failure and is not one.

## What a patch release skips

A fix that touches neither the compiler nor the library — a page, a document, a
README — needs only step 6. It does not need a tag, and it should not get one:
a version that names no binary change is a version somebody will try to
install.
