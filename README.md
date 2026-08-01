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
| **No hidden control flow** | No exceptions, no operator overloading, no implicit conversions, no destructors, no macros. |
| **Errors are values, and the compiler enforces it** | Go's `(T, error)` shape, but a value returned alongside an unchecked error is *unreadable* until the error is checked. Go's single biggest flaw, removed, without changing how the code looks. |
| **Immutable by default** | `let` and struct fields are immutable unless marked `var`. This maps directly onto WasmGC's per-field mutability flag, and makes most types automatically safe to share across threads. |
| **No pointers, no references, no lifetimes** | Structs are GC-managed reference types. There is no `*T`, no `&T`, and no value/pointer receiver distinction. |
| **One concurrency concept, not two** | `async`/`await`. No goroutines, no channels, no mutex-by-default. The scheduler is multi-threaded where the platform permits — the source never says which. |
| **Wasm is the reference target** | The semantics are chosen so that lowering to WasmGC is direct. Native and bytecode targets follow the Wasm semantics, not the other way round. |

## Targets

| Target | Backend | Status |
|---|---|---|
| `wasm32-gc` | WasmGC via `wasm-encoder` | Primary — the reference implementation |
| `kbc` | Register-based bytecode + interpreter | Dev loop, embedding, REPL |
| `native-*` | Cranelift AOT (aarch64, x86-64) | Desktop and CLI applications |

## Reading order

| Document | Contents |
|---|---|
| [SPECIFICATION.md](SPECIFICATION.md) | **The language.** Lexical structure, types, declarations, expressions, error handling, traits, generics, modules. |
| [docs/01-platform-research.md](docs/01-platform-research.md) | What Wasm can and cannot do in 2026, with sources. Every constraint that shaped the design. |
| [docs/02-concurrency.md](docs/02-concurrency.md) | The async model, the `Share` marker, and how single-source code becomes parallel when the platform allows. |
| [docs/03-compiler-architecture.md](docs/03-compiler-architecture.md) | Crate layout, IR pipeline, WasmGC lowering, diagnostics. |
| [docs/04-stdlib-ui.md](docs/04-stdlib-ui.md) | The UI layer: layout engine, retained scene graph, and the dual DOM/canvas renderer. |
| [docs/05-grammar.ebnf](docs/05-grammar.ebnf) | Complete formal grammar. |
| [docs/06-roadmap.md](docs/06-roadmap.md) | Implementation phases, with a defensible order. |

## Status

**Phase 1 complete** — the vertical slice runs end to end. See
[docs/06-roadmap.md](docs/06-roadmap.md) for what comes next.

```bash
cargo run --bin kitec -- run examples/hello.kite
```

```kite
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    let x = add(2, 3)
    if x > 4 {
        io.print("big")
    }
    for i in 0..x {
        io.print(i)
    }
}
```

Working today: the full lexer (all 27 keywords, newline termination, Unicode
identifiers), parser with error recovery, name resolution, bidirectional type
checking, definite-assignment analysis, MIR with explicit basic blocks,
register-based bytecode, and a VM. 212 tests, plus an annotated compile-fail
corpus.

The language subset is `int`/`float`/`bool`/`str`, functions, `let`/`var`,
`if`/`else`, the three `for` forms with labelled `break`/`continue`, and
`io.print`. Structs, enums, traits, and generics are Phase 2; error handling
with `check` is Phase 3; the WasmGC backend is Phase 4.

```bash
kitec run     file.kite      # compile and run
kitec check   file.kite      # check only
kitec build   file.kite --emit mir    # ast | hir | mir | kbc
kitec --explain E0301        # why a rule exists
```
