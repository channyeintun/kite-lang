# kite-cli

The Kite compiler, as an npm dependency.

```bash
npm install --save-dev kite-cli
```

```json
{
  "scripts": {
    "check": "kitec check src/main.kite",
    "fmt": "kitec fmt src/main.kite"
  }
}
```

**It is `kitec`, not a copy of it.** There is no reimplementation here and
nothing compiled to WebAssembly — `node_modules/.bin/kitec` is the same binary
a terminal runs, so a script and a terminal cannot answer differently. Every
argument, both streams and the exit code pass straight through.

## How the binary gets there

One package per platform, each holding the compiler for it, listed as
`optionalDependencies` with `os` and `cpu` set. npm installs the one that
matches and skips the rest. That is how esbuild and swc do it, and the reason
is the same: **no postinstall script and no download at install time**, so
adding the compiler to a project runs no code and fetches nothing beyond the
package itself.

| | |
|---|---|
| `@kite-lang/cli-darwin-arm64` | macOS, Apple silicon |
| `@kite-lang/cli-darwin-x64` | macOS, Intel |
| `@kite-lang/cli-linux-arm64` | Linux, statically linked |
| `@kite-lang/cli-linux-x64` | Linux, statically linked |
| `@kite-lang/cli-win32-x64` | Windows |

`kite-lsp` comes with it, so an editor can find the language server the same
way.

## Building the platform packages

```bash
./build.sh                    # the platform you are on, from target/release
./build.sh path/to/release/   # every archive of a tagged release
```

**Nothing is published yet.** The packages build and install correctly — a
packed tarball resolves and runs — and pushing them to npm waits on a tagged
release, since a version here has to name binaries that exist.
