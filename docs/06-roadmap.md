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

## Phase 1 — Vertical slice ✅ **complete**

**Goal:** `kite run hello.kite` prints output, executed by the bytecode VM.

**Exit criterion met.** The program below runs, and a syntax error produces one
well-rendered diagnostic. 212 tests pass, plus an annotated compile-fail corpus
in `tests/corpus/` that asserts each expected code lands on its expected line
*and* that no unannotated line produces a diagnostic — which is how the "one
diagnostic per cause" requirement stays true as the compiler grows.

Two things came out differently from this plan, both recorded where they
happened:

- **Definite-assignment analysis landed in Phase 1, not later.** The
  specification permits `let z: int` followed by branch assignment, and there is
  no way to accept that without the analysis. It is a two-element lattice
  merged at branch joins — the same shape as the Phase 3 taint analysis, so
  Phase 3 now has a worked precedent to copy.
- **Loop forms survive HIR into MIR.** The desugaring table below originally put
  `for` expansion in HIR. That is wrong: flattening the loop there puts the
  increment after the body, and `continue` then skips it, so `for i in 0..5 {
  if i == 2 { continue } }` would never terminate. MIR builds the CFG with the
  increment in its own block, which `continue` targets.

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

## Phase 2 — The type system

**Exit criterion met.** The `Shape` example from
[spec §10](../SPECIFICATION.md#10-traits) runs, and a non-exhaustive `match`
reports exactly which variants are missing, by name and arity:

```
error[E0210]: non-exhaustive match: `Rect(_, _)`, `Point` not covered
```

**Done:** the interned type arena, `struct` with methods and associated
functions, `enum` with named and positional payloads, `match` with guards,
alternation, ranges and struct patterns, exhaustiveness checking, `trait` with
default methods, and nominal `impl` validation.

**Remaining:** `[T]`, `{K: V}`, `?T`, tuples, closures, generics with
monomorphisation, `dyn Trait` dispatch, and string interpolation. Each is
additive; none changes what already works.

Four things came out differently from this plan:

- **`a.b` is no longer folded into a path by the parser.** Whether `io.print`
  names a module or a field of a local called `io` is a *resolution* question.
  The parser emits a field access unconditionally and the resolver records its
  answer against that span. The old design could not express a local shadowing
  a module path; a test now covers it.
- **Struct-literal suppression resets inside brackets.** The specification tells
  users to parenthesise a literal in an `if` condition. Without the reset that
  advice did not work.
- **Named call arguments exist, narrowly.** The specification writes
  `LoadError.Missing(path: path)`, so named-payload variant construction needs
  them. They are rejected everywhere else, with a note pointing at structs.
- **`match` lowers to sequential arm tests, not a decision tree.** Bindings are
  written only after a pattern matches, so a failed arm never leaves a
  half-written local behind. Sharing tests across arms is an optimisation for
  later; this is the version whose semantics are obvious.

---

## Phase 3 — Error handling ✅ **complete**

**Exit criterion met.** `(T, error)` returns, `check`, `return _, err`,
`errors.new`, `err.message()`, and the flow-sensitive taint analysis producing
E0301 and E0302. The annotated corpus in `tests/corpus/` covers both.

The analysis is the same two-element lattice as the definite-assignment pass
built in Phase 1, merged at branch joins — that precedent made this the smaller
job it was predicted to be.

Two behaviours are worth recording:

- **Testing the error cleans the value on the right branch.** `if err != nil {
  … } else { … }` proves the error is nil in the *else*, and `if err == nil` in
  the *then*. An error branch that diverges cleans the value for everything
  after the `if`. That is what makes a hand-written test do the same work as
  `check`.
- **Rebinding `err` in the same scope is permitted**, as the specification
  requires — it is what lets a function chain several fallible calls. Each `err`
  still gets its own local slot, so an earlier one that was never checked is
  still reported.

The specification's `??` defaulting operator was **removed** rather than
implemented. It hid a branch behind a sigil, which is precisely what the
language exists to avoid. An inline `if` does the same work in the open, and the
compiler narrows the optional or cleans the taint on the branch where it is
valid. `?.` and the `?T` type sigil went with it; the optional type is spelled
`Option<T>`.

---

## Phase 4 — WebAssembly backend 🟡 **step 1 of 6 complete**

`kite-codegen-wasm` emits WebAssembly directly via `wasm-encoder` — no LLVM.

**Done — steps 1 and 2: functions, locals, arithmetic, control flow, and
WasmGC structs.**

```bash
kitec build examples/hello.kite --emit wasm --out dist
# wrote dist/app.wasm (346 bytes) and dist/app.js
```

**346 bytes**, against a 10 KB budget. Shipping no garbage collector is what
makes that reachable, and it is the single reason this design was not viable
before WasmGC reached cross-browser baseline.

Every emitted module is validated with `wasmparser` in the test suite, so a
codegen bug fails in CI rather than in a browser.

**The differential test is live and it paid immediately.** Every program in
`crates/kite-driver/tests/differential.rs` is compiled to *both* bytecode and
Wasm, run on both, and the outputs compared. It caught two codegen bugs on its
first run: a unit-returning call being stored to a local that had nothing on the
stack, and a fallthrough `return` pushing a unit placeholder in a non-unit
function.

**MIR is a CFG and Wasm has no `goto`**, so the backend uses a dispatch loop:
one `loop` of nested `block`s entered through a `br_table` on a synthetic
program counter. It handles an arbitrary CFG including irreducible ones. A
relooper that recovers `if`/`loop` structure would produce tighter code and is
the obvious later improvement.

Structs lower to real WasmGC `struct` types in one `rec` group, so mutually
recursive declarations work — which they must, since every Kite aggregate is a
GC reference and recursion needs no annotation from the user. Kite's per-field
`var` marker becomes WasmGC's per-field mutability flag directly; that
correspondence is not a coincidence, it is why the language was designed this
way.

**Remaining steps:**

2. ~~GC structs and arrays~~ ✅
3. ~~Enums via subtyped variant records~~ ✅
4. `str` as `externref` with JS String Builtins (today a constant index into a
   table the glue holds, which needs no linear memory at all)
5. Trait objects with typed function-reference vtables
6. `extern` declarations driving the glue, rather than a fixed import list

String concatenation and comparison lower as host calls, since a `str` is an
index into a table the glue holds and the glue grows it.

Optionals lower as a nullable reference to a one-field box, so `nil` is a null
reference and the payload keeps its own type. Narrowing became an explicit
`Unwrap` node in HIR: previously the checker rewrote a local'''s type in a
branch, which the untyped VM tolerated and typed Wasm locals would not.

Slices are WasmGC arrays. `array.get` traps when out of range, which is exactly
Kite's rule for `xs[i]`; `.get()` bounds-checks and yields an optional instead.
Mutation copies the array first, which is what gives `[T]` value semantics —
the bytecode VM does the same lazily through `Rc::make_mut`, and here it is
unconditional, correct but not yet cheap.

A fallible result is one GC object holding both slots, so a function returns
the pair without multi-value plumbing. `return _, err` carries no value — that
is the point of the failure arm — but the record still needs bits there, so a
default goes in a slot the taint analysis has already proved unreadable.

**Every shipped example now compiles to WebAssembly and produces output
identical to the bytecode VM**, and a test asserts it so the two cannot drift.

Tuples lower as positional records, sharing the field machinery structs use.

Also still to lower: maps, trait objects, and structural equality on
aggregates. The rvalue match in the backend has **no catch-all**, so adding a
form to MIR fails to compile rather than silently producing a module that
traps.

**`--emit wasm` refuses what it cannot lower**, rather than emitting a module
that validates and then traps with no explanation:

```
error[E0204]: the wasm target cannot lower slices yet
  ┌─ slices.kite:1:1
  │
1 │ fn sum(xs: [int]) -> int {
  │ ^^^^^^^^^^^^^^^^^^^^^^^^^^ used in `sum`
  │
  = note: the bytecode target supports it: run without `--emit wasm`
```

**Exit criterion:** a Kite program runs in Chrome, Firefox, and Safari, and
`hello world` is under 10 KB. The size half is met; browser verification waits
on the remaining lowering steps.

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

## Where the implementation actually stands

Recorded honestly, because a roadmap that overstates progress is worse than
none.

| Phase | State |
|---|---|
| 0 — Specification review | ✅ done, and the spec was amended four times by what the code found |
| 1 — Vertical slice | ✅ complete |
| 2 — Type system | ✅ structs, enums, match, exhaustiveness, traits, slices, optionals, tuples, maps. ❌ closures, generics, `dyn` dispatch |
| 3 — Error handling | ✅ complete |
| 4 — WebAssembly backend | 🟡 everything the language currently has except maps and trait objects — all seven examples run. ❌ JS String Builtins |
| 5 — Concurrency | ❌ not started |
| 6 — Standard library | ❌ not started |
| 7 — Layout engine and DOM renderer | ❌ not started |
| 8 — Canvas renderer | ❌ not started |
| 9 — Native backend | ❌ not started |
| 10 — Tooling | 🟡 `kitec` with run/check/build/--emit/--explain. ❌ fmt, LSP, test runner, package manager |

406 tests, an annotated compile-fail corpus, and a differential corpus that runs
every program on both backends and compares.

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
