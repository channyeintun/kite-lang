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

**Also done since:** `[T]`, `{K: V}`, `Option<T>`, tuples, and `dyn Trait`
dispatch on both backends.

**Also done:** string interpolation. A hole is parsed as an ordinary
expression by a sub-parser given the file and a byte range, so a syntax error
inside one points into the string rather than at the whole literal.

**Also done:** generics on functions, with monomorphisation. Type arguments
are inferred from the arguments passed — there is no turbofish — and bounds say
what a parameter can do. Specialisation happens on HIR, before lowering, so
neither backend ever sees a `Param`: MIR, the bytecode VM and the Wasm backend
all work on concrete types and none of them knows generics exist.

One shortcut is worth naming. A method called on a bounded parameter lowers to
a *virtual* call rather than a direct one. It is correct — after specialisation
the receiver is concrete and the vtable finds the right body — but the tag
comparison is avoidable, and devirtualising after monomorphisation is a
worthwhile follow-up.

**Also done:** closures. The body is lifted into a function of its own whose
leading parameters are what it captured, so nothing after the type checker
knows closures exist. Captures are by value, taken when the closure is made —
which is why a returned closure works, and why capturing a `var` is refused.

In WebAssembly a closure is a record of a typed function reference and an
opaque environment. Two closures of one Kite type capture different things, so
the environment cannot be in the shared signature; each lifted function gets a
thunk that casts the environment back to its own record and unpacks it. Calls
go through `call_ref`, and the thunks are named in a declarative element
segment because `ref.func` may only reference a function declared for it.

**Also done:** type parameters on structs and enums. A generic declaration is
a template rather than a type — `Box` says nothing about what it holds — and
each set of arguments gets a definition of its own with the parameters
substituted away. Nothing past the type checker knows: a `Box<int>` is a struct
like any other, with no boxing, no tag and no dispatch.

Arguments are inferred from the values, as for functions, because `<` in
expression position is a comparison and Kite has no turbofish. Where inference
has nothing to work from — `Maybe.None` — the binding must be annotated, and
E0209 says so.

Three things fell out of the recursive case, `struct Tree<T> { children:
[Tree<T>] }`. A specialisation may be asked for while its own template is still
being filled in, so every one is recomputed once the declarations are complete.
Substitution has to reach through a specialisation's own arguments, which needs
a record of what each was made from. And `Box<Box<int>>` ends in a token the
lexer read as a shift; the parser splits it rather than making the lexer care
about types.

**Also done:** methods on generic types. `impl<T> Box<T>` is written once
against the parameters and specialised per receiver — the type arguments come
off the receiver's own type, so there is nothing at a call site to infer. An
associated function has no receiver, so its arguments come from the type the
result is used as: `let s: Stack<int> = Stack.empty()`.

**Phase 2 is complete.**

---

## Phase 6 — Standard library (started)

`std/prelude.kite` is compiled into every program, ahead of its own source. It
is written in Kite rather than in the compiler, which is the test a standard
library should have to pass: a prelude needing compiler support would be
evidence the language was missing something. Nothing in it does.

It holds slice combinators — `map`, `filter`, `fold`, `any`, `all`, `count`,
`find`, `first`, `last`, `reversed`, `concat`, `take`, `drop` — numeric helpers,
and `or_else` for optionals. It is deliberately small: each name is one every
program must live with, and a name taken is hard to give back.

Two things made it workable. A program's own definition **shadows** the
prelude's, because there is no module system yet to qualify one and a prelude
that could not be shadowed would make every name in it permanently unusable.
And unreachable functions are **dropped** before code generation, so a program
using none of it pays nothing: `hello.kite` is 469 bytes with the prelude in
the compilation, against 6,562 for a program that uses most of it.

**Also done:** guard-clause narrowing. `if x == nil { return }` leaves only
the path where `x` is present, so it reads as a `T` for the rest of the block —
the shape people actually write, and one the standard library needed twice
before it existed. The narrowing ends with the block that guarded it.

**Also done:** `str.len()` and `as` casts between `int` and `float`. Casts are
saturating rather than trapping on both backends, so a value out of range or a
NaN gives a number rather than killing the program, and the two agree on every
input.

**Also done:** strings. `str` has four methods, all host calls: `len`, `slice`,
`index_of` and `trim`. Everything else — `contains`, `starts_with`,
`ends_with`, `split`, `join`, `replace`, `words` — is written in Kite on top of
them, which is where it belongs. A host call is a boundary two runtimes have to
agree about, and every one added is a thing that can drift.

They count characters rather than bytes, on both sides: the VM walks a `char`
iterator and the glue spreads with `[...s]`, so `"héllo日本"` is seven either
way and `index_of` returns a position a caller can pass back to `slice`.

**Also done:** the module declares only the imports it reaches for. The import
list had grown to seventeen host functions, and a `hello world` was carrying
string slicing and a font metric it never asks about. It is now 426 bytes —
smaller than before any of them were added.

One library file shadowing another is now an **error** rather than a silent
drop. A program shadowing a library name is fine and stays silent — that is
what a prelude is for — but a library shadowing a library leaves the first file
calling a name that no longer means what it did, which is a bug in the standard
library and has bitten twice.

**Also done:** `Display`. `io.print` and `\(x)` interpolation both look for
it, so implementing it once makes a type printable everywhere — and the two
cannot disagree, because printing goes through interpolation.

The trait is declared in the prelude, not in the compiler. The checker finds it
by name, which is what lets a program define its own and what stops the
compiler from having an opinion about how anything reads. It is deliberately
**not** derived: a mechanical answer would be wrong more often than right, and
a `Password` whose derived form printed its field is exactly the case where
being wrong matters.

**Remaining:** real modules, so a library's names can be *qualified* rather
than shadowed at all; map methods.

---

## Phase 7 — Layout (started)

`std/ui.kite` is a subset of flexbox, written in Kite. It computes where things
go and draws nothing — the same layout can feed a DOM renderer and a canvas
renderer, and neither can disagree with the other about where a box ended up
because neither decides.

The model is deliberately *one* algorithm. CSS has block, inline, float, table,
flex and grid, and most of the difficulty of writing CSS is knowing which is in
force. Kite has boxes in a row or a column: fixed or content sizing, `grow` for
the leftover, `justify` along the main axis, `align` across it, padding and
gaps. That is what every UI toolkit designed after the web converged on, and it
is enough for application UI.

`layout(root, viewport)` returns one `Frame` per node in paint order, in
absolute coordinates, and `hit(frames, x, y)` reads the same list backwards.

It is opt-in through `use std/ui`, because it declares types with ordinary
names — `Size`, `Rect`, `Node` — and a program that never mentions layout
should not have to avoid them. That is file-level opt-in rather than a module
system: the names arrive unqualified and a program cannot ask for some and not
others.

**Text is measured by the host**, through `text.width`, because only the host
has the font — and two hosts can legitimately disagree, which is what different
fonts *are*. A browser measures with `measureText` in the font it will draw
with, so the layout matches what is painted. A runtime with no font answers
with a nominal advance per character; the bytecode VM and the generated glue
use the same one, so a layout stays comparable across backends under test.

`text.height()` is the same arrangement for the other axis: the font's ascent
plus descent plus leading, which is what a line actually occupies. A canvas
reports the first two, and the fall back to a nominal value matters —
`fontBoundingBox*` is not universal, and a layout that produced `NaN` would
place everything at zero.

### Wrapping

A text node wraps when it has been **given a width**, and not otherwise. Which
width to wrap to is only knowable once a parent has decided, and measurement
runs before that — so a node that wants to wrap says how wide it is, and one
that does not is measured as a single run.

Flexbox answers this with a second measurement pass after the first layout.
That is more capable and much harder to predict, and predicting it is most of
what makes CSS difficult.

The wrapper is greedy and word-based. It is **not UAX #14**: it will not break
a long URL, and it knows nothing about the rules for CJK, where a line may
break between almost any two characters. Both need the line-breaking property
table, which belongs with the canvas renderer.

A `Frame` carries its outer rect and the rect inside its padding. Without the
second, a leaf's padding would size its box and then be ignored when its text
was drawn — visible as text flush against a box that was clearly wider than
it.

### Rendering

A program draws through exactly two host calls, `draw.rect` and `draw.text`.
Everything a layout produces is a rectangle or a run of text, and a boundary
that stays that narrow can be met by more than one renderer — which is the only
way two renderers can be made to agree.

`kitec build … --emit wasm` writes `app.wasm`, `app.js` and an `index.html`
that runs the module against a **DOM renderer** (absolutely positioned
elements), a **canvas renderer** (`fillRect` and `fillText`), or a **text
renderer** that writes each call out. All three are in the same page, switched
live, against the same compiled module: the program cannot tell which is
running. The text renderer writes what the bytecode VM writes, so drawing is
covered by the differential suite without a browser.

Switching between DOM and canvas in that page shows the text-measurement
placeholder for what it is: the canvas clips a label that the DOM lets
overflow, because `nominal_advance` says 8 units per character and the real
font disagrees. That is the honest state of it until Phase 8.

### Applications

A program that exports `init`, `view` and `update` instead of `main` is an
application, and the generated page drives it: a click becomes an event with a
position, `update` returns a new model, and `view` draws it.

The model never crosses the boundary as data. It is a Wasm reference the host
holds and hands back, opaque to JavaScript — which is what lets a model be any
Kite type at all without a representation both sides have to agree on, and why
`update` returns a new model rather than changing one. Kite has no mutable
global state, so this is the only shape an application could have taken; it is
also the shape every state-management library eventually converges on.

Every `pub` free function is now exported. Method names are not unique across
types and a module may not export one name twice, so methods are not — nor are
generic specialisations, which would all want the same name.

Events all come through one door: `update(model, event, x, y, key)`. A click
fills the position and leaves the key empty; a key press fills the key and
leaves the position at zero. One signature rather than one export per kind
means a new kind of event is a new constant, and a program that ignores a kind
simply never matches on it.

A `str` crossing into an export has to be interned first. It is an index into
the module's string table, not a pointer and not a JavaScript string — and
handing an export a JavaScript string does not fail, it runs `ToNumber`, gets
`NaN`, and reads index 0. The glue exports `str()` and `text()` so nothing has
to know that.

### Scrolling

Scrolling is not a layout concern. A tree is laid out at its natural size and a
viewport decides which part is visible — so scrolling changes nothing about
where anything *is*, only about what is drawn. Hit-testing works on the same
frames: subtract the offset from the point rather than re-laying anything out,
which is one subtraction instead of one per frame.

Frames outside the viewport are skipped rather than drawn and thrown away.

This is the one place the drawing boundary had to grow, from two calls to
four. `draw.clip` and `draw.unclip` cannot be built out of fills: a half-visible
row has to be cut by the renderer, and painting a rectangle over it would erase
whatever it is scrolling past. The two renderers diverge most here — the DOM
one nests an `overflow: hidden` element and shifts its children's origin, the
canvas one pushes a path with `save`/`clip`/`restore` and moves nothing — and
they still produce the same picture.

**Remaining:** pointer events beyond a click and a wheel, and incremental
redraw — `view` repaints the whole tree.

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

## Phase 4 — WebAssembly backend ✅ **complete, less JS String Builtins**

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
4. `str` as `externref` with JS String Builtins — **not done**. A `str` is a
   constant index into a table the glue holds, which needs no linear memory at
   all and costs one call per operation. Builtins would make a string a real JS
   string and the boundary free; it is an optimisation, not a gap.
5. ~~Trait objects with typed function-reference vtables~~ ✅ — a tag and a
   dispatcher per method, because WasmGC compares types structurally and
   `ref.test` cannot separate two structs of the same shape.
6. ~~`extern` declarations driving the glue~~ ✅ — `@host("net") extern fn`
   becomes an import, the generated glue declares the group, and a page
   supplies it. The compiler's own builtins are still a fixed list, which is
   the honest shape of it: those are the language's, not the program's.

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

Maps lower as a record holding parallel key and value arrays, and lookup is a
linear scan — which is what makes insertion order and first-match-wins
obviously right. A hash index is an optimisation for later. A write builds new
arrays and rebinds, so maps keep value semantics; one code path covers both
replacing a key and appending one, because the scan yields either the key'''s
index or the current length.

A trait object is a reference to a one-field root record holding the concrete
type's identity, and each trait method gets a dispatcher that compares that
tag and calls the matching implementation.

The tag is not redundant with `ref.test`. WasmGC compares types
*structurally*, so `struct Circle { r: float }` and `struct Square { s: float }`
are **the same** Wasm type — a nominal distinction in Kite is no distinction at
all down here, and a cast-based dispatch would silently pick the wrong body.
Only types that appear in some vtable carry the tag, so a program without `dyn`
is byte-for-byte what it was before trait objects existed.

Structural equality on aggregates is a generated function per type, because
Wasm has no deep-equality instruction. A struct compares fields, an enum
compares tags then payloads, a slice compares lengths then elements, an
optional compares presence then payloads. Each returns at the first
difference, and the functions call each other, so a struct holding a slice of
structs compares correctly at any depth. They are emitted only for types a
program actually compares.

**All twelve examples compile to WebAssembly and agree with the bytecode VM,
and every construct the language has now lowers.** `--emit wasm` refuses
nothing the language can express. The scan that would report a gap is still
wired in, because a backend that quietly emits a trapping module for something
it cannot do is the failure that check exists to prevent. The rvalue match in the backend has **no catch-all**, so adding a
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

## Phase 5 — Concurrency ✅ **complete, except real parallelism**

`async fn` compiles to two ordinary functions: a **starter** that allocates the
frame, hands the scheduler a resume closure and returns the task, and a
**resume function** that is the original body with an entry dispatching on where
the last suspension left off. It is one MIR pass, so both backends see ordinary
functions, ordinary structs and an ordinary closure, and neither knows
concurrency exists.

MIR being a CFG already is what made this small: the hard part of the usual
transform — recovering the resume points — is free when a suspension is a block
boundary. Locals are spilled and reloaded around a suspension rather than
rewritten into frame fields everywhere; a live-range analysis would spill fewer
and getting it wrong would be a miscompile, so that is a later optimisation
with a correct starting point.

`Task<T>` is a struct the compiler declares, generic like any other, so
substitution and monomorphisation carry it for free and `both<A, B>` works. Its
fields are named `$done` and `$value`, which no program can spell.

Four primitives — `task.yield`, `task.park`, `task.wake_at` and `time.now`,
plus `task.wait_host` for a fetch — and every combinator in `std/task` is Kite
written on top of them: `both`, `all`, `race`, `sleep`, `timeout`, `parallel`,
`scope`.

Two things the scheduler had to get right:

- **An `await` that finds its task unfinished parks.** An awaiting task is
  otherwise runnable on every sweep, and a scheduler cannot tell that from
  progress — so one sleeping task would spin the program forever.
- **A completion wakes everything**, because that is what a parked task was
  waiting for. Without it a `timeout` would sleep through the result it was
  waiting for.

The clock is **virtual** on both backends: when every task is waiting on a
deadline it jumps to the earliest. A program that sleeps costs no real time
under test, and the two backends produce the same interleaving — a scheduler
racing real timers could not be differentially tested.

**Exit criterion, honestly:** three URLs fetched concurrently on web works and
is tested under Node; `E0520` names the mutable field responsible, not just the
type. **What does not work is real parallelism, on any target.** A WasmGC
reference cannot cross a thread boundary until shared-everything-threads ships,
and the bytecode VM's values are `Rc`-based rather than `Send`. `task.parallel`
therefore interleaves rather than parallelises today. The *rule* is real and
enforced now, which is the forward-compatibility decision that mattered: the
day either restriction lifts, the same source starts using cores.

---

## Phase 6 — Standard library core ✅ **complete, less `decode<T>`**

`std/math`, `std/time`, `std/errors`, `std/fmt`, `std/json`, `std/test`,
`std/buffer`, `std/task`, `std/http`, `std/crypto` and `std/ui` — all written
in Kite, all reached through `use`, all documented by `kitec doc` from their
own source.

**Modules landed first**, because the library needed them. A module is a
directory, and its declarations are merged *qualified*: `load` in module
`config` is declared as `config.load`. A dot cannot appear in an identifier, so
the qualified name is unforgeable and is exactly what an importer writes — which
is why diagnostics need no demangling and the rest of the compiler needs no
notion of a module beyond "which one am I in". `pub` finally means something:
reaching for an unmarked declaration across a module boundary is E0401.

Writing `std/json` — a parser, a writer, a pretty-printer and accessors — found
four things the language now does, which is exactly what this phase was for:

- **Returning a fallible call's result directly**, `return parse_user(raw)`,
  which the specification's own example does and the checker refused.
- **Match arms coerce into an expected optional**, so `Text(s) => s` and
  `other => nil` are arms of one `Option<str>`.
- **Reading an error counts as checking it.** E0302 exists to catch the error
  nobody looked at, not the one handed to `errors.wrap`.
- **Every accessor takes an optional and yields one**, which is what makes them
  chain without a `?.` operator: `field(field(doc, "a"), "b")` reads left to
  right and the branch is taken once, at the end.

Maps grew `keys()` and `values()` and, with them, `for (k, v) in m` — lowered
to a loop over the keys with the value looked up per key, which is what a
reader would write by hand and keeps insertion order.

`kitec test` runs every `test_` function in a file. Assertions return errors
rather than trapping, so one failure does not stop the rest.

**Exit criterion, honestly:** the library's own tests are ordinary Kite
programs in `tests/std/`, and they run on the bytecode VM *and* on WebAssembly
under Node with the outputs compared. The third backend does not exist, so
"all three" is two.

**Remaining:** `json.decode<T>` — a document straight into a user's struct —
needs compile-time derivation, and so do `Eq`, `Hash` and `Debug`. None of that
machinery is built. Until it is, a document is taken apart with accessors that
each yield an optional, which is more typing and no less safe.

---

## Phase 7 — Layout engine and DOM renderer 🟡 **the loop works; the diff does not**

Done: the flexbox subset in `std/ui.kite`, the Elm-shaped update loop, the DOM
renderer, and events — click, key, wheel, pointer move, down, up, and resize,
all through one door.

A frame is **recorded before it is painted**, and an identical one is not
painted at all: a pointer moving over nothing costs one comparison rather than
a rebuilt tree. That is the half of damage tracking a model that did not change
needs. Finding *which* rectangle changed needs a retained scene graph that
survives between frames, and that is not written.

**Remaining:** the retained scene graph and its diff; a widget set (there are
boxes and text, and everything else is a program's own); keyboard navigation
and focus; validation against Taffy's fixtures. Layout is over ordinary slices
rather than flat buffers, which is the shape `buffer.F64` exists to change and
has not yet.

---

## Phase 8 — Canvas renderer 🟡 **it draws; text is where it stops**

Done: `canvasRenderer` over Canvas2D, drawing the same four calls the DOM
renderer takes and switchable live in the same page against the same module.
Text is measured through `measureText` in the font that will be drawn, so
layout matches what is painted.

**Hidden-overlay text input** is done: typing on the canvas path goes to a real
input positioned where the pointer was, which is the trick every canvas editor
uses and what brings up the on-screen keyboard on a phone.

**A parallel tree** is done, and is deliberately not called an accessibility
tree: the same runs of text, in the same order, in hidden DOM beside the
canvas, so a screen reader is not left with a picture. There are no roles, no
focus and no live regions.

**Line breaking is a named subset of UAX #14**: Latin breaks between words, CJK
between characters, and which characters are wide is *measured* rather than
tabulated — the host owns the font. It will not break a long URL, knows nothing
of non-breaking spaces, and will leave a closing bracket at the start of a
line.

**Remaining, and it is most of the phase:** HarfBuzz-quality shaping, UAX #9
bidi, a glyph atlas, per-rectangle damage tracking, WebGPU, and the
golden-image tests against the DOM renderer across Latin, Cyrillic, Arabic,
Hebrew, Devanagari, Thai, CJK and Burmese. Text is the reason this was called
the hardest phase, and it still is.

---

## Phase 9 — Native backend ❌ **not written. `kitec bundle` is packaging, not codegen**

What exists is a **bundle**: `kitec bundle app.kite` writes one executable that
is this compiler with the program appended to it. Running it finds the program,
compiles it and runs it — under a millisecond, with no linker, no runtime to
unpack and nothing to install. That solves distribution, which is what most
people mean when they ask for a native binary, and the roadmap's start-up
criterion is met by a wide margin.

It is **not** a third backend, and calling it one would be the kind of
overstatement this document exists to avoid. The program still runs on the
bytecode VM. There is no machine code, no Cranelift, and no collector.

What the real backend needs, unchanged from the original plan:
`kite-codegen-clif` over Cranelift, and a precise generational collector in
`kite-rt` — stack maps at safepoints, precise root scanning, a nursery with a
bump allocator, and a write barrier for the old generation. The GC is the real
work, which is why this is the phase the plan says to cut first: the bytecode
VM already covers native execution adequately, and Cranelift is a performance
upgrade rather than a capability.

**Exit criterion, unmet:** the differential corpus runs on two backends, not
three.

---

## Phase 10 — Tooling 🟡 **all but the package manager**

| Tool | State |
|---|---|
| `kitec fmt` | ✅ token-based, comment-preserving, and the whole tree passes `--check` |
| `kite-lsp` | ✅ diagnostics, hover, definition, completion, symbols, formatting |
| `kitec test` | ✅ every `test_` function, with failures as values so one does not stop the rest |
| `kitec fix` | ✅ applies the machine-applicable suggestions diagnostics carry |
| `kitec doc` | ✅ the reference, from `///` comments, with signatures read from the parse |
| `kitec bundle` | ✅ one executable that needs nothing installed |
| `--explain` | ✅ |
| `kite pkg` | ❌ not started — no manifest, no lockfile, no dependency resolution |

The formatter works on **tokens**, not the tree: a tree has dropped the
comments and the blank lines a formatter must keep. It decides indentation,
spacing and blank lines, and leaves where the lines end to the author — the
same bargain `gofmt` makes. Running it over the tree found six bugs in it,
which is the argument for running a formatter on real code before believing it.

`kite pkg` is what remains, and with it the manifest in
[spec §13.2](../SPECIFICATION.md#132-manifest): a lockfile with content hashes,
no post-install scripts, and no way for a dependency to execute code at build
time.

---

## Phase 11 — Networking 🟡 **the client works; the server has no sockets**

`std/http` is five host declarations and the rest Kite. Three requests started
together take as long as the slowest, which the tests demonstrate under Node
against `data:` URLs. A 404 is a `Response`, not an error — the request
succeeded and the answer was "no" — and only a transport failure is an `error`.

Waiting is `task.wait_host()`, which is its own thing: an awaiting task parks
for another task, and a fetching one must let the event loop run. Saying which
is what stops the scheduler spinning through a promise that cannot resolve
while it holds the thread.

The **server half is the part that needs no sockets**: `Request`, `Response`, a
router with `:name` captures, and handlers that are functions and can be tested
by calling them. What is missing is the boundary underneath — WASI's
`wasi:http/incoming-handler`, or a Node adapter — so nothing listens on a port
yet.

A web language with no way to talk to a server is a toy, and the two halves are
not symmetric: the client runs in a sandbox that already has `fetch`, and the
server runs somewhere with sockets.

**Client.** `http.get`, `http.post`, and a request builder for the rest. It
lowers to the host's `fetch`, which means it is `async` — so this cannot land
before Phase 5. Every call returns `(Response, error)` like anything else that
can fail; a 404 is a `Response` with a status, not an error, because the request
succeeded and the answer was "no". Only a transport failure is an `error`.

**Server.** A `Handler` trait and a router, on top of a host boundary the
runtime supplies: WASI's `wasi:http/incoming-handler` where it exists, and a
Node or Deno adapter where it does not. The same `Request` and `Response` types
as the client, so a handler can be tested by calling it directly and a proxy is
not a special case.

**Deliberately absent at first:** no middleware stack, no dependency injection,
no ORM. A handler is a function from `Request` to `Response`; anything that
wants to wrap one can be a function that takes a handler and returns a handler,
which needs no framework.

The hard part is not the API. It is that TLS, HTTP/2 and connection pooling all
live below the boundary, and Kite should not reimplement any of them — the host
has them, and reaching for them is the whole reason the boundary is declared.

---

## Phase 12 — Cryptography 🟡 **hashing, HMAC, PBKDF2 and randomness**

`std/crypto` binds SHA-256/384/512, HMAC-SHA-256, PBKDF2 password hashing with
a generated salt, cryptographically secure random bytes, and a constant-time
comparison. Comparing a secret with `==` is a warning (E0600) that says to use
`crypto.equal`.

**Remaining:** AES-GCM, Ed25519 and X25519 — each is another few declarations
over WebCrypto, and each needs key handling that has not been designed. Argon2
is not in WebCrypto at all, so it waits on a runtime that has it.

The rules the module was built to enforce are in place: no ECB, no CBC, no MD5,
no SHA-1, no raw RSA; salts generated rather than passed.

**Bindings, not implementations.** Every constant-time guarantee a cipher makes
is a guarantee about generated machine code, and Kite compiles through WasmGC to
an engine that is free to reorder anything. A pure-Kite AES would look correct
and leak timing, which is worse than none.

So: `crypto` is a thin, declared boundary over WebCrypto in the browser and the
runtime's own primitives elsewhere. Random bytes, SHA-256/384/512, HMAC,
AES-GCM, Ed25519, X25519, and PBKDF2/Argon2 for passwords.

Three design commitments, all of them about removing choices:

- **No ECB, no CBC, no MD5, no SHA-1, no raw RSA.** A primitive that is
  dangerous by default is not offered, and the diagnostic when someone asks for
  one says what to use instead.
- **Nonces are generated, never passed.** The single most common failure in
  applied cryptography is a reused nonce, and an API that accepts one invites
  it.
- **Comparison of secrets is `crypto.equal`, not `==`.** Structural equality
  short-circuits, which is a timing oracle; the checker warns when a value that
  came from `crypto` is compared with `==`, the same way it warns on float
  equality.

---

## Phase 13 — Documentation and site ✅ **including the playground**

`site/` is four pages and two scripts, built by `site/build.sh`: the pitch, the
specification and the other documents rendered from their Markdown, the
standard library reference generated by `kitec doc`, and the playground.

**The playground is the compiler.** `kitec` is Rust and already targets
WebAssembly, so the page compiles and runs Kite in the same tab with no server
at all, and the diagnostics it shows are the ones a terminal shows because they
come from the same code. It runs, checks, formats, and shows any of the
intermediate forms.

The Markdown renderer and the syntax highlighter are written here rather than
fetched: a language's documentation should load on a bad connection, and a
dependency in the site is a dependency in the project. The highlighter is a
lexer rather than a pile of regular expressions — a keyword inside a string is
not a keyword.

**Every example on the site is compiled in CI**, and the playground's samples
are also *run*, so one that stopped working would fail the build rather than
sit there wrong.

The specification already exists and is the source of truth. What is missing is
somewhere to read it that is not a Markdown file on a git host.

- **`kite doc`** — extracts doc comments and produces the reference. The
  standard library is written in Kite, so its documentation and a user's are
  generated by the same tool, which is the only way the two stay comparable.
- **The site** — the specification, the reference, a tour, and a playground.
  Static, no framework.
- **The playground is the point.** The compiler is Rust and already targets
  WebAssembly; compiling `kitec` itself to Wasm means the site can compile and
  run Kite with no server at all, with the same diagnostics a terminal shows.
  A language whose site cannot run the language is asking to be taken on faith.
- **Every example on the site is compiled in CI**, so a documentation example
  that stops working fails the build rather than sitting there wrong.

---

## Phase 14 — Editor support ✅ **the server, and an extension over it**

`kite-lsp` answers over stdio: diagnostics as you type, hover, go to
definition, completion, document symbols and formatting. Every answer comes
from the same passes `kitec` runs — a language server that re-derives its own
is a second compiler, and the disagreement always surfaces as "the editor says
this is fine and the build says it is not".

The protocol is `Content-Length`-framed JSON, and both halves are written by
hand: the framing is six lines and the JSON two hundred, which keeps the
dependency list at nothing a build has to fetch. The VS Code extension has no
npm dependency either, not even the LSP client library.

**Remaining:** rename, find references, and inlay hints for solved generic
arguments — the last is worth more here than in a language with a turbofish,
because there is nowhere else to see what a call inferred.

**`kite-lsp`, then a thin VS Code extension over it.** The order matters: an
extension that implements its own analysis is one that only ever works in one
editor, and the same queries the compiler runs are the ones an editor needs.

- Diagnostics as you type, from the same `DiagBag` the CLI renders
- Go to definition, find references, hover types, completion
- Rename, using the resolver's binding table
- Format on save, once `kite fmt` exists
- Inlay hints for inferred types and for solved generic arguments — Kite has no
  turbofish, so seeing what a call inferred is worth more here than in a
  language that lets you write it

The extension itself should be a few hundred lines: launch the server, ship a
grammar, register the file type. Anything more belongs in the server.

**Syntax highlighting** ✅ — `editors/vscode/` has the grammar, a language
configuration, and a manifest. It colours all 27 keywords, the primitives,
string interpolation (`\(expr)` as embedded code rather than as string),
numeric literals in every base, `dyn Trait`, declaring positions, and dotted
calls.

A test in `kite-lexer` asserts that every keyword the lexer knows appears in
the grammar, so adding one to the language fails the build until it is
coloured. Highlighting drifting out of step with the language is the normal
failure here, and it is worth a test rather than a habit.

The same file is what a Linguist submission needs, so this is not work done
twice.

### Highlighting on GitHub

GitHub highlights through [Linguist](https://github.com/github-linguist/linguist),
and there are two ways in.

**Now, in this repository:** `.gitattributes` can map an extension onto a
language Linguist already knows.

```
*.kite linguist-language=Rust
```

Rust is much the closest fit. It shares `fn`, `let`, `match`, `struct`, `enum`,
`trait`, `impl`, `pub`, `use`, `as`, `type`, `self`, `dyn`, `async` and `await`
outright, and the shapes line up as well — `-> T`, `Option<T>`, `[T]` and
`struct X { field: T }` all parse the way a reader expects. What misses is
small: `var` and `check` are not Rust keywords, `nil` is `None` there, and
`\(x)` interpolation goes uncoloured.

Go was the other candidate and is a worse one: it shares perhaps ten keywords,
has no `match`, `trait`, `impl` or `enum`, and would colour `chan`, `go`,
`defer` and `select` — none of which Kite has, and three of which it
deliberately does not.

`linguist-detectable` makes `.kite` files count towards the repository's
language statistics.

**Properly, later:** adding Kite to Linguist needs a submitted TextMate grammar
with a permissive licence, a unique extension, and — the real gate — evidence of
use: Linguist asks for hundreds of repositories with the language in them before
accepting a new one. That is not a task, it is a consequence of adoption, and
the grammar written for VS Code is the same one the submission needs.

---

## Phase 15 — Distribution 🟡 **the pipeline exists; nothing is published**

`.github/workflows/release.yml` cross-compiles `kitec` and `kite-lsp` for macOS
(arm64, x86-64), Linux (x86-64 and arm64, static musl) and Windows on a tag,
with a checksum file. The builds are reproducible — the path prefix is remapped
and debug info dropped — because a compiler is exactly the artefact where
"trusting trust" is not hypothetical. `install.sh` downloads one archive,
verifies it against the release's own checksums, and refuses rather than warns
when they differ.

**Remaining:** nothing has been released, so none of it has run in anger. There
is no Homebrew formula, no Scoop manifest and no AUR package, and releases are
not signed beyond their checksums. `kitec` as a Wasm module exists — the
playground is it — but is not published as an artefact.

A compiler nobody can install is a compiler nobody uses.

- **Cross-compiled binaries** for macOS (arm64, x86-64), Linux (x86-64, arm64,
  and a musl static build), and Windows (x86-64), built in CI on tags.
- **An install script** and, in time, Homebrew, Scoop and the AUR. Not npm:
  Kite deliberately has no relationship with that ecosystem, and shipping the
  compiler through it would be the first thread of one.
- **`kitec` as a Wasm module**, which the playground needs and which makes the
  compiler runnable anywhere a browser is.
- **Reproducible builds** — same source, same binary — because a compiler is
  exactly the artefact where "trusting trust" is not a hypothetical.
- **Signed releases with checksums.**

The binary should stay one file with no runtime dependency. Kite ships no
garbage collector and links no LLVM, so there is no reason it should not.

---

## Where the implementation actually stands

Recorded honestly, because a roadmap that overstates progress is worse than
none.

| Phase | State |
|---|---|
| 0 — Specification review | ✅ done, and the spec was amended by what the code found |
| 1 — Vertical slice | ✅ complete |
| 2 — Type system | ✅ complete — structs, enums, match, exhaustiveness, traits, trait objects, slices, optionals, tuples, maps, interpolation, closures, generics on functions and types |
| 3 — Error handling | ✅ complete |
| 4 — WebAssembly backend | ✅ every construct the language has, on both backends, compared. ❌ JS String Builtins (an optimisation, not a gap) |
| 5 — Concurrency | ✅ `async`/`await`, the state machine, `Task<T>`, the combinators, `Share`. ❌ real parallelism on any target — the platform forbids it today |
| 6 — Standard library | ✅ modules, and ten of them written in Kite, tested on both backends. ❌ `json.decode<T>` and the derivation machinery it needs |
| 7 — Layout and DOM renderer | 🟡 layout, events and the update loop; a frame that did not change is not repainted. ❌ retained scene graph, widget set, focus |
| 8 — Canvas renderer | 🟡 draws, measures in the real font, hidden-overlay input, a parallel tree for a screen reader, UAX #14 subset. ❌ shaping, bidi, glyph atlas, per-rectangle damage, golden images |
| 9 — Native backend | ❌ not written. `kitec bundle` produces one self-contained executable, which is packaging rather than code generation |
| 10 — Tooling | 🟡 fmt, doc, fix, test, bundle, `--explain`, and the language server. ❌ `kite pkg` |
| 11 — Networking | 🟡 the client, tested end to end under Node; the router and types for the server. ❌ anything that listens on a port |
| 12 — Cryptography | 🟡 hashing, HMAC, PBKDF2, randomness, constant-time comparison, and E0600. ❌ AES-GCM, Ed25519, X25519 |
| 13 — Documentation site | ✅ four pages, the reference generated from the library, and a playground that is the compiler |
| 14 — Editor support | ✅ the language server and a VS Code extension over it. ❌ rename, references, inlay hints |
| 15 — Distribution | 🟡 CI, cross-compiled release builds with checksums, an install script. ❌ nothing published yet |

543 tests: unit tests per crate, an annotated compile-fail corpus, a
differential corpus that runs every program on both backends and compares, the
standard library's own suite on both backends, the host boundary under Node,
and every example on the site.

### What is deliberately not done

Three things are absent on purpose rather than pending, and each is recorded
where the decision was made:

- **Real parallelism.** WasmGC references cannot cross a thread boundary, and
  the VM's values are `Rc`-based. `Share` is enforced *now* so the day either
  changes, no source does.
- **A third backend.** Cranelift plus a precise collector is the largest single
  piece of work left, and the plan says to cut it first for a reason: the
  bytecode VM covers native execution, and `kitec bundle` covers distribution.
- **`json.decode<T>`, `Eq`, `Hash`, `Debug`.** All four want the same
  derivation machinery, and building it for one of them alone would be building
  it twice.

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
