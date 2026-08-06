# Kite + Vite

A checkout, where the arithmetic is Kite compiled to WebAssembly and the page
is HTML and CSS.

```bash
npm install
npm run dev
```

**Nothing is installed and nothing is downloaded.** `npm install` brings the
compiler with it: `@kite-lang/compiler-wasm` is the Kite compiler built for
WebAssembly, so there is no binary to fetch, no platform matrix, and no
postinstall step. It is the same crate `kitec` is built from, and a test in
this repository asserts the two produce byte-identical output — so a build here
and a terminal elsewhere cannot disagree about what a program means.

That is also why this project runs unchanged in
[StackBlitz](https://stackblitz.com) and other WebContainer environments, where
native machine code cannot execute at all.

**Copy this directory anywhere and it works.** Both packages are on npm, so
there is nothing here to resolve by path and nothing vendored to drift.

## Scripts

```bash
npm run dev          # the site, rebuilt as you edit
npm run build        # a production build
npm run check        # Kite diagnostics, without running anything
npm run fmt          # lay the Kite out the one way
npm run fmt:check    # say which files would change, and fail if any would
```

`check` and `fmt` are the compiler itself, reached through WebAssembly rather
than a spawned binary. There is one compiler and one formatter, and a script
here is a shorter way of typing them rather than a second tool that could
answer differently.

The native `kitec` is the fuller tool — `bundle`, `pkg`, native execution and
the language server live there, and it is what
[kite-lang.dev/install](https://kite-lang.dev/install) puts on your `PATH`.
Nothing in this project needs it.

## What is where

| | |
|---|---|
| `src/main.kite` | The program: reads the inputs, listens, draws the rows. Owns this part of the page. |
| `src/checkout.kite` | Line totals, tax, discounts, money formatting, a Luhn check. No DOM in it. |
| `index.html`, `about.html` | The markup, which keeps its job. |
| `src/about.kite` | A second page's program, to show nothing is special-cased. |
| `vite.config.js` | Three lines. |

**There is no JavaScript in this project at all.** A page points a
`<script type="module">` straight at a `.kite` file and the plugin wires it up,
which is what a build tool is for.

**Nothing is special about `index.html` or `main.kite`.** `about.html` runs
`src/about.kite`, and each page gets its own module — the second one is 2.4 kB
and the first is 16 kB, and neither page downloads the other's. Any HTML, any
Kite, as many pages as you like; `vite.config.js` lists them the way Vite lists
any multi-page input. Every event listener, every DOM write
and all the arithmetic is Kite, over `std/dom` and `std/html`.

The split inside the Kite is worth a look too: `checkout.kite` knows nothing
about a web page, and `main.kite` is what puts its answers on one.

## Things worth looking at

**Money never touches a float.** `0.1 + 0.2` is the oldest money bug there is,
and the way not to have it is to hold pennies in an integer. `money()` is what
turns them back into `£12.05` — padded, so it is not `£12.5`.

**Tax rounds half up, written out.** `as int` truncates towards zero, which
loses a penny per line and is found by an accountant rather than by a test.

**Try editing `src/checkout.kite` while `npm run dev` is running.** It
recompiles and the page updates.

**Try breaking it.** Delete a `return`, or use a value whose error you have not
checked. The compiler's diagnostics come through Vite's overlay.
