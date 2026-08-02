# Packaging

What a release produces, and what each package manager needs from it.

`.github/workflows/release.yml` builds `kitec` and `kite-lsp` for five targets
on a tag, writes a `SHA256SUMS` file over the archives, signs that file with
Sigstore, and attaches the compiler as a WebAssembly module. Everything in this
directory is generated from, or verified against, those artefacts — a manifest
whose checksum was typed by hand is a manifest nobody can check.

## The three manifests

| File | For | Updated by |
|---|---|---|
| [`homebrew/kite.rb`](homebrew/kite.rb) | macOS and Linux, `brew install` | `./packaging/render.sh <version>` |
| [`scoop/kite.json`](scoop/kite.json) | Windows, `scoop install` | `./packaging/render.sh <version>` |
| [`aur/PKGBUILD`](aur/PKGBUILD) | Arch Linux | `./packaging/render.sh <version>` |

Each is checked in with placeholder checksums so it can be read and reviewed.
`render.sh` downloads a release's `SHA256SUMS`, substitutes the real values,
and writes the three files out — and the release workflow runs it and attaches
the results, so the manifests a tap needs come *from* the release rather than
being written alongside it and hoped to match.

None of these is published yet. Homebrew and Scoop want a separate repository
(a tap and a bucket); the AUR wants an account and a git push. Those are three
decisions about identity and hosting, not three pieces of code, and they are
recorded here as undone rather than implied to be done.

## Signing

The release signs `SHA256SUMS` with [Sigstore](https://www.sigstore.dev)
keyless signing: GitHub Actions' OIDC token is the identity, the signature and
certificate go in the public transparency log, and there is **no private key
anywhere** — which is the point. A signing key held by one person is a key that
can be lost, stolen, or unavailable when it is needed, and a compiler is
exactly the artefact where that matters.

To verify a download:

```sh
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp 'https://github\.com/channyeintun/kite-lang/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum -c SHA256SUMS --ignore-missing
```

`install.sh` does the second half always and the first half when `cosign` is
installed. It does not install `cosign` to check a signature — a verifier
fetched by the thing it is verifying proves nothing.

## Syntax highlighting on GitHub

[`linguist/`](linguist/) holds a submission that is ready except for the one
thing that cannot be written: Linguist wants 2000 indexed files with the
extension before it will take a new language, and that is adoption rather than
work. The entry, the heuristics and a sample-assembling script are there; the
README beside them says what the current count is and how to check it.

## The compiler as WebAssembly

`kite_playground.wasm` is `kitec` compiled to `wasm32-unknown-unknown`. The
site already uses it, which is what makes the playground the compiler rather
than a re-implementation of it; the release attaches it too, so anything that
wants to compile Kite in a browser can have it without building the site.
