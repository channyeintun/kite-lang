# @kite-lang/cli

The Kite compiler, as an npm dependency.

```bash
npm install --save-dev @kite-lang/cli
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

npm enforces the pairing rather than trusting it. Installing the macOS package
on Linux is refused before anything is unpacked:

```
npm error code EBADPLATFORM
npm error notsup Unsupported platform for @kite-lang/cli-darwin-arm64@0.1.0:
  wanted {"os":"darwin","cpu":"arm64"} (current: {"os":"linux","cpu":"x64"})
```

## Building the platform packages

```bash
./build.sh                    # the platform you are on, from target/release
./build.sh path/to/release/   # every archive of a tagged release
```

**Nothing is published yet**, and the order matters:

1. `git push` and tag a release, so CI builds all five targets reproducibly and
   signs the checksums. A compiler published from a laptop, built from source
   nobody can check out, is the *trusting trust* problem this project's release
   workflow goes to trouble to avoid.
2. `./build.sh path/to/release/` to make the platform packages from those
   artefacts.
3. `npm login`, then publish each platform package **before** this one — npm
   resolves `optionalDependencies` at install time, and a meta-package whose
   platform packages do not exist installs cleanly and then cannot find a
   compiler.

The name `kite-cli` was taken on npm by an unrelated project, which is why this
is scoped.
