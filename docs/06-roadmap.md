# Implementation roadmap

Ordered so that each phase produces something runnable and each phase's output
is the test harness for the next.

---

## Sequencing principle

The most common way a language project dies is building the whole frontend
before anything executes, then discovering the semantics do not lower cleanly.
This plan inverts that: **get end-to-end execution on a tiny subset first, then
widen the language**. The vertical slice in Phase 1 is deliberately narrow and
deliberately complete.

The second principle: **the bytecode VM comes before the Wasm backend**, even
though Wasm is the reason the project exists. A VM is far quicker to build and
debug, and it becomes the differential-testing oracle for every backend after it.
Codegen bugs are the hardest class to find; having two independent
implementations that must agree finds them almost for free.

---

## Phase 0 — Specification review

**Deliverable:** the documents in this repository, critiqued.

Before writing the compiler, settle these. Each is cheap to change now and
expensive later:

1. **Is `check` the right propagation form?** It is the highest-traffic construct
   in the language. Write fifty lines of realistic Kite by hand and see how it
   reads.
2. **Is the error taint analysis worth the implementation cost?** It is the most
   novel thing in the language and the main reason to prefer it over Go. Estimate
   is a few hundred lines in `kite-flow`. Confirm it feels right on paper first.
3. **Is the widget set right?** Adding widgets later is easy; removing them is
   not.
4. **Do the diagnostics in the spec actually read well?** They are the product.

**Exit criterion:** a page of realistic Kite that you would be happy to maintain.

---

## Phase 1 — Vertical slice (4–6 weeks)

**Goal:** `kite run hello.kite` prints output, executed by the bytecode VM.

Language subset:

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

Only: `int`, `bool`, `str` literals; `fn`, `let`, `var`, `if`, `for`, `return`;
arithmetic and comparison; `io.print`.

Crates: `kite-span`, `kite-diag`, `kite-lexer`, `kite-ast`, `kite-parser`,
`kite-resolve`, `kite-types`, `kite-hir`, `kite-mir`, `kite-codegen-kbc`,
`kite-vm`, `kite-driver`.

**Exit criterion:** the program above runs, and a syntax error produces one
well-rendered diagnostic — not a `panic!`, not a debug-printed AST.

Do the diagnostic rendering *now*, not later. Every subsequent phase is easier
when errors are readable, and retrofitting spans into an IR that lacks them is
miserable.

---

## Phase 2 — The type system (6–8 weeks)

Add: `struct`, `enum`, `match` with exhaustiveness, `trait`, `impl`, generics
with monomorphisation, `[T]`, `{K: V}`, `?T`, closures.

This is the largest single phase and the one where the specification will be
found wrong in places. Expect to amend it.

**Exit criterion:** the `Shape`/`Display` example from
[spec §10](../SPECIFICATION.md#10-traits) runs, and a non-exhaustive `match`
reports exactly which variants are missing.

---

## Phase 3 — Error handling (3–4 weeks)

`(T, error)` returns, `check`, `??`, the `Error` trait, and the taint analysis in
`kite-flow`.

Build the analysis with an annotated test corpus from the start:

```kite
fn broken() -> int {
    let (v, err) = fallible()
    return v            //~ ERROR E0301
}
```

**Exit criterion:** every example in
[spec §7](../SPECIFICATION.md#7-error-handling) compiles or fails exactly as
documented, with the documented codes.

---

## Phase 4 — WasmGC backend (6–8 weeks)

`kite-codegen-wasm` via `wasm-encoder`. The type mapping is specified in
[docs/03 §7](03-compiler-architecture.md#7-wasmgc-lowering).

Order within the phase:

1. Functions, locals, arithmetic, control flow
2. GC structs and arrays
3. Enums via `br_on_cast` subtyping
4. `str` as `externref` with JS String Builtins
5. Trait objects with typed function reference vtables
6. `extern` declarations and generated JS glue

Set up differential testing against the VM on day one of this phase: every
program in the test corpus runs on both, and outputs must match. This is where
that investment pays.

Validate every emitted module with `wasmparser` in CI. Run the corpus in headless
Chrome, Firefox, and Safari — Safari's Wasm implementation differs enough to
catch real bugs, and it is the target most likely to be neglected.

**Exit criterion:** a Kite program runs in all three browsers, and `hello world`
is under 10 KB.

---

## Phase 5 — Concurrency (4–6 weeks)

`async`/`await`, the state machine transformation, `Task<T>`, the combinators,
`task.scope` and cancellation, `Share` inference, and the schedulers.

Native and bytecode targets get the real work-stealing pool here. Web gets the
cooperative loop plus the isolate pool for `task.parallel`.

**Exit criterion:** a program that fetches three URLs concurrently works on web;
the same source uses all cores on native; and moving a type with a `var` field
into `task.parallel` produces `E0520` with the documented message.

---

## Phase 6 — Standard library core (4–6 weeks)

`core`, `errors`, `fmt`, `math`, `time`, `io`, `json`, `task`, `buffer`, `test`.

Written in Kite. This is the first substantial body of Kite code, and it will
find ergonomic problems that no amount of specification review would have. Budget
time to act on what it reveals.

`json.decode<T>` requires compile-time derivation — build that machinery here,
since `Eq`, `Hash`, and `Debug` derivation need the same infrastructure.

**Exit criterion:** the standard library's own test suite passes on all three
backends.

---

## Phase 7 — Layout engine and DOM renderer (6–8 weeks)

The flexbox subset over flat buffers, the retained scene graph and its diff, the
widget set, the Elm-style update loop, and `DomRenderer`.

DOM first, deliberately: it is simpler, it is the accessible path, and it
establishes the correct rendering output that the canvas renderer must later
match.

Validate layout against Taffy's test suite — it is an independent flexbox
implementation with extensive fixtures, and agreeing with it is strong evidence
of correctness.

**Exit criterion:** the task-list application from
[docs/04 §6](04-stdlib-ui.md#6-events-and-state) runs in a browser, is keyboard
navigable, and is usable with VoiceOver.

---

## Phase 8 — Canvas renderer (8–10 weeks)

`CanvasRenderer` over Canvas2D, then WebGPU. HarfBuzz for shaping, UAX #14 line
breaking, UAX #9 bidi, the glyph atlas, damage tracking, hidden-overlay text
input, and the parallel ARIA tree.

The hardest phase, and the one most likely to overrun. Text is the reason.

Golden-image tests against the DOM renderer are the acceptance mechanism: the two
must produce identical layout for every fixture, across Latin, Cyrillic, Arabic,
Hebrew, Devanagari, Thai, CJK, and Burmese.

**Exit criterion:** the same task-list source runs under both renderers with
identical layout, and both are usable with a screen reader.

---

## Phase 9 — Native backend (6–8 weeks)

`kite-codegen-clif` via Cranelift, plus the precise generational collector in
`kite-rt`.

The GC is the real work. Stack maps at safepoints, precise root scanning, a
nursery with a bump allocator, and a write barrier for the old generation.

**Exit criterion:** the differential test corpus produces identical output on all
three backends, and a native binary starts in under 10ms.

---

## Phase 10 — Tooling (ongoing, start at Phase 2)

| Tool | Start after |
|---|---|
| `kite fmt` | Phase 1 — the CST is lossless, so this is cheap and prevents style debate |
| `kite-lsp` | Phase 2 — same salsa queries as the compiler |
| `kite test` | Phase 2 |
| `kite fix` | Phase 3 |
| `kite pkg` | Phase 6 |
| `kite doc` | Phase 6 |
| `--explain` | Phase 3 |

Start `kite fmt` early. A formatter from day one means no formatting discussion
ever happens, and the lossless CST that makes it easy is already required for the
LSP.

---

## Realistic timeline

| Milestone | Cumulative |
|---|---|
| Runs a program (Phase 1) | ~1.5 months |
| Real type system (Phase 2–3) | ~4 months |
| Runs in a browser (Phase 4) | ~6 months |
| Concurrent and useful (Phase 5–6) | ~9 months |
| Builds real UIs (Phase 7) | ~11 months |
| Canvas parity (Phase 8) | ~14 months |
| Three backends (Phase 9) | ~16 months |

This assumes one focused person. It is not a two-month project, and treating it
as one is the other common way language projects die.

---

## Where to be willing to cut

If time runs short, cut in this order — each is genuinely deferrable:

1. **Native backend (Phase 9).** The bytecode VM already covers native execution
   adequately for early users. Cranelift is a performance upgrade, not a
   capability.
2. **Canvas renderer (Phase 8).** DOM rendering is a complete product on its own,
   and it is the accessible one. Ship it, then add canvas.
3. **WebGPU path.** Canvas2D first; WebGPU is an optimisation.
4. **Generics.** Painful, but a language with `dyn` traits and no generics is
   usable, and generics can be added compatibly later. Adding them later is a
   real option; adding *error handling* later is not.

## Where not to cut

1. **Diagnostics.** They are the product. A language with mediocre errors will
   not be adopted regardless of its semantics.
2. **The taint analysis.** It is the strongest single argument for Kite over Go
   and the reason the error design is worth having.
3. **`Share`.** Adding it after v1 is a breaking change. It must be present from
   the first release, whether or not the platform can yet exploit it.
4. **Differential testing.** It is what makes three backends tractable for one
   person.

---

## First decision to make

Phase 0. Write a page of realistic Kite by hand — not examples chosen to flatter
the design, but the actual code you would write for something you want to build.
The specification is a hypothesis about what that will feel like. Test it before
building the compiler that assumes it.
