# Compiler architecture

How `kitec` is built in Rust: crate layout, the passes, the three backends,
and diagnostics. This describes the code as it is. Where the first design
wanted something the code does not have, the section says so rather than
describing the plan as though it had happened.

---

## 1. Guiding decisions

**No LLVM.** The Wasm backend emits WebAssembly directly via `wasm-encoder`. This
is the architecture MoonBit validated — their compiler performs static analysis
then generates Wasm, converted by `wasm-tools`, with no LLVM anywhere. The
benefits are large and immediate: sub-second builds, a toolchain measured in
megabytes rather than gigabytes, generated code you can read, and no dependency
on a C++ build. Cranelift covers native. LLVM stays off the path entirely.

**One pipeline, run from the source every time.** The first draft of this
document planned a query engine (`salsa`) from day one, so the language server
would share an incremental database with the batch compiler. That was not
built. Every compile starts from source text — lex, parse, load modules,
resolve, check, lower — and the language server calls the same `kite-driver`
entry point on every request. It is therefore not a second implementation that
can drift; it is the same code, run more often. What keeps that affordable is
that the passes are fast, not that they are incremental — see §12 for what that
costs.

**Diagnostics are a product surface, not a phase.** Every AST and HIR node
carries a span, and a MIR function carries its own. Every pass emits structured
diagnostics with codes, secondary spans, and machine-applicable fixes where one
exists. Several language decisions in the specification exist purely to make
this achievable.

**One frontend, three backends.** Everything through MIR is target-independent.
Backends never affect semantics, and the differential suite (§11) is what holds
them to it.

---

## 2. Crate layout

```
kite-lang/
├── crates/
│   ├── kite-span/          Source positions, file interning, spans
│   ├── kite-float/         How a float is written as text, for the VM and kite-rt
│   ├── kite-diag/          Diagnostic types, the renderer, codes and --explain text
│   ├── kite-lexer/         Tokeniser; newline termination decided here
│   ├── kite-ast/           Syntax tree, a span on every node
│   ├── kite-parser/        Recursive descent + Pratt; error recovery; NFC identifiers
│   ├── kite-resolve/       Name binding, qualified module names, visibility
│   ├── kite-types/         Type checking, inference, traits, and the flow analyses;
│   │                       lowers to HIR
│   ├── kite-hir/           Typed high-level IR, the type table, monomorphisation
│   ├── kite-mir/           Basic blocks and terminators; async → state machines
│   ├── kite-codegen-wasm/  WasmGC emission (wasm-encoder), JavaScript glue, validation
│   ├── kite-codegen-clif/  Native code through Cranelift: an object file, or JIT
│   ├── kite-codegen-kbc/   Register bytecode
│   ├── kite-vm/            Bytecode interpreter
│   ├── kite-rt/            Native runtime: collector, host functions, scheduler
│   ├── kite-fmt/           `kitec fmt`, over tokens
│   ├── kite-doc/           `kitec doc`, from doc comments
│   ├── kite-driver/        Pass order, module loading, `@derive`, doc tests, packages
│   ├── kite-lsp/           Language server, over the driver
│   └── kite-playground/    The compiler built for WebAssembly
├── bin/
│   └── kitec/              CLI, including `kitec bundle` and `kitec pkg`
└── std/                    The standard library, in Kite
```

Three crates the first layout named do not exist, and each went somewhere
specific:

- **`kite-flow`.** The dataflow analyses run inside the type checker, in the
  same walk that types each expression (§5). A separate pass would have had to
  rebuild the state the checker already holds.
- **`kite-std`.** The standard library is Kite source under `std/`, which
  `kite-driver` compiles into the binary with `include_str!`. It is a directory
  of `.kite` files, not a Rust crate, because nothing in it needs compiler
  support — that is the test it exists to pass.
- **`kite-pkg`.** The manifest, the version constraints and the solver are in
  `kite-driver` (`manifest.rs`, `semver.rs`, `solve.rs`); fetching and writing
  `kite.lock` is `kitec pkg`.

### External dependencies, and why each

| Crate | Purpose | Rationale |
|---|---|---|
| `wasm-encoder` | Wasm binary emission | Bytecode Alliance, tracks the spec, GC types supported |
| `wasmparser` | Validation of every emitted module | Catch codegen bugs at the build, not in a browser (§7). Default features off, because the compiler is itself shipped as WebAssembly and everything linked here is downloaded by everyone who builds a Kite project |
| `cranelift-codegen`, `-frontend`, `-module`, `-object`, `-jit`, `-native` | Native backend | Rust-native, fast, designed for language backends. Pinned together at 0.134, so the IR a frontend builds is the IR the module and JIT crates expect. Not linked into the WebAssembly build of the compiler, which has nothing to emit native code for |
| `unicode-ident` | UAX #31 `XID_Start` / `XID_Continue` | See below |
| `unicode-normalization` | NFC | Identifiers are compared after NFC ([spec §2.1](../SPECIFICATION.md#21-source-encoding)), so the parser normalises them, and rename compares spellings the same way |

`unicode-ident` is not optional polish — `XID_Continue` includes combining
marks, and without them Burmese, Devanagari, Thai, and Hebrew cannot spell
ordinary words. An `is_alphanumeric` approximation rejects `နာမည်` at its final
character.

Everything else is written here: the diagnostic renderer (about two hundred
lines, pinning the output format exactly), the language server's JSON, the
manifest's TOML, and the command line. `wasmprinter` is a dev-dependency only,
for reading modules in tests.

Deliberately avoided: parser generators (hand-written recursive descent gives far
better error recovery), LLVM, and any C or C++ dependency.

---

## 3. Pipeline

```
  source text
      │
      ▼
┌───────────┐
│   Lexer   │  tokens; newline termination applied here
└─────┬─────┘
      ▼
┌───────────┐
│  Parser   │  AST. Recovers at statement/declaration boundaries.
└─────┬─────┘  Never emits a cascade from one missing brace.
      ▼
┌───────────┐
│  Modules  │  Every `use`, transitively; cycles; the prelude and each
└─────┬─────┘  module merged under qualified names; `@derive` expanded.
      ▼
┌───────────┐
│  Resolve  │  Every identifier bound to one definition. Visibility.
└─────┬─────┘
      ▼
┌───────────┐
│   Types   │  Bidirectional inference, traits, and the flow analyses (§5)
└─────┬─────┘  in the same walk. Output: HIR, fully typed.
      ▼
┌───────────┐
│    HIR    │  Monomorphise, drop what nothing reaches, specialise `==` (§6).
└─────┬─────┘
      ▼
┌───────────┐
│    MIR    │  Basic blocks. `match` → sequential tests. Async → state
└─────┬─────┘  machines.
      ▼
 ┌────┴────┬──────────┐
 ▼         ▼          ▼
Wasm    Cranelift   Bytecode
```

`kite-driver` owns this order, so `kitec`, the tests, the language server and
the playground all drive the compiler identically. Resolution and checking
still run after a syntax error — the parser recovers, and later passes report
their own findings on what did parse. Code generation does not, because its
input would be poisoned.

### 3.1 Lexer

Hand-written. A token carries only a kind and a span; a literal's value is read
back from the source when a later pass needs it, which keeps tokens `Copy` and
small. Comments are not tokens the parser sees. The formatter asks for them
separately, alongside the tokens, because it is the one consumer that must keep
them.

Newline termination is decided here: a newline ends a statement unless the
preceding token is an operator, an open delimiter, or a comma. Two additions
make ordinary code read naturally — a line *starting* with `.` continues the
previous one, so method chains work, and `else` is never separated from its
`}`. Because Kite has no prefix-`(` or prefix-`[` expression statements, the
rule has no ambiguous cases — unlike JavaScript's ASI. The one token the lexer
cannot classify is `>`, which also closes `Option<int>`: it keeps the line
break after `>` and `>>`, and the parser skips it after any operator it has
read as binary.

### 3.2 Parser

Recursive descent for declarations and statements, Pratt for expressions
(the precedence table is [spec §5.1](../SPECIFICATION.md#51-operator-precedence)).

Error recovery is a specified requirement, not best-effort. On an unexpected
token the parser reports once, then skips to the next synchronisation point — a
line break or a token that starts a declaration, at the current bracket depth,
or the `}` that closes it — and leaves an `Error` node. The skip starts from
where the failed construct began, so the brackets it opened are closed before
anything else counts, and a declaration beginning a line always stops it. A
missing closing brace produces **one** diagnostic: a declaration keyword at the
indentation of an open `{`, in braces whose members are indented past it, is
where the author thought the braces had closed, and the report points at the
first `{` whose `}` was indented for an outer block. Only a declaration those
braces cannot hold says so. A method at the margin of an `impl`, or a `pub`
field at the margin of a struct, is a member laid out unusually — indentation
means nothing to Kite — and the braces that do hold it are where unwinding from
a method body's missing `}` stops. A method there that takes no `self` could
also be a function of its own; it is remembered, and if the declaration then
turns out to have a brace missing from before it, the declaration is read
again, ending there. A comma missing between parameters is supplied, and a
comma between struct fields read as a line break, so the declaration survives
for the code that uses it. One missing between two arguments on a line is
reported without a fix — `f("sum " n)` wanted a `+` — and the call becomes an
`Error` node rather than a call with a guessed number of arguments. At the end
of a line, a list, a call or a literal laid out over several lines is missing a
comma when the next line is indented past the one it opened on, and it goes on
as if the comma were there; a line no further in is the next statement, and it
is the closer that is missing, which is what is reported. A bracket whose
closer was reported missing there — or before the end of the input, or before
the closer of a bracket around it — counts as closed from then on, so recovery
resumes at the next line instead of taking the function's `}` for the
literal's. A mistake further along the line leaves the bracket open for
recovery to skip to its closer. Inside a struct or map literal a mistake is
recovered from inside the braces: the member is skipped to the next `,`, the
next member's line or the literal's own `}`, and the literal becomes an
`Error`. A binding whose value did not parse is kept, with an `Error` for a
value, and if skipping the value took the rest of the block with it, the block
ends in an `Error` statement. A struct literal, a `match`, a value `if` or a
closure cut short by a missing `}` becomes an `Error` node too, rather than
being checked as though what was written so far were the whole of it; a
declaration cut short is kept, because the rest of the program names it. An
index the parser already refused, such as `xs[a..b..c]`, makes the whole
indexing an `Error`.

The type checker gives an `Error` node the error type and reports nothing
against it or what is made from it: a pair taken apart from one binds both
names, returned from a fallible function it is a return, a generic call it
leaves nothing to infer from is an error too, and in a body holding one no
error binding is reported unchecked, since the `check` may be in what was not
read.

Recursion depth is bounded (`E0102`): brackets, blocks and prefix operators
may nest 256 levels. A left-deep chain — `a + b + …`, `x.f().g()…`, `else if`
— is read by a loop, but every later pass recurses over the tree it builds, so
its links are counted too, against a ceiling of their own: 8,192, or 1,024 in
the compiler built for WebAssembly, whose stack is the JavaScript engine's.
`kitec` and the language server give the compiler a 512 MiB stack
(`kite_driver::on_compiler_stack`), reserved rather than used, which a debug
build needs for a chain that long; on a main thread's 8 MiB a release build ran
out at under two thousand method calls.

The AST keeps a span on every node and stores no literal values; the source is
the single truth, and a pass that wants a value reads it through the span. It
is not a lossless tree — comments and blank lines are gone — which is exactly
why `kitec fmt` works on tokens instead: a formatter that rebuilt a program
from this tree would delete them. `kitec fix` does not need the tree at all. It
applies the text edits that diagnostics carry — and, as `kitec fmt` does,
refuses a file with lexical errors, whose diagnostics describe the tokens that
survived rather than the text on disk.

### 3.3 Modules

Loading is `kite-driver`'s, not the resolver's. Starting from the entry file's
`use` lines, it finds each module — a sibling directory or a single `.kite`
file, a dependency the package's `kite.toml` names, a standard library module,
or a source a host handed over (the playground, a bundler) — and follows that
module's own imports, reporting a cycle as `E0402`. An editor's unsaved buffer
is not handed over as a module: the files are found exactly as on disk, and
only then is a file that is open read from its buffer, so a buffer stands in
for the file it is a buffer of and for nothing else.
Each module's declarations are then merged into one item list under their
qualified names: `load` in module `config` is declared as `config.load`, which
cannot be forged as an identifier and is exactly what an importer writes. The
prelude is merged the same way, as a module whose names are searched without
being written.

`@derive` runs last, once every declaration is in the list. It is a
source-to-source expansion: the bodies it writes are ordinary Kite, parsed like
anything else, so nothing after it knows derivation happened.

### 3.4 Resolve

Binds every identifier to one definition — a function, a type, a variant, a
constant, a local slot or a builtin — so the checker never has to ask which `x`
this is. Visibility is checked here, and the two-level `pub` rule makes that a
single predicate rather than a lattice walk. A qualified name resolves only in
a module that imported it. Method calls are not resolved here: `x.area()` needs
`x`'s type, so it is the checker's.

### 3.5 Types

Bidirectional type checking: inference propagates *down* from annotations and
*up* from literals. Function signatures are always fully annotated, so inference
never crosses a function boundary. This is a deliberate limit — it keeps
inference local, makes errors point at the actual mismatch rather than at a
distant unification failure, and makes the checker fast. Once an error is
reported the offending expression takes an error type that satisfies every
expectation, so one mistake yields one diagnostic.

Trait resolution is straightforward because of the choices in the
specification: nominal implementations, exactly one `impl` per trait and type,
no associated types, no specialisation, no variance. Whether a type implements
a trait is a lookup; there is no trait solver in the Chalk sense and none is
needed. `Share` is the one bound nobody implements — the compiler decides it
structurally (§5.3).

Module-level constants are worked out before any body is checked
(`kite-types/src/consts.rs`). The right-hand side may be a literal, an
operator over constants, an interpolation of constants, or another constant —
not a call (`E0118`), and not itself (`E0119`) — and every use is replaced by
the literal in the HIR, so nothing downstream learns that constants exist.

---

## 4. HIR

Post-typecheck and fully typed. The checker produces it directly as it walks the
AST; there is no separate desugaring pass. What each surface form becomes:

| Surface | HIR |
|---|---|
| `check err` | `if err != nil { return _, err }` with the value slot left empty, or `return err` in a function answering with a bare `error` |
| `for x in xs {…}`, `for i in a..b {…}` | *left intact* — `ForSlice` and `ForRange`; see the note below |
| `a..b` | not a value: only a `for` header and a subslice `xs[a..b]` take a range, and each has its own node |
| `"a \(b) c"` | concatenation folded left to right, each hole rendered — `int`, `float` and `bool` directly, anything else through its `Display` implementation |
| `Point{ ..p, y: 5.0 }` | an explicit construction naming every field, reading the base for each field it supplies |
| `m.remove(k)`, `xs.push(v)` | statements on the binding (`MapRemove`, `SlicePush`), because both write to it |
| `grid[i][j] = v`, `b.cells.push(x)`, any change to a slice or map held other than in a plain binding | a block: the operands bound to hidden locals, each level copied into another, the innermost changed, and each written back (`SetIndex`, `SetField`) — so every such statement still changes a local |
| a module-level constant | the literal it evaluated to |

`match` remains in HIR — it is lowered in MIR, after the exhaustiveness check
has run against the source-level shape so diagnostics can name the user's own
arms.

**Loop forms also remain in HIR**, which is a correctness requirement rather
than a convenience. Expanding `for i in a..b` here would place the increment at
the end of the body, and `continue` would then jump past it — the loop would
never advance. MIR builds the control-flow graph with the increment in a block
of its own, and `continue` targets that block. This is why `kite-hir` carries
`ForRange`, `ForSlice`, `While`, and `Loop` rather than a single desugared
`Loop`.

---

## 5. Flow analysis

There is no separate flow pass and no control-flow graph at this stage. The
analyses below run inside the type checker, over the AST it is already
walking, with their state saved at each branch and joined where control
arrives from more than one place. That is enough because every lattice here
has height at most two and the source's control flow is structured: an `if`
joins its two arms, and a loop joins the state after its body with the state it
was entered in, since the body may have run zero times. Exhaustiveness and
exclusivity are the exceptions, and each says so below.

### 5.1 Error taint — `E0301`, `E0302`

The enforcement mechanism behind
[spec §7.3](../SPECIFICATION.md#73-correlated-results-and-taint-analysis).

Each local is in one of three states: `Clean` (nothing to prove), `Tainted` (a
value bound from a fallible call whose error is not yet known to be nil), or
`Unchecked` (an error nobody has inspected).

```
let (v, e) = call               : v ← Tainted, e ← Unchecked
let e = call  (bare `error`)    : e ← Unchecked
read v  while Tainted           : E0301
e out of scope while Unchecked  : E0302 — at the end of its block, and at
                                  a return, check, break or continue
                                  that leaves it behind
e = call  (assigned later)      : e ← Unchecked
e = …  while e is Unchecked     : E0302 at the write — it drops the old one
test e (`e == nil`, `e != nil`) : e ← Clean
path on which e is proved nil   : v ← Clean
check e                         : v ← Clean after it — the other path returned
join(a, b)                      : Tainted if either is Tainted,
                                  else Unchecked if either is Unchecked
```

The join rule is what makes it sound: a value is Clean only when it is Clean on
*every* incoming path. Inside `if e != nil { … }` the value is still Tainted —
that branch is exactly the path on which it does not exist.

This is emphatically **not** a borrow checker. There is no notion of ownership,
aliasing, moves, or lifetimes. It is the same machinery as definite-assignment
analysis, applied to a different property, and it lives beside it.

### 5.2 Definite assignment — `E0110`, and divergence

Permits `let x: int` followed by assignment in branches
([spec §4.1](../SPECIFICATION.md#41-bindings)). A local is `Assigned` after a
join only when it is assigned on every incoming path. The same walk tracks
whether a block always leaves — through `return`, `break` or `continue` — which
is what reports `E0116` for a statement after one and `E0203` for a function
that can fall off its end. Narrowing an `Option` after `if x != nil`, and
knowing an error is not nil before `err.message()` is allowed, ride on the same
state.

### 5.3 `Share` — `E0520`

Structural: a type is `Share` when it is deeply immutable, computed over the
type graph with a guard for recursive types (`Types::is_share` in `kite-hir`).
It is checked wherever a type parameter is bounded by `Share` — today that is
`task.parallel`'s input and output, which is the one place the standard library
asks — and the diagnostic names the field responsible, because "not Share" says
nothing a reader can act on. See
[docs/02 §4](02-concurrency.md#4-share-the-invariant-made-nearly-invisible).

### 5.4 Exhaustiveness — `E0210`

Maranget's usefulness algorithm on the match matrix
(`kite-types/src/exhaustive.rs`), run when the checker reaches a `match`.
Reports the *missing patterns* by name, not just "non-exhaustive", including
nested patterns. This is what makes adding an enum variant safe.

### 5.5 Exclusivity — `E0800`

Not a dataflow analysis at all: it carries no state across statements and needs
no fixpoint. It runs once over finished HIR, after every other check has passed,
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

## 6. From HIR to MIR

### On HIR

Three passes run on HIR before it is lowered (`kite-hir/src/mono.rs`), because
HIR still has the shape the checker produced and each is one walk over
expression trees:

1. **Monomorphisation** — one copy of a generic function per set of type
   arguments its callers use, and the templates dropped. Kite specialises
   rather than boxing because the concrete type *is* known at the call site;
   runtime polymorphism is what `dyn Trait` is for. No backend ever sees a type
   parameter. A generic function that instantiates itself at an ever larger
   type is recognised by its growth — its own template 64 times on the chain
   of copies that asked for each other, or on that chain at all once the type
   arguments pass the size limit — and refused with `E0220` rather than run
   forever. The limits themselves (256 levels, 65,536 parts, 65,536 copies)
   bound what a program that finishes may ask for, and are reported as that.
2. **Pruning** — drop every function nothing can reach from the entry, the
   program's own `pub` functions, a closure's lifted body or a vtable.
   Reachability is exact, because a call names its target by index. This is
   what keeps the prelude, which is in every program, out of a `hello world`.
3. **Equality specialisation** — `==` inside a generic function was checked as
   a structural comparison, because nothing was known about the operand. Once
   specialisation has made it concrete, a primitive gets the primitive's own
   comparison.

### MIR

MIR is explicit basic blocks and terminators. It is **not** SSA: locals are
numbered slots, which the bytecode backend maps straight onto registers, and
nothing yet needs more. Lowering does two things beyond building the graph:

- **Match lowering** — arms are tested in order, each falling through to the
  next on failure, and the block after the last arm is `unreachable`, because
  exhaustiveness proved it dead. A decision tree that shares tests across arms
  is not built; sequential testing is what makes the semantics obvious.
- **Async lowering** — `async fn` becomes a *starter*, which allocates the frame
  and the task and hands the scheduler a closure that resumes it, and a *resume
  function*, which is the original body with an entry that jumps to wherever
  the last suspension left off
  ([docs/02 §8](02-concurrency.md#8-implementation)). Locals are spilled to the
  frame and reloaded around each suspension rather than rewritten into frame
  fields everywhere; which ones are actually live across a suspension is not
  computed. After this, no backend knows `async` exists.

The native and bytecode backends skip blocks unreachable from the entry.

### Not built

The first design listed six more passes. None exists, and each is worth
recording with the reason it was wanted:

- **Inlining**, cost-model driven, always inlining a single-call-site function.
- **Constant folding and propagation** inside function bodies. Module-level
  constants are folded (§3.5); nothing else is.
- **Dead code elimination** beyond the function-level pruning above and the
  unreachable-block skip — sound, when it is written, because Kite has no
  reflection.
- **Identical code folding** — merging monomorphised instantiations with
  byte-identical bodies. `[User]` and `[Post]` typically produce the same code
  when all operations are reference moves, and this is the main defence against
  monomorphisation bloat, which matters more on the web than anywhere else.
- **Escape analysis** — stack-allocating non-escaping aggregates on the native
  target. On Wasm it is the engine's job.
- **Bounds-check elimination** where the index is provably in range.

### Size budget

Binary size is a first-class metric on the web target. `kitec` reports it on
every build:

```
$ kitec build examples/page/main.kite --emit wasm --release --out dist
  and dist/api.js with dist/api.d.ts
wrote dist/app.wasm (… bytes), dist/app.js and dist/index.html
```

**There is no per-symbol breakdown**, and this section used to show one — a
mock-up naming `ui.layout.flex` and `json.decode<Task>`, neither of which
exists. What is real is one number per build, and a gate that fires when it
moves: `crates/kite-driver/tests/size.rs` compiles four programs and asserts a
budget for each, recording what they cost today in a comment beside it, and the
site's own program has a fifth. A budget is generous on purpose — it catches a
regression of a different order, a runtime creeping in or a pass that stopped
pruning — and the recorded number is what catches an ordinary change.

Attributing bytes to the declaration they came from would be worth having and
is not written.

---

## 7. WasmGC lowering

The reference backend. It assumes WebAssembly 3.0's garbage collection and
typed function references, and ships no collector of its own — which is the
whole reason a `hello world` is hundreds of bytes rather than hundreds of
kilobytes.

MIR is a control-flow graph and Wasm has structured control flow with no
`goto`, so each function is a **dispatch loop**: one `loop` containing nested
`block`s, entered through a `br_table` on a program-counter local. It handles an
arbitrary graph, irreducible ones included. The blocks are laid out in reverse
postorder, so every jump goes forward but a loop's jump back to its head; a
jump forward is a plain `br` out to the block holding its target, and only a
jump back goes round the dispatch loop. Code without a loop is then nothing but
blocks and forward branches. When every jump went round the loop, V8's
optimising compiler merged every local at the loop's head from every block, and
a chain of two thousand `||` took it gigabytes after the program had finished.
A relooper that recovered `if` and `loop` structure would produce tighter code
and is the obvious later improvement.

### Type mapping

| Kite | WasmGC |
|---|---|
| `int` | `i64` |
| `float` | `f64` |
| `bool` | `i32` |
| `str` | `(array (mut i32))`, one Unicode scalar per element. A JavaScript string exists only while a value crosses the host boundary |
| `struct S` | `(struct (field …))`, led by an `i32` identity tag when `S` appears behind a `dyn` |
| immutable field | `(field $x f64)` |
| `var` field | `(field $x (mut f64))` |
| `enum E` | a base record holding an `i32` tag, and one subtype per variant carrying the tag and that variant's payload |
| `Option<T>` | a nullable reference to a one-field box record, one per payload type: `nil` is null, and the payload keeps its own type |
| `[T]` | a header record `{buf: (mut (ref $arr)), len: (mut i32)}` over `(array (mut T))`, one pair per element type. The buffer may be longer than the slice |
| `{K: V}` | a record holding two parallel arrays, keys and values, in insertion order |
| `(A, B)` | one record per tuple shape |
| `(T, error)` | one record per value type: the value slot, and the error |
| `error` | a nullable reference to the error record — message, carried value, that value's type tag, and the error it wrapped |
| `fn(A) -> B` | a closure record: a typed function reference and an `anyref` environment, called through `call_ref` with the environment first |
| `dyn Trait` | a reference to the tagged root record every dispatchable type extends |
| `Task<T>` | an ordinary struct the compiler declares, with the frame an ordinary struct of spilled locals and a state field |
| `JsValue` | `externref` |

The one-to-one correspondence between Kite's per-field `var` marker and WasmGC's
per-field mutability flag is not a coincidence — the language was designed to
line up with it. Immutable fields let the engine hoist and constant-fold loads
without alias analysis.

**A slice is a header over a buffer with room to spare, written in place when
nothing else can see it.** Slices are values — `let a = xs; xs.push(1)` leaves
`a` alone — and the VM gets that from `Rc::make_mut`, which copies only when the
reference count says the storage is shared. A GC target has no count, so the
compiler keeps the fact instead: every slice local a function writes into has an
`i32` *owned* flag beside it, set when the function made the header and buffer
itself (a literal, a range, `keys()`, or its own copy) and cleared wherever the
local is read in a way that can keep the reference — into another local, a call,
a field, a map, a box, a closure. A length, an element, a comparison or a range
(which copies) keeps nothing and clears nothing. A parameter, or anything read
out of somewhere else, starts unowned.

`push` and `xs[i] = v` call a small helper per element type with the header and
the flag. Owned with room, `push` writes the next slot and bumps the length;
otherwise it copies into a buffer twice as long plus four, and the local owns the
result. So a loop of pushes is amortised constant time — a hundred thousand of
them went from twenty seconds to a few milliseconds — and the first write to a
slice someone handed in copies once rather than every time. An index is checked
against the header's length as a 64-bit value, never against the buffer's, which
may be longer. `tests/differential.rs` (`slices-are-values`) changes a slice
after every way its reference can be kept and compares all three backends.

### Structural equality

`==` on an aggregate calls one generated function, `(anyref, anyref, kind) ->
i32`, with a case per compared type. Scalars and strings are compared in place;
a component that is itself an aggregate is pushed onto a list of pending pairs —
a cons list of immutable cells — and the same loop takes it off again. The
comparison therefore never recurses, and a value a million cells deep compares
in bounded stack, as it does on the VM and natively; the first version generated
a function per type that called its components' functions, and ended in the
engine's `RangeError`. A map's keys are compared by the same function, so any
type `==` accepts can key a map.

### Trait objects

A `dyn Trait` value is the concrete value itself, statically widened to the
tagged root record — WasmGC subtyping makes the conversion free. A method call
goes to one **dispatcher** function per trait method: a chain of comparisons on
the stored tag, each arm casting the receiver to its concrete type, where the
cast cannot fail, and calling that implementation.

The tag is there because WasmGC compares types structurally: `struct Circle {
r: float }` and `struct Square { s: float }` are the same Wasm type, and
`ref.test` cannot tell them apart. Only types that appear behind a `dyn` carry
one, so a program without `dyn` pays nothing. A `br_table` would be denser, but
tags encode a struct or enum id and are not contiguous, and a trait rarely has
more than a handful of implementers.

The first design here was a vtable struct of typed function references per
trait object, called through `call_ref`. It is not what was built.

### Enums

A `match` reads the tag from the base record and compares it — MIR tests the
arms in order (§6) — and reading a variant's payload casts to that variant's
record once the tag has said which it is. The first design lowered `match` to a
chain of `br_on_cast`; the code does not.

### Errors

`(T, error)` is one GC record, so a function returns both halves at once. On
the failure path, `return _, err` still has to put bits in the value slot, so a
default of the value's type goes there — unobservable, because the taint
analysis has proved nothing can read it. The error path allocates the pair and
the error. (The first design claimed it would emit no value at all; a record
with a slot cannot do that.)

### Validation

Every emitted module is run through `wasmparser` — in the test suite, and again
on every build before the module is handed back. A codegen bug should fail in
CI; failing at the build is the backstop for the ones CI has no test for.

The build-time check is not redundant with the tests, because of where the
alternative surfaces. An invalid module is accepted by `build` and rejected by
the engine, so the first sign of one is a blank page and a `CompileError` naming
`wasm-function[37]` — a long way, in a real module, from the function that
caused it. Validating costs microseconds and it is the last point at which the
compiler still knows what it was lowering. A failure is `E0900`, which says it
is a bug in Kite rather than in the program.

---

## 8. Native backend (Cranelift)

MIR → Cranelift IR, then one of two ends. `kitec build --emit native` writes a
relocatable object file and hands `cc` the job of linking it against the
`staticlib` build of `kite-rt`. `kitec run --native` maps the same code
straight into the process through `cranelift-jit`, resolving the same runtime
symbols by name, so running a file needs no linker at all.

An `int` is an `i64`, a `float` an `f64`, a `bool` an `i8`, and everything else
is an `i64` holding a pointer into `kite-rt`'s heap, with 0 for `nil`. The
runtime's objects are self-describing — a two-word header, then 8-byte slots,
with registered shape tables saying which slots are references — so one
collector traces everything, and one routine renders and compares any value
the way the bytecode VM does.

The garbage collector is Kite's own: **precise and generational**. New objects
are bump-allocated in a contiguous nursery, and a minor collection *evacuates*
the live ones into the old generation, updating every reference — which is why
precision is not optional. The old generation does not move: each object is its
own allocation, swept by an occasional mark-and-sweep once the generation has
grown past a threshold. The in-place heap mutations — a `var` field write, and a
write into a slice the function owns (below) — go through the runtime so the
write barrier lives in one place; an old object that has a reference stored
into it joins the remembered set, which the next minor collection scans as
roots.

**A slice is one object with room to spare, written in place when nothing else
can see it** — the Wasm backend's rule (§7), on a different heap. The second
header word holds the length in its low half and the capacity in its high
half; only the first `len` slots are elements, and they are all the collector
traces and all `==` and the renderer walk. Every slice local a function writes
into has an `i8` *owned* flag, set when the function made the slice itself (a
literal, a range, `keys()`, or the copy a write made) and cleared wherever the
local is read in a way that can keep the reference — the Wasm backend's list,
plus an `Unwrap` of a non-optional, which is a move here. `kite_rt_slice_push`
and `kite_rt_set_index` take the flag: owned with room, they write the slot
and return the same object; otherwise they copy — twice the length plus four
for a push, exactly the length for a write — and the local owns the copy. A
hundred thousand pushes went from four seconds to a few milliseconds, and a
store of a reference into an owned slice that has been promoted is the second
thing the write barrier covers. `tests/differential.rs` runs
`slices-are-values` and `slices-grow-in-place-natively` on all three backends,
and natively again with a nursery of one page.

A variadic construction stages its operands in a fixed window of 4,096 words,
`KITE_RT_STAGE`, before the runtime call that allocates. A slice or map
literal longer than that is built a window at a time: the slice is allocated
at its full length and each later window appended in place, and each window of
a map is added to a copy by the literal's own rule, so a key repeated across
windows is one entry.

Roots come from Cranelift's stack maps. Every reference-typed local is declared
as needing one, so at each safepoint — a call — the live references sit in
stack slots the maps record, and are reloaded afterwards, which is what lets
the nursery move them. A local assigned once and read only later in the same
block is carried as the value that defined it, declared as needing a map
itself, rather than through a Cranelift variable: the SSA builder keeps a table
per variable as long as the function has blocks, and a debug build splits a
block at every checked `+`, so a variable per local made a function of tens of
thousands of `let`s cost gigabytes to compile. At collection time the runtime walks the frame-pointer
chain and visits the recorded slots of every frame whose return address is a
registered safepoint. Because Kite has no `unsafe`, no pointer arithmetic, and
no FFI that hands out raw addresses, every reference is known to the collector —
conservative scanning is never required.

A native program runs on a thread of its own, with a stack of 512 MB reserved
rather than used (`kite_rt_run`, which the exported `main` calls), and every
compiled function counts itself into `KITE_RT_DEPTH` on entry and out on
return. The call past 100,000 traps with the VM's `call depth exceeded`, so the
two backends end a deep recursion at the same call; before, the native one
ran to the end of whatever stack it had been given and aborted there.

That walk is also why there is no native backend on Windows: Cranelift's Win64
prologue puts the frame record where the walk does not expect it, and
`--native` refuses there rather than corrupting the heap.

Cranelift's tradeoff is accepted deliberately: roughly 20% faster code generation
than LLVM, less optimised output. For the programs Kite is for — where a build
runs many times a day and the hot path is usually the browser's work rather than
the module's — that is the right side of the trade. An LLVM backend for release
builds stays possible and unplanned.

---

## 9. Bytecode backend

A register-based VM (in the Lua 5 / Dart tradition, not stack-based). Register
machines execute fewer dispatches per operation, and MIR's numbered locals map
onto registers almost directly.

Purpose:

- **Fast dev loop** — `kitec run` compiles to bytecode and runs it, with no
  code generator to wait for.
- **Compiler test oracle** — differential testing against the Wasm and native
  backends catches codegen bugs that no single backend would reveal. It is the
  specification for everything observable: float formatting, map ordering, trap
  messages. The native runtime transcribes it deliberately.
- **Embedding** — the VM takes its host functions through a trait, which is how
  `@host("fs")` resolves under `kitec run` and what a Rust program embedding
  Kite would implement.

Two things the first design promised are not built. There is **no REPL**. And
there is **no `.kbc` file format**: `--emit kbc` prints a listing of the
bytecode for reading, nothing is written to disk in a versioned form, and
nothing loads one back.

The VM does not share `kite-rt`'s collector or scheduler. Its values are its own
tagged enum, with aggregates behind `Rc` — so a cycle of references is never
freed while the program runs ([SPECIFICATION §14](../SPECIFICATION.md#14-memory-model))
— and it runs tasks on its own cooperative loop. The two runtimes agree on behaviour because the differential
suite makes them, not because they share code.

---

## 10. Diagnostics infrastructure

```rust
pub struct Diagnostic {
    pub severity: Severity,
    pub code:     Option<Code>,       // E0301 — stable, documented
    pub message:  String,             // one line, lowercase, no period
    pub labels:   Vec<Label>,         // the primary span, then secondary spans explaining *why*
    pub notes:    Vec<String>,
    pub fixes:    Vec<Fix>,           // machine-applicable text edits
}
```

What holds these to their rules:

- **One diagnostic per cause.** A missing brace produces one error, not forty.
  The compile-fail corpus asserts not only that each expected code is reported
  on its line, but that no diagnostic appears on a line that did not ask for
  one.
- **Secondary spans say why.** A type error names the parameter or return type
  that created the expectation, where there is one.
- **`--explain E0301`** prints the full rationale, including *why* the rule
  exists, and a test fails if any code lacks one. An unknown code prints the
  whole list.
- **`kitec fix`** applies every machine-applicable suggestion.
- **Source maps** are emitted for the Wasm target so browser stack traces name
  `.kite` files and lines — one entry per function, at the line it was declared
  on, because a MIR instruction has no span to do better with.

The renderer's output is pinned by exact-output tests in `kite-diag`, so
changing the format is a reviewable change to a test rather than a drift.

---

## 11. Test strategy

| Layer | Method |
|---|---|
| Lexer / parser | Unit tests per crate. `kitec fmt --check` over every `.kite` file in the tree on CI, and formatting twice must equal formatting once |
| Parser recovery, types, flow | `tests/corpus/*.kite` with `//~ E0301` annotations, rustc-style: the expected code on that line, and nothing on a line that did not ask |
| Exhaustiveness | Unit tests of the missing-pattern reconstruction |
| Codegen | Differential: run the same program on all three backends, assert identical output |
| Wasm validity | `wasmparser` over every emitted module |
| Wasm runtime | Executed under Node — the differential, host, string and DOM suites. Skipped with a message where Node is absent. No browser runs the suite |
| Diagnostics | The renderer's exact output; every code has an explanation |
| Standard library | `tests/std/` programs run on the VM and on Wasm and compared; doc-comment code fences extracted and run |
| Size | Four budgets in `size.rs`, and one for the site's own program |
| Documents | Every example on the site, the specification's Appendix A, and every example in `skills/kite` |

The differential codegen test is the highest-value one. Three independent
backends producing identical results is a strong signal, and it is the reason the
bytecode VM is worth building even though it is not needed for shipping.

---

## 12. Build performance targets

The targets, because they are the reason for skipping LLVM:

| Operation | Target |
|---|---|
| Full build, 10k lines | < 1s |
| Incremental, single function edited | < 50ms |
| LSP completion response | < 30ms |
| `kitec run` (bytecode) startup | < 20ms |

Nothing on CI measures them. And with no query engine (§1), the second row is not
met by design: an edit re-checks the whole file and everything it imports. What
keeps an editor usable is that a full check is fast — the site's own program,
with the standard library modules it imports, checks in under a tenth of a
second — but that is a property of today's programs rather than a design that
holds as they grow. If these regress, the architecture has gone wrong somewhere
and it is worth stopping to find out where; an incremental engine is the answer
this document once promised, and retrofitting one is the rewrite it warned
about.
