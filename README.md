<img src="site/kite-mark.svg" alt="" width="64">

# Kite

A small, explicit programming language for building application software.
WebAssembly is the primary target, not an afterthought.

```kite
fn main() {
    let (cfg, err) = config.load("app.toml")
    check err

    ui.run(App{ title: cfg.title })
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
a resume function — so both backends see ordinary code and neither knows
concurrency exists. `std/task` supplies `both`, `all`, `race`, `sleep`,
`timeout`, `scope` and `parallel`, all written in Kite over four compiler
primitives.

```kite
let a = fetch("alpha", 100)
let b = fetch("beta", 50)
let (first, second) = await task.both(a, b)   // 100ms, not 150
```

**A standard library, in Kite.** `math`, `time`, `errors`, `fmt`, `json`,
`test`, `buffer`, `task`, `http`, `socket`, `crypto`, `ui`. Its own tests are
ordinary Kite programs that run on *both* backends and must agree.

**Bodies the compiler writes.** `@derive(Debug, Hash, Encode, Decode)` in front
of a struct or an enum writes them from the fields — as ordinary Kite, expanded
before resolution, so both backends handle it without knowing derivation
exists. `Display` is deliberately not derivable, and `Eq` is not derivable
because `==` is already structural on every value.

```kite
@derive(Encode, Decode)
struct User { name: str, age: int }

let (doc, err) = json.parse(text)
check err
let (user, uerr) = User.decode(doc)
```

**A declared host boundary.** `@host("net") extern fn` becomes a Wasm import
and a group in the generated glue, so the boundary is written once, in Kite,
and the glue cannot drift from it.

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
| `native-*` | Cranelift, AOT and JIT | Machine code, with a precise collector in `kite-rt`. `--emit native` writes an object file; `run --native` needs no linker |

Every program in the differential corpus is compiled to **all three** real
backends, run on all three, and the outputs compared. Three independent
implementations that must agree is what makes codegen bugs findable, and it is
why the bytecode VM was built before the Wasm backend even though Wasm is the
point of the project.

```bash
kitec build examples/hello.kite --emit wasm --out dist
# wrote dist/app.wasm (500 bytes), dist/app.js and dist/index.html
```

`examples/todo.kite` is a task list with a text field, buttons and checkboxes,
navigable with the keyboard alone. A program with a user interface writes its
layout with `std/ui.kite` and draws through four host calls. The generated page runs the same module against a DOM
renderer, a canvas renderer, and a text renderer, switched live — the program
cannot tell which is running.

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
| [docs/04-stdlib-ui.md](docs/04-stdlib-ui.md) | The UI layer: layout engine, retained scene graph, and the dual DOM/canvas renderer. |
| [docs/05-grammar.ebnf](docs/05-grammar.ebnf) | Complete formal grammar. |
| [docs/06-roadmap.md](docs/06-roadmap.md) | Implementation phases, and exactly how far each one got. |
| [site/brand.html](site/brand.html) | The mark: geometry, clear space, colourways, lockups. Open it in a browser. |

## What is not done

Recorded here rather than left to be discovered:

- **No real parallelism, on any target.** A WasmGC reference cannot cross a
  thread boundary until shared-everything-threads ships, and the VM's values
  are `Rc`-based. `Share` is enforced now so that the day either changes, no
  source has to.
- **No shaping beyond joining.** Arabic joins and combining marks stay put;
  HarfBuzz-quality shaping is OpenType GSUB/GPOS and cannot be written against
  a boundary that only measures. Indic reordering, Thai mark placement and
  Burmese clusters come from the host's font stack or not at all.
- **Golden transcripts, not golden images.** The eight scripts are compared by
  the drawing calls they produce, on both backends. A rasterisation difference
  needs a browser and pixels, and would need a dependency this does not have.
- **No `wasi:http/incoming-handler`.** A Kite program listens on a port through
  a generated Node adapter. WASI's version is a component-model export, and
  `kitec` emits a core module.
- **Nothing published.** The release pipeline is signed, packaged for Homebrew,
  Scoop and the AUR, and has never run: no tag has been pushed.
- **No Argon2.** It is not in WebCrypto, so it waits on a runtime that has it.

683 tests: unit tests per crate, an annotated compile-fail corpus, a
differential corpus that runs every program on **three** backends and compares,
the standard library's own suite on two of them, the host boundary and a real
socket under Node, golden text transcripts across eight scripts, both string
representations compared against each other and against the VM, every example
on the site, and the brand assets, which are checked for drift because the mark
is drawn once and copied three times.
