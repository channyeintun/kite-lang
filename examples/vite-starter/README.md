# Kite + Vite

A checkout, where the arithmetic is Kite compiled to WebAssembly and the page
is HTML and CSS.

```bash
npm install
npm run dev
```

Needs `kitec` on your `PATH` — [kite-lang.dev/install](https://kite-lang.dev/install).

## What is where

| | |
|---|---|
| `src/checkout.kite` | Line totals, tax, discounts, money formatting, a Luhn check. No DOM in it. |
| `src/main.js` | Reads the page, hands values to Kite, puts answers back. |
| `index.html` | The markup, which keeps its job. |
| `vite.config.js` | Three lines. |

The split is the point: everything that could be wrong about money is on the
Kite side, with a type checker over it, and nothing there knows it is on a web
page.

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
