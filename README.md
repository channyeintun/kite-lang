<img src="site/kite-mark.svg" alt="" width="64">

# Kite

A small, explicit programming language for the web.
WebAssembly is the primary target, not an afterthought.

```kite
fn main() {
    let (cfg, err) = config.load("app.toml")
    check err

    io.print("listening on \(cfg.port)")
}
```

---

## Why this exists

JavaScript and TypeScript grew into application development by accident. Every
serious web application today ships a compiler, a bundler, a type checker bolted
on from outside, a virtual DOM, and a runtime that re-derives structure the
compiler already knew and threw away.

WebAssembly 3.0 — ratified **13 June 2026** — removed the last technical reason
to accept that. It standardises garbage collection, native exception handling,
tail calls, and typed function references, and all of it is baseline across
Chrome, Firefox, and Safari. A language targeting Wasm today does not need to
ship a garbage collector inside its own binary. That single fact is the
difference between a 300 KB "hello world" and a 5 KB one, and it is why this
project is worth starting in 2026 and was not worth starting in 2022.

Kite is designed for the era where most code is read more often than it is
written, and where a large fraction of it is written with machine assistance.
It optimises for **unambiguous, greppable, locally-understandable code** over
terseness. Boilerplate is not the enemy. Hidden control flow is.

## Design commitments

| Commitment | Consequence |
|---|---|
| **27 keywords** | Comparable to Go's 25. Every one maps to a concept a beginner must learn anyway. |
| **No hidden control flow** | No exceptions, no operator overloading, no implicit conversions, no macros. `defer` releases; it cannot change a return value. |
| **Errors are values, and the compiler enforces it** | Go's `(T, error)` shape, but a value returned alongside an unchecked error is *unreadable* until the error is checked. Go's single biggest flaw, removed, without changing how the code looks. |
| **Immutable by default** | `let` and struct fields are immutable unless marked `var`. This maps directly onto WasmGC's per-field mutability flag, and makes most types automatically safe to share across tasks. |
| **No pointers, no references, no lifetimes** | Structs are GC-managed reference types. There is no `*T`, no `&T`, and no value/pointer receiver distinction. |
| **One concurrency concept, not two** | `async`/`await`. No goroutines, no channels, no mutex-by-default. Calling an `async fn` starts it; `await` is how the value comes out. |
| **Wasm is the reference target** | The semantics are chosen so that lowering to WasmGC is direct. |
| **HTML and CSS keep their jobs** | Kite replaces JavaScript, and nothing else. A program creates real elements with real class names, so somebody else's stylesheet — Tailwind, Bootstrap, a design system you already own — works on it unchanged. The browser lays out. Canvas is a `<canvas>` you draw into. |
| **Adoptable one file at a time** | Every `pub fn` is a real export, and `kitec build` writes `api.js` and `api.d.ts` beside the module. A TypeScript project imports it and type-checks against it, with none of the calling convention visible. That is how TypeScript itself spread. |
| **It lives inside a page, not instead of one** | A Kite program owns the parts of a page that need real logic, rather than owning the page. Attaching to `<body>` is still available; making it the only option is what puts a Wasm download in front of the first paint of everything. |

## Install

```bash
curl -fsSL https://kite-lang.dev/install.sh | sh
```

The current release is **v0.1.8**, signed and built by CI for macOS, Linux and
Windows; the installer reads `releases/latest` and checks the release's own
`SHA256SUMS`. Kite is pre-1.0 and the language is still allowed to move, so
the version is always `0.1.N` — the patch number climbs and the first two
never do.

**New to Kite?** [The crash course](https://nexteditor.dev/learn/kite-crash-course) tours the whole language in one narrated sitting, typing each program into an editor and running it — every one compiled by this compiler rather than transcribed.

It is also on npm, which is how a project that already has a `package.json`
would take it:

```bash
npm install --save-dev @kite-lang/cli
```

Building from source works too, and needs Rust 1.85 and nothing else — Kite
links no LLVM and ships no collector:

```bash
git clone https://github.com/channyeintun/kite-lang
cd kite-lang && cargo build --release
./target/release/kitec run examples/hello.kite
```

[kite-lang.dev/install](https://kite-lang.dev/install.html) has the rest:
package managers, what the installer checks and what it deliberately does not,
the platform table, and how to remove it.

## What runs today

```bash
kitec run     file.kite          compile and run
kitec run     file.kite --native run as machine code, under the JIT
kitec check   file.kite          check only
kitec test    file.kite          run every `test_` function
kitec fmt     file.kite          lay it out the one way
kitec doc     file.kite          the reference, from the doc comments
kitec fix     file.kite          apply every machine-applicable suggestion
kitec bundle  file.kite          one executable that needs nothing installed
kitec pkg     [directory]        resolve versions, write kite.lock
kitec build   file.kite --emit wasm   --out dist
kitec build   file.kite --emit native --out dist
kitec --explain E0301            why a rule exists
```

**The language.** `int`/`float`/`bool`/`str`, functions, `let`/`var`, `if`/`else`
as statement and expression, three `for` forms with labelled `break`/`continue`,
structs with methods, enums with named and positional payloads, `match` with
guards and exhaustiveness that names the missing variants, traits with default
methods and trait objects, generics on functions and types with bounds,
closures, slices, tuples, maps with `keys()`/`values()` and `for (k, v) in m`,
`Option<T>`, string interpolation, `defer`, `require`/`assert`, modules with
`pub`, `@derive` for the bodies a compiler can write, and enforced error
handling with `check`.

**Concurrency.** `async fn` compiles to a state machine in MIR — a starter and
a resume function — so every backend sees ordinary code and none of them knows
concurrency exists. `std/task` supplies `both`, `all`, `race`, `sleep`,
`timeout`, `scope` and `parallel`, all written in Kite over seven compiler
primitives.

```kite
let a = fetch("alpha", 100)
let b = fetch("beta", 50)
let (first, second) = await task.both(a, b)   // 100ms, not 150
```

**Errors that are types.** `impl Error for MyType` makes a concrete type
usable wherever an `error` is expected — the conversion happens at that point
and is an ordinary call in the IR, so nothing about it is hidden.

```kite
pub enum LoadError {
    Absent(path: str)
}

impl Error for LoadError {
    fn message(self) -> str {
        return match self {
            Absent(path) => "no task file at \(path)",
        }
    }
}

fn load(path: str) -> ([Task], error) {
    return _, LoadError.Absent(path: path)
}
```

**A standard library, in Kite.** `math`, `time`, `errors`, `fmt`, `json`,
`toml`, `text`, `test`, `buffer`, `task`, `sync`, `fs`, `http`, `socket`,
`crypto`, `canvas`, `js`, `dom`, `window`, `html`. Its own tests are ordinary Kite programs that run on *both*
backends and must agree.

**Bodies the compiler writes.** `@derive(Debug, Hash, Encode, Decode)` in front
of a struct or an enum writes them from the fields — as ordinary Kite, expanded
before resolution, so every backend handles it without knowing derivation
exists. `Display` is deliberately not derivable, and `Eq` is not derivable
because `==` is already structural on every value.

```kite
@derive(Encode, Decode)
struct User { name: str, age: int }

let (doc, err) = json.parse(text)
check err
let (user, uerr) = User.decode(doc)
```

`Decode` is the one worth dwelling on, because the usual alternative only
looks like it does the same thing. `axios.get<User>(url)` is a claim:
TypeScript writes the type in and never reads a field, so a server that stops
sending `age` yields an `undefined` somewhere later, far from the request.
`User.decode(doc)` reads the fields, and a body without `age` comes back as
`User.age: expected a whole number` — at the request, naming what was wrong.
The decoder comes from the struct, so unlike a validation schema kept beside
the type there is no second copy to drift out of step.

It is strict deliberately: a missing field fails the whole value. That suits a
server whose columns cannot be null and does not suit one that may leave a
field out, which wants a reader written by hand — a decision worth making
rather than discovering.

**Elements described, and only the difference written.** `std/html` holds a
tree as a value; `update` compares it against the last one and touches what
changed. Children are matched by key where there is one, so a thirty-five row
sort moves thirty-three elements and creates none — instead of rewriting every
cell it moved past.

**What a press does is written where the control is**, in the same list as the
class, closing over whatever is in scope there. A listener is attached once per
element and event; a repaint replaces the stored closure, so re-describing a
tree costs no registrations and the handler that runs is always the newest.

```kite
fn row(app: App, r: Row) -> html.Node {
    return html.keyed("\(r.id)", html.el("tr", [
        html.click(|e: dom.Event| { open(app, r.id) }),
    ], [
        html.txt("td", [html.class("num")], "\(r.id)"),
        html.txt("td", [], r.name),
    ]))
}

html.update(view, map(rows, |r: Row| -> html.Node { return row(app, r) }))
```

**A declared host boundary.** `@host("net") extern fn` becomes a Wasm import
and a group in the generated glue, so the boundary is written once, in Kite,
and the glue cannot drift from it.

**The browser, without writing JavaScript.** `std/js` is about thirty
primitives — `get`, `set`, `call`, `new`, `func`, conversions — and its host
block is a fixed sixty-five lines that does not grow however much of the platform a
program reaches. `std/dom` is written over it in ordinary Kite with no `extern`
in it at all, which is what makes the primitives a real answer to *the standard
library never wrapped the thing I need* rather than a promise. A host object is
an `externref`, so it is traced by the same collector as everything else, and a
thrown exception comes back as an error the compiler makes you check rather than
unwinding through the Wasm frames.

```kite
use std/dom

let button = dom.find("#save")
if button == nil {
    return
}
let (sub, err) = dom.on(button, "click", |e: dom.Event| { save() })
```

**Tools.** A formatter that preserves comments, a documentation generator, a
fixer, a test runner, a package manager that resolves versions across the whole
dependency graph, and a language server with diagnostics, hover, go to
definition, find references, rename, completion, symbols and inlay hints for
what a call inferred — all over the same passes the compiler runs.

## Targets

| Target | Backend | State |
|---|---|---|
| `wasm32-gc` | WasmGC via `wasm-encoder` | Every construct the language has. `--emit wasm` refuses nothing it can express |
| `kbc` | Register bytecode and a VM | The dev loop, the embedding target, and the differential oracle |
| bundle | This compiler with the program appended | One file, nothing installed, starts in about a millisecond |
| `native-*` | Cranelift, AOT and JIT | Machine code, with a precise collector in `kite-rt`. `--emit native` writes an object file; `run --native` needs no linker. macOS and Linux; Windows is refused, and says why |

Every program in the differential corpus is compiled to **all three** real
backends, run on all three, and the outputs compared. Three independent
implementations that must agree is what makes codegen bugs findable, and it is
why the bytecode VM was built before the Wasm backend even though Wasm is the
point of the project.

```bash
kitec build examples/hello.kite --emit wasm --out dist
# wrote dist/app.wasm (1077 bytes), dist/app.js and dist/index.html
```

**A Kite program goes in a page.** `examples/page` is a table of five thousand
rows. Every keystroke in the filter walks all of them, sorts what survives and
writes only the cells that changed — in about **3 ms**, from a **28 KB**
module. The markup is HTML, the
hover and the alignment and the colour of a status are CSS, and Kite holds the
rows, does the work and writes text and class names. Finding an element and
setting a class costs 3 KB, and there is a budget in CI that fails the build if
any of it grows.

That replaced a UI layer that computed its own layout and painted absolutely
positioned elements through two interchangeable renderers. It worked, and it
meant no stylesheet written by anyone else could address a single part of a
Kite application — which made the language a competitor to Flutter Web rather
than an alternative to JavaScript. `std/ui` and the Material package are gone;
`std/dom` is ordinary Kite over `std/js`, with no `extern` declaration in it.
[docs/04-the-web.md](docs/04-the-web.md) is the design, and
[the roadmap](docs/06-roadmap.md#the-direction-changed-at-phase-16) is the
reasoning.

Drawing survives the change and is narrowed by it. `std/canvas` and the drawing
builtins are untouched; what went is canvas as a *whole-application renderer*,
with the parallel accessibility tree and damage tracking that existed to make a
canvas pretend to be a document. A `<canvas>` is an element in a page that a
program draws into — a chart, a game, a visualisation — which is what a canvas
is on the web.

## The playground is the compiler

`kitec` is Rust and already targets WebAssembly, so the site compiles and runs
Kite in the same tab with no server at all. The diagnostics it shows are the
ones a terminal shows, because they come from the same code.

```bash
./site/build.sh
python3 -m http.server -d site 8000
```

## Reading order

| Document | Contents |
|---|---|
| [SPECIFICATION.md](SPECIFICATION.md) | **The language.** Lexical structure, types, declarations, expressions, error handling, traits, generics, modules. |
| [docs/01-platform-research.md](docs/01-platform-research.md) | What Wasm can and cannot do in 2026, with sources. Every constraint that shaped the design. |
| [docs/02-concurrency.md](docs/02-concurrency.md) | The async model, the `Share` marker, and how single-source code becomes parallel when the platform allows. |
| [docs/03-compiler-architecture.md](docs/03-compiler-architecture.md) | Crate layout, IR pipeline, WasmGC lowering, diagnostics. |
| [docs/04-the-web.md](docs/04-the-web.md) | The web model: HTML and CSS keep their jobs, Kite replaces JavaScript, and how a Kite program reaches the browser. |
| [docs/05-grammar.ebnf](docs/05-grammar.ebnf) | Complete formal grammar. |
| [docs/06-roadmap.md](docs/06-roadmap.md) | Implementation phases, and exactly how far each one got. |
| [site/brand.html](site/brand.html) | The mark: geometry, clear space, colourways, lockups. Open it in a browser. |
| [RELEASING.md](RELEASING.md) | Everything that carries a version, and the order it has to happen in. |

## What is not done

Recorded here rather than left to be discovered:

- **An error carries its message, not its value.** `impl Error for MyType` now
  works and a concrete type may be returned in an error slot — but the
  conversion renders the message and drops the value, so `cause`,
  `errors.chain`, `errors.is<T>` and `errors.as<T>` are still absent. Carrying
  the value needs a change to the error representation in all three backends:
  [Phase 24's remaining half](docs/06-roadmap.md#phase-24--concrete-error-types).
- **No line breaking outside the browser.** `ui.wrap` was the only one, and it
  went with `std/ui`. The browser wraps its own text, so this only matters to a
  program painting into a `<canvas>` — `std/text` has the UAX #14 break
  opportunities and `canvas.width_of` has the measurement, so it is a small
  function nobody has written rather than a missing capability.
- **No real parallelism, on any target.** A WasmGC reference cannot cross a
  thread boundary until shared-everything-threads ships, and the VM's values
  are `Rc`-based. `Share` is enforced now so that the day either changes, no
  source has to.
- **No shaping beyond joining.** Arabic joins and combining marks stay put;
  HarfBuzz-quality shaping is OpenType GSUB/GPOS and cannot be written against
  a boundary that only measures. Indic reordering, Thai mark placement and
  Burmese clusters come from the host's font stack or not at all.
- **No golden images, and no golden transcripts either.** The transcripts that
  compared eight scripts by the drawing calls they produced went with the
  layout engine that fed them. `std/text`'s bidi, joining and line breaking keep
  their direct tests; the end-to-end comparison does not exist, and pixels never
  did — that needs a browser and a dependency this does not have.
- **No native backend on Windows.** The collector finds roots by walking frame
  pointers, and Cranelift's Win64 prologue puts the frame record where that
  walk does not expect it. `--native` refuses there rather than corrupting the
  heap. Finishing it wants a Windows machine.
- **No `wasi:http/incoming-handler`.** A Kite program listens on a port through
  a generated Node adapter. WASI's version is a component-model export, and
  `kitec` emits a core module.
- **Nothing published.** The release pipeline is signed, packaged for Homebrew,
  Scoop and the AUR, and has never run: no tag has been pushed.
- **No Argon2.** It is not in WebCrypto, so it waits on a runtime that has it.

834 tests: unit tests per crate, an annotated compile-fail corpus, a
differential corpus that runs every program on **three** backends and compares,
the standard library's own suite on two of them, the host boundary and a real
socket under Node, the DOM layer and the typed door driven under Node — with
real `tsc` type-checking the generated declarations where it is installed — both
string representations compared against each other and against the VM, size
budgets that fail the build when a module grows, every example on the site, the
specification's own Appendix A, and the brand assets, which are checked for
drift because the mark is drawn once and copied three times.
