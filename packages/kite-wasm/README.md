# @kite-lang/compiler-wasm

The [Kite](https://kite-lang.dev) compiler, as WebAssembly.

```bash
npm install --save-dev @kite-lang/compiler-wasm
```

It is the compiler, not a copy of it: the same Rust crate `kitec` is built
from, targeting WebAssembly instead of the machine. A test in the repository
builds the same project both ways and asserts the artefacts are byte-identical,
so a project's build and its author's terminal cannot come to different
conclusions about what a program means.

## Why not a native binary

`@kite-lang/cli` ships one, and on a developer's machine it is the faster
thing to run. This package exists because a binary cannot go everywhere:

- **One artefact for every platform.** No `os`/`cpu` matrix, no
  `optionalDependencies` that can resolve to nothing on a platform nobody
  built for, and no install that succeeds while leaving no compiler behind.
- **Nothing is fetched at install time.** No postinstall script, so the
  supply-chain surface is the tarball npm already verified.
- **It runs where WebAssembly runs.** Including a browser-based Node such as
  [WebContainer](https://webcontainers.io) — what StackBlitz and Bolt run —
  where native machine code cannot execute at all. A Kite project opened in a
  StackBlitz link builds without installing anything.

## As a library

```js
import { compiler, BuildFailed } from "@kite-lang/compiler-wasm";

const kite = await compiler();

kite.run('fn main() {\n    io.print("hi")\n}\n');   // → "hi\n"
kite.check("fn main() {\n    let x: int = 1\n}\n"); // → "" when clean
kite.format("fn f(a:int)->int{\nreturn a*2\n}\n");  // → the one layout

// A Kite module is a *directory*, so siblings are handed over by module
// name — `checkout`, not `checkout.kite`, because that is what `use` names.
try {
  const artefacts = kite.build({
    entry: await readFile("src/main.kite", "utf8"),
    siblings: { checkout: await readFile("src/checkout.kite", "utf8") },
    release: true,
  });
  // → { "app.wasm", "app.js", "api.js", "api.d.ts" } as Uint8Array
} catch (error) {
  if (error instanceof BuildFailed) process.stderr.write(error.diagnostics);
}
```

`checkModule({ entry, siblings })` is `check` for a whole module, which is what
a project wants: `check` takes a single file and would report a missing module
for a program that says `use checkout`.

The module is instantiated once per process and imports nothing.

## As a command

```bash
npx kitec check src/main.kite
npx kitec fmt src/*.kite
npx kitec build src/main.kite --out dist
```

`run`, `check`, `build`, `fmt` and `doc`, over the same compiler. The native
`kitec` is the fuller tool — `bundle`, `pkg`, `--native` and the language
server are the machine's to run — and it is what
[kite-lang.dev/install](https://kite-lang.dev/install) puts on your `PATH`.

## Building it

The `.wasm` is a build artefact and is not checked in:

```bash
./build.sh
```

`wasm-opt -Oz` takes about 20% off when it is installed, and the build works
without it.
