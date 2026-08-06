# Install

One archive, checked against the release's own checksums, and two binaries out
of it: `kitec` and `kite-lsp`. Nothing is compiled, nothing is run from the
archive, and a checksum that does not match stops the install rather than
warning about it.

```
curl -fsSL https://kite-lang.dev/install.sh | sh
```

Read [the script](install.sh) first if you would rather — piping anything into
a shell deserves that, and it is 120 lines.

## There is no release yet

**Kite is pre-1.0 and nothing has been tagged**, so the command above has
nothing to fetch and will tell you so. The language is still allowed to move;
[the roadmap](read/06-roadmap.html) is what is settled and what is not.

Building from source works today. It needs Rust 1.85 and nothing else — Kite
links no LLVM and ships no garbage collector, so there is no third-party
toolchain to install first.

```
git clone https://github.com/channyeintun/kite-lang
cd kite-lang
cargo build --release
./target/release/kitec run examples/hello.kite
```

That leaves `kitec` and `kite-lsp` in `target/release/`. Put them somewhere on
your `PATH` and the rest of this page applies.

## From a package manager

```
brew install kite          # macOS and Linux
scoop install kite         # Windows
yay -S kite-bin            # Arch
```

Each of these downloads the same archive the installer does and checks the same
checksum. They wait on the same first tag.

## What the installer does

It reads the release's `SHA256SUMS`, picks the archive matching your platform,
downloads it, verifies the checksum, and unpacks two binaries. It writes to
`~/.local/bin` unless `KITE_PREFIX` says otherwise, and tells you if that
directory is not on your `PATH`.

If `cosign` is already installed, the Sigstore signature over the checksum file
is verified as well, against the release workflow's own identity. **The
installer will not install `cosign` for you**, and that is deliberate: a
verifier fetched by the thing it is meant to verify proves nothing. It says
what to install and carries on rather than pretending to have checked.

Signing is keyless — the workflow's OIDC token is the identity — so there is no
private key for anyone to lose or steal. For a compiler, that is the right
trade: a signing key held by one person is a single point of failure in exactly
the artefact where *trusting trust* is not hypothetical.

## Platforms

| Target | State |
|---|---|
| `aarch64-apple-darwin` | Built and tested |
| `x86_64-apple-darwin` | Built and tested |
| `x86_64-unknown-linux-musl` | Built and tested, statically linked |
| `aarch64-unknown-linux-musl` | Built and tested, statically linked |
| `x86_64-pc-windows-msvc` | Built and tested; `--emit native` is refused there and says why |

The native backend finds garbage-collection roots by walking frame pointers,
and Cranelift's Win64 prologue puts the frame record where that walk does not
expect it. Rather than corrupt the heap, `--native` refuses on Windows. The
WebAssembly and bytecode targets work there in full, which is every part of
Kite the web is about.

## Editors

The language server is `kite-lsp`, installed alongside the compiler. It gives
diagnostics, hover, go to definition, find references, rename, completion,
symbols and inlay hints for what a call inferred — over the same passes the
compiler runs, so it cannot disagree with `kitec`.

The VS Code extension is in `editors/vscode`. Any editor that speaks LSP can
point at the binary directly.

## Uninstalling

Delete the two binaries. There is nothing else — no daemon, no cache directory,
no registry entry, and nothing written outside the prefix you chose.

```
rm ~/.local/bin/kitec ~/.local/bin/kite-lsp
```
