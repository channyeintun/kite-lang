# vite-plugin-kite

Import `.kite` files from a Vite project.

```js
// vite.config.js
import kite from "vite-plugin-kite";

export default { plugins: [kite()] };
```

A page whose program is Kite points at it directly, and needs no JavaScript of
its own:

```html
<script type="module" src="/src/main.kite"></script>
```

Or import one as a library, if JavaScript is driving:

```js
import { load, money } from "./checkout.kite";

await load();
money(1205n);   // "£12.05"
```

`kitec` runs when a `.kite` file is imported and Vite gets the module it
produced. In dev the module is served and an edit recompiles it; in a build it
is emitted hashed beside your other assets.

**This is not a framework.** There is no runtime, nothing is injected into your
app, and it has no opinion about how the project is arranged. What you import
is what `kitec` wrote.

## Packages

A module is a directory, and a project's own `.kite` files are the ones beside
the entry. Anything else comes from `kite.toml`, which the plugin reads from
the first directory at or above the entry that has one:

```toml
[dependencies]
markdown = { git = "https://github.com/example/kite-markdown", tag = "v1.2.0" }
shared   = { path = "../shared" }
```

```kite
use markdown/render
use shared/money
```

**Nothing is fetched at build time.** A `path` dependency is read where it is;
a `git` one is read out of `.kite/vendor`, which `kitec pkg` filled — run that
once, on purpose, and commit `kite.lock`. A build that reached the network
would be a build that could change without anyone deciding it should.

A dependency's files are watched exactly as siblings are, so a package edited
in place — which is what `path` is for — reloads the page that uses it.

## Install

```
npm install -D vite-plugin-kite @kite-lang/compiler-wasm
```

The compiler is WebAssembly and comes with it, so there is nothing to put on
your `PATH` and no `os`/`cpu` matrix. It is the same crate the native `kitec`
is built from, and a test in the repository holds the two to identical bytes —
so what a build produces and what a terminal produces cannot come apart.

## What crosses the boundary

**`int`, `float`, `bool` and `str`, and nothing else yet.** An `int` is 64-bit,
so it crosses as a `bigint`:

```js
line_total(8999n, 3n);   // BigInt in, BigInt out
```

A function taking or answering with a slice, an `Option<T>` or a `(T, error)`
pair is still exported by the module, but the generated wrapper will not
describe it — `api.js` lists what it left out and why, rather than converting it
wrongly. In practice that means the aggregate stays on the Kite side of the
call and scalars cross, which is what the starter does.

## Options

| | |
|---|---|
| `bin` | The compiler. `kitec` on `PATH` by default. |
| `release` | `--release`: `assert` is dropped, `require` is not. Follows Vite's mode when not given. |

## What it does about `.wasm`

Vite inlines an asset under `assetsInlineLimit` — 4 KB by default — and a small
Kite module is under it. Base64 costs a third more bytes than the module it
encodes, the module stops being cacheable on its own, and the behaviour would
flip the day it grew past the limit. So `.wasm` is never inlined. Anything your
project already set for other assets is left alone.

## Types

`kitec` writes `api.d.ts` beside `api.js`. TypeScript will not find it through
this plugin yet — declare the module for now:

```ts
declare module "*.kite" {
  export function load(source?: string | Uint8Array): Promise<unknown>;
}
```
