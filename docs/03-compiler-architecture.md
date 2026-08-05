# Compiler architecture

How `kitec` is built in Rust: crate layout, IR pipeline, WasmGC lowering,
and diagnostics.

---

## 1. Guiding decisions

**No LLVM.** The Wasm backend emits WebAssembly directly via `wasm-encoder`. This
is the architecture MoonBit validated — their compiler performs static analysis
then generates Wasm, converted by `wasm-tools`, with no LLVM anywhere. The
benefits are large and immediate: sub-second builds, a toolchain measured in
megabytes rather than gigabytes, generated code you can read, and no dependency
on a C++ build. Cranelift covers native. LLVM stays off the v1 path entirely.

**Query-driven from the start.** Incremental compilation retrofitted later is a
rewrite. Building on [`salsa`](https://github.com/salsa-rs/salsa) from day one
makes the language server the same code path as the batch compiler, not a
parallel implementation that drifts.

**Diagnostics are a product surface, not a phase.** Every IR node carries a span.
Every pass emits structured diagnostics with codes, secondary spans, and
machine-applicable fixes. Several language decisions in the specification exist
purely to make this achievable.

**One frontend, three backends.** Everything through MIR is target-independent.
Backends never affect semantics.

---

## 2. Crate layout

```
kite/
├── crates/
│   ├── kite-span/          Source positions, file interning, spans
│   ├── kite-diag/          Diagnostic types, rendering, --explain, kite fix
│   ├── kite-lexer/         Tokeniser + newline-termination rules
│   ├── kite-ast/           Concrete syntax tree, spans on every node
│   ├── kite-parser/        Recursive descent + Pratt; error recovery
│   ├── kite-resolve/       Modules, imports, name binding, visibility
│   ├── kite-types/         Type checker, inference, trait solving, coherence
│   ├── kite-hir/           Desugared, fully typed high-level IR
│   ├── kite-flow/          Dataflow: error taint, definite init, Share, exhaustiveness
│   ├── kite-mir/           SSA, monomorphisation, optimisation passes
│   ├── kite-codegen-wasm/  WasmGC emission (wasm-encoder)
│   ├── kite-codegen-clif/  Native AOT (cranelift)
│   ├── kite-codegen-kbc/   Register bytecode
│   ├── kite-vm/            Bytecode interpreter
│   ├── kite-rt/            Runtime: scheduler, native GC, host bridge
│   ├── kite-std/           Standard library (written in Kite)
│   ├── kite-pkg/           Manifest, resolution, lockfile, fetch
│   ├── kite-lsp/           Language server (shares the salsa database)
│   └── kite-driver/        Pipeline orchestration
└── bin/
    └── kitec/              CLI
```

### External dependencies, and why each

| Crate | Purpose | Rationale |
|---|---|---|
| `salsa` | Incremental query engine | Compiler and LSP share one implementation |
| `wasm-encoder` | Wasm binary emission | Bytecode Alliance, tracks the spec, GC types supported |
| `wasmparser` | Validation of own output | Catch codegen bugs in CI, not in a browser |
| `cranelift-codegen`, `cranelift-module`, `cranelift-object` | Native backend | Rust-native, fast, designed for language backends |
| *(none — hand-written)* | Diagnostic rendering | The renderer is ~200 lines and pins the output format exactly to the specification, under snapshot test. Revisit if it grows. |
| `rustc-hash` | FxHashMap | Measurably faster than SipHash for compiler workloads |
| `indexmap` | Insertion-ordered maps | Kite maps guarantee insertion order |
| `la-arena` / `id-arena` | Arena allocation | Index-based IR nodes, no lifetime plumbing |

Also required: `unicode-ident`, for UAX #31 `XID_Start` / `XID_Continue`. This
is not optional polish — `XID_Continue` includes combining marks, and without
them Burmese, Devanagari, Thai, and Hebrew cannot spell ordinary words. An
`is_alphanumeric` approximation rejects `နာမည်` at its final character.

Deliberately avoided: parser generators (hand-written recursive descent gives far
better error recovery), LLVM, and any C or C++ dependency. As of Phase 1 the
compiler has exactly one external dependency.

---

## 3. Pipeline

```
  source text
      │
      ▼
┌───────────┐
│   Lexer   │  tokens + trivia; newline-termination applied here
└─────┬─────┘
      ▼
┌───────────┐
│  Parser   │  AST. Recovers at statement/declaration boundaries.
└─────┬─────┘  Never emits a cascade from one missing brace.
      ▼
┌───────────┐
│  Resolve  │  Module graph, cycle detection, name binding, visibility.
└─────┬─────┘  Every identifier now points at exactly one definition.
      ▼
┌───────────┐
│   Types   │  Bidirectional inference. Trait selection. Coherence.
└─────┬─────┘  Output: HIR, fully typed and desugared.
      ▼
┌───────────┐
│   Flow    │  Four dataflow analyses over the CFG (§5).
└─────┬─────┘  This is where E0301/E0302/E0210/E0520 come from.
      ▼
┌───────────┐
│    MIR    │  SSA. Monomorphisation. Async → state machines.
└─────┬─────┘  Inline, DCE, const-fold, identical-code-folding.
      ▼
 ┌────┴────┬──────────┐
 ▼         ▼          ▼
Wasm    Cranelift   Bytecode
```

### 3.1 Lexer

Hand-written. Produces tokens with full trivia attached (comments, whitespace) so
the same tree serves the formatter and the LSP.

Newline termination is decided here: a newline ends a statement unless the
preceding token is an operator, an open delimiter, or a comma. Because Kite has
no prefix-`(` or prefix-`[` expression statements, this rule has no ambiguous
cases — unlike JavaScript's ASI.

### 3.2 Parser

Recursive descent for declarations and statements, Pratt for expressions
(the precedence table is [spec §5.1](../SPECIFICATION.md#51-operator-precedence)).

Error recovery is a specified requirement, not best-effort. On an unexpected
token the parser skips to the next synchronisation point — `fn`, `struct`,
`enum`, `trait`, `impl`, `use`, `pub`, or a statement boundary at the current
brace depth — and inserts an `Error` node. A missing closing brace produces **one**
diagnostic.

The AST is a lossless concrete syntax tree: every byte of source is recoverable,
which is what lets `kite fmt` and `kite fix` operate on it directly.

### 3.3 Resolve

Builds the module graph (directory = module), detects cycles, and binds every
identifier to a definition ID. Visibility is checked here — the two-level `pub`
rule makes this a single predicate rather than a lattice walk.

### 3.4 Types

Bidirectional type checking: inference propagates *down* from annotations and
*up* from literals. Function signatures are always fully annotated, so inference
never crosses a function boundary. This is a deliberate limit — it keeps
inference local, makes errors point at the actual mismatch rather than at a
distant unification failure, and makes the checker fast.

Trait solving is straightforward because of the choices in the specification:
nominal implementations, the orphan rule, no associated types, no
specialisation, no variance. Resolution is a lookup in a `(TraitId, TypeId)` map,
with a small recursive search for generic bounds. There is no trait solver in the
Chalk sense and none is needed.

---

## 4. HIR

Post-typecheck, desugared, fully typed. Desugarings applied:

| Surface | HIR |
|---|---|
| `check err` | `if err != nil { return ErrPath(err) }` |
| `for x in xs {…}` | *(left intact — see note)* |
| `0..10` | `Range.new(0, 10)` |
| `"a \(b) c"` | `str.concat(["a ", Display.show(b), " c"])` |
| `Point{ ..p, y: 5.0 }` | explicit per-field construction |
| String literal | interned constant reference |

`match` remains in HIR — it is lowered to a decision tree in MIR, after the
exhaustiveness check has run against the source-level shape so diagnostics can
name the user's own arms.

**Loop forms also remain in HIR**, which is a correctness requirement rather
than a convenience. Expanding `for i in a..b` here would place the increment at
the end of the body, and `continue` would then jump past it — the loop would
never advance. MIR builds the control-flow graph with the increment in a block
of its own, and `continue` targets that block. This is why `kite-hir` carries
`ForRange`, `While`, and `Loop` rather than a single desugared `Loop`.

---

## 5. Flow analysis

Four forward dataflow passes over the CFG. All four terminate quickly because
every lattice has height ≤ 2.

### 5.1 Error taint — `E0301`, `E0302`

The enforcement mechanism behind
[spec §7.3](../SPECIFICATION.md#73-correlated-results-and-taint-analysis).

State per binding: value ∈ {`Tainted`, `Clean`}, error ∈ {`Unchecked`, `Checked`}.

```
transfer(let (v,e) = call)      : v←Tainted, e←Unchecked
transfer(read v) if v=Tainted   : emit E0301
transfer(scope-exit) if e=Unchecked : emit E0302
branch(e == nil)  then-edge     : e←Checked, v←Clean
branch(e != nil)  then-edge     : e←Checked, v stays Tainted
merge(a, b)                     : Tainted if either is Tainted   (⊓ = Tainted)
                                  Unchecked if either is Unchecked
```

The merge rule is what makes it sound: a value is Clean only when it is Clean on
*every* incoming path. The lattice is two elements tall, so the fixpoint is
reached in one pass over a reducible CFG.

This is emphatically **not** a borrow checker. There is no notion of ownership,
aliasing, moves, or lifetimes. It is the same machinery as definite-assignment
analysis, applied to a different property. Implementation is on the order of a
few hundred lines.

### 5.2 Definite assignment — `E0110`

Permits `let x: int` followed by assignment in branches
([spec §4.1](../SPECIFICATION.md#41-bindings)). Verifies exactly one assignment
on every path before first use.

### 5.3 `Share` inference — `E0520`

Structural, computed bottom-up over the type graph with a worklist for recursive
types. Checked at every task boundary (`task.start`, `task.parallel`, closure
capture into a spawned task). See
[docs/02 §4](02-concurrency.md#4-share-the-invariant-made-nearly-invisible).

### 5.4 Exhaustiveness — `E0210`

Maranget's usefulness algorithm on the match matrix. Reports the *missing
patterns*, not just "non-exhaustive", including nested and range patterns. This
is what makes adding an enum variant safe.

### 5.5 Exclusivity — `E0800`

Not one of the four: it carries no state across statements and needs no
fixpoint. It runs once over finished HIR, after every other check has passed,
and looks at one thing — the argument list of a direct call.

For each call, arguments of reference type (`Struct`, `Dyn`) that are *places* —
paths rooted at a local, built from field and index steps — are collected
alongside the parameter they bind. Two places conflict when their roots match,
no step definitely differs, and at least one of the two parameters is `var`. A
literal index compares exactly; an unknown one may be any element. Because the
walk stops at the shorter path, a prefix relation counts, which is what makes
`f(o, o.inner)` a conflict as well as `f(a, a)`.

The pass enforces [spec §14.1](../SPECIFICATION.md#141-exclusivity), and it is a
deliberately incomplete rule rather than the first half of a borrow checker.
It knows nothing about ownership, moves, or lifetimes, and it does not follow a
reference through the heap: two fields holding one object are two places here.
Completing it would require alias analysis, and alias analysis is what a
collector exists to make unnecessary. What is left after the collector is the
part a collector cannot help with — a wrong number, produced because two
parameter names turned out to be one object — and that part is checkable at the
call site, in one pass, with no annotation anywhere in the language.

Implementation is on the order of three hundred lines
(`crates/kite-types/src/exclusive.rs`).

---

## 6. MIR

SSA with explicit basic blocks. Generic code is monomorphised on entry to MIR, so
every MIR function is concrete.

### Passes, in order

1. **Monomorphisation** — instantiate generics; collect the reachable set from
   `main` and exported functions
2. **Async lowering** — `async fn` → resumable state machine
   ([docs/02 §8](02-concurrency.md#8-implementation))
3. **Match lowering** — decision trees, sharing tests across arms
4. **Inlining** — cost-model driven; always inline single-call-site functions
5. **Constant folding and propagation**
6. **Dead code elimination** — sound because Kite has no reflection
7. **Identical code folding** — merge monomorphised instantiations with
   byte-identical bodies. `[User]` and `[Post]` typically produce the same code
   when all operations are reference moves. This is the main defence against
   monomorphisation bloat, which matters more on the web than anywhere else.
8. **Escape analysis** — stack-allocate non-escaping aggregates on the native
   target; on Wasm this is left to the engine, which already does it
9. **Bounds check elimination** — where the index is provably in range

### Size budget

Binary size is a first-class metric on the web target. `kitec` reports it on
every build and CI can fail on regression:

```
$ kite build --target web --release
  compiled 42 modules in 0.8s
  app.wasm      18.4 KB   (gzip 7.1 KB)
  app.js         1.2 KB   glue, generated from 14 extern declarations
  ─ largest contributors ────────────────
    ui.layout.flex          3.1 KB
    json.decode<Task>       1.8 KB   ← 6 instantiations, 2 folded
    std.str                 1.4 KB
```

---

## 7. WasmGC lowering

The reference backend. Assumes all of WebAssembly 3.0.

### Type mapping

| Kite | WasmGC |
|---|---|
| `int`, `i64`, `u64` | `i64` |
| `i32`, `u32`, `i8`, `i16`, `u8`, `u16`, `bool`, `char` | `i32` |
| `f32` / `f64` | `f32` / `f64` |
| `str` | `externref` (JS string, via JS String Builtins) |
| `struct S` | `(type $S (struct (field …)))` |
| immutable field | `(field $x f64)` |
| `var` field | `(field $x (mut f64))` |
| `enum E` | `(struct (field $tag i32) …)` + one subtype per variant, dispatched by `br_on_cast` |
| `?T` | `(ref null $T)` |
| `[T]` | `(struct (field (mut (ref $arr_T))) (field (mut i32)))` — buffer + length |
| `[N]T` | `(array $T)` |
| `{K: V}` | GC struct wrapping index and entry arrays |
| `(A, B)` | anonymous GC struct |
| `fn(A)->B` | `(ref $functype)` — typed function reference |
| `dyn Trait` | `(struct (field $data anyref) (field $vtable (ref $vt_Trait)))` |
| `Task<T>` | GC struct: state tag + saved locals |
| `HostRef` | `externref` |

The one-to-one correspondence between Kite's per-field `var` marker and WasmGC's
per-field mutability flag is not a coincidence — the language was designed to
line up with it. Immutable fields let the engine hoist and constant-fold loads
without alias analysis.

### Trait objects

`dyn Trait` is a two-field struct: the data reference plus a vtable of **typed
function references** (ratified in Wasm 3.0). Calls go through `call_ref`, which
the engine type-checks structurally at instantiation rather than per call — no
runtime signature comparison, unlike `call_indirect` through a table.

```wat
(type $vt_Display (struct (field $show (ref $fn_anyref_to_extern))))
(type $dyn_Display (struct (field $data anyref) (field $vt (ref $vt_Display))))
```

### Enums

Each variant is a WasmGC subtype of the enum's base struct. `match` lowers to a
chain of `br_on_cast`, which is a single type check per arm — no tag load and
compare. For enums with many variants the compiler emits a tag switch through
`br_table` instead, choosing by arm count.

### Errors

`(T, error)` is a GC struct with a nullable error field. Because the flow
analysis has already proved the value is never read on the error path, the
compiler emits **no value at all** on that path — no zero, no null placeholder.
The error path allocates only the error.

### Validation

Every emitted module is run through `wasmparser` in the test suite. A codegen bug
should fail in CI, never in a browser.

---

## 8. Native backend (Cranelift)

MIR → Cranelift IR → object file → system linker.

The garbage collector is Kite's own: precise, generational, non-moving in v1.
Non-moving avoids needing a read barrier and keeps interior state simple; the
tradeoff is fragmentation, revisited after v1.

Precision comes from stack maps emitted at every safepoint, using type maps the
compiler already has from MIR. Because Kite has no `unsafe`, no pointer
arithmetic, and no FFI that hands out raw addresses, every reference is known to
the collector — conservative scanning is never required.

Cranelift's tradeoff is accepted deliberately: roughly 20% faster code generation
than LLVM, less optimised output. For application software this is the right side
of the trade. An LLVM backend for release builds stays possible but off the v1
path.

---

## 9. Bytecode backend

A register-based VM (in the Lua 5 / Dart tradition, not stack-based). Register
machines execute fewer dispatches per operation and map more directly from SSA.

Purpose:

- **Fast dev loop** — `kite run` with no codegen wait
- **REPL and scripting**
- **Embedding** — Kite as a configuration or plugin language inside a Rust host
- **Compiler test oracle** — differential testing against the Wasm and native
  backends catches codegen bugs that no single backend would reveal

`.kbc` files are versioned and validated on load. The VM shares `kite-rt`'s
collector and scheduler with the native target, so only the execution engine
differs.

---

## 10. Diagnostics infrastructure

```rust
pub struct Diagnostic {
    pub code:     Code,               // E0301 — stable, documented
    pub severity: Severity,
    pub message:  String,             // one line, lowercase, no period
    pub primary:  Span,
    pub labels:   Vec<Label>,         // secondary spans explaining *why*
    pub notes:    Vec<String>,
    pub fixes:    Vec<Fix>,           // machine-applicable
}
```

Rules enforced by the test suite:

- **One diagnostic per cause.** A missing brace produces one error, not forty.
  Regression-tested with a corpus of deliberately broken files asserting exact
  diagnostic counts.
- **Every type error carries a secondary span** naming the source of the
  expectation — the parameter or return type that created the constraint.
- **`--explain E0301`** prints the full rationale, including *why* the rule
  exists. The specification is the source text for these.
- **`kite fix`** applies every machine-applicable suggestion.
- **Source maps** are emitted for the Wasm target so browser stack traces name
  `.kite` files and lines.

Diagnostics are snapshot-tested. Changing a message requires updating a snapshot,
which makes message quality a reviewable part of every pull request.

---

## 11. Test strategy

| Layer | Method |
|---|---|
| Lexer / parser | Round-trip: parse → print → parse, assert identical trees |
| Parser recovery | Corpus of broken files; assert exact diagnostic count and codes |
| Types / flow | `.kite` files with `//~ ERROR E0301` annotations, rustc-style |
| Exhaustiveness | Property test: generate enums and matches, assert the checker agrees with a reference implementation |
| Codegen | Differential: run the same program on all three backends, assert identical output |
| Wasm validity | `wasmparser` over every emitted module |
| Wasm runtime | Execute under `wasmtime` and in headless Chrome, Firefox, and Safari |
| Diagnostics | Snapshot tests on rendered output |
| Standard library | Doc-comment code fences extracted and run as tests |

The differential codegen test is the highest-value one. Three independent
backends producing identical results is a strong signal, and it is the reason the
bytecode VM is worth building even though it is not needed for shipping.

---

## 12. Build performance targets

Non-negotiable, because they are the reason for skipping LLVM:

| Operation | Target |
|---|---|
| Full build, 10k lines | < 1s |
| Incremental, single function edited | < 50ms |
| LSP completion response | < 30ms |
| `kite run` (bytecode) startup | < 20ms |

`salsa` makes the incremental number achievable, and the LSP number is the same
query path. If these regress, the architecture has gone wrong somewhere and it is
worth stopping to find out where.
