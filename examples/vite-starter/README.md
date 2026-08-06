# Kite + Vite

A checkout, where the arithmetic is Kite compiled to WebAssembly and the page
is HTML and CSS.

```bash
npm install
npm run dev
```

`npm install` brings the compiler with it. `@kite-lang/cli` is a dependency like any
other, and it holds the real `kitec` — the same binary a terminal runs, so
`npm run fmt` and `kitec fmt` cannot answer differently.

**`@kite-lang/cli` is not published yet.** Until it is, put `kitec` on your `PATH`:
[kite-lang.dev/install](https://kite-lang.dev/install). The plugin looks in
`node_modules/.bin` first and falls back to `PATH`, so both work.

**Copy this directory anywhere and it works.** The plugin is not published to
npm yet, so it is vendored in `plugin/` and imported by path rather than by
name. Depending on it with `file:` looked fine and was not: npm makes a
symlink, `npm install` succeeds and says nothing is wrong, and the failure
turns up later as `ERR_MODULE_NOT_FOUND` against a generated temp file. When
the package is published, `vite.config.js` becomes
`import kite from "vite-plugin-kite"`, the dependency goes in `package.json`,
and `plugin/` goes away.

## Scripts

```bash
npm run dev          # the site, rebuilt as you edit
npm run build        # a production build
npm run check        # Kite diagnostics, without running anything
npm run fmt          # lay the Kite out the one way
npm run fmt:check    # say which files would change, and fail if any would
```

`check` and `fmt` are `kitec` itself. There is one compiler and one formatter,
and a script here is a shorter way of typing them rather than a second tool
that could answer differently.

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
