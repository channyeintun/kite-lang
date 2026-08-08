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

## The direction changed at Phase 16

Read this before Phases 7 and 8, because those two describe a design that is no
longer the plan.

Phases 1–15 built a language and a UI layer. The UI layer computed its own
flexbox in Kite and handed both renderers the same rectangles, so a program
looked identical on the DOM and on a canvas. That property was real, it was
expensive, and it turned out to be the wrong thing to buy.

**What it cost was the web.** The DOM renderer drew absolutely-positioned
`div`s, styled inline, under names it generated itself, with every box
`aria-hidden` and the document's structure carried by a parallel semantics tree
rather than by real elements. Nothing in a Kite application was a `<button>`.
Which means **no stylesheet written by anyone else could address any part of
it** — not Tailwind, not Bootstrap, not a company's own tokens. A Kite
application could only be styled by Kite.

That put the project in Flutter Web's position: a competitor to Flutter Web,
not an alternative to JavaScript. It is not the position this language wants.

The new direction is the web's own division of labour:

> **HTML is the document. CSS styles it. Kite replaces JavaScript, and nothing
> else.**

Four decisions follow from it, and they are decisions rather than guesses
because each one is what interoperating with somebody else's CSS actually
requires:

1. **Real tags.** A stylesheet targets `<button>`, `<input>`, `<table>`. A tree
   of `div`s takes none of it.
2. **Class names pass through unchanged.** No hashing, no scoping, no rewriting.
3. **Light DOM only.** Blocking outside stylesheets is what a shadow root is
   *for*.
4. **State lives in attributes.** `:hover`, `:focus-visible`, `:checked`,
   `[data-state]`. Hover stops being something the model tracks and the program
   repaints; it stops crossing into Wasm at all.

And one decision about where a Kite program sits: **inside a page, owning parts
of it**, rather than owning the whole page as a single-page application. The
general case contains the special one — attaching to `<body>` is the SPA — and
it makes the cost of downloading and starting a Wasm module optional rather
than unavoidable. A page can be served complete and have Kite enhance the three
parts that need real logic.

What this deletes: `std/ui`, the Material package, the canvas renderer's
parallel-tree machinery, and the frame recording and damage tracking that
existed to make painted output comparable. Roughly 14,000 lines. What it keeps:
the language, all three backends, the standard library, the tooling, and canvas
— as a real `<canvas>` element a program draws into, which is what a canvas is
on the web.

**Layout in Kite is not deleted; it is descoped.** It was written for a renderer
that had to agree with a second renderer. Where a program still paints its own
pixels — a chart, a game, a native window — that problem is unchanged and the
answer is still the one Phase 7 built.

Phases 7 and 8 are left standing below as the record of what was built and why.
They are marked superseded rather than rewritten, because a roadmap that
quietly edits its own past is not a roadmap.

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

**Goal:** `kitec run hello.kite` prints output, executed by the bytecode VM.

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
`index_of` and `trim` — `code_at` became the fifth further down this same
phase, when `Hash` needed one, and is the only one added since.
Everything else — `contains`, `starts_with`, `ends_with`, `split`, `join`,
`replace`, `words` — is written in Kite on top of them, which is where it
belongs. A host call is a boundary two runtimes have to agree about, and every
one added is a thing that can drift.

They count characters rather than bytes, on both sides: the VM walks a `char`
iterator and the glue spreads with `[...s]`, so `"héllo日本"` is seven either
way and `index_of` returns a position a caller can pass back to `slice`.

**A `pub` function is a root, not a survivor.** Pruning used to take the entry
point as the only root when a program had one, and the backend then exported
whatever happened to survive — so a program with *both* an application and a
`main` kept only what `main` reached. An example whose `main` built its own
model never exported `init`; the page found no application and ran `main`
instead, drawing one still frame and looking exactly like a broken animation.
It is the other way round now: every `pub` free function the program itself
declares is exported, so it is a root. A library's names are qualified —
`json.parse`, `prelude.map` — and a dot cannot appear in an identifier, so
"declared here" is decidable and a `hello world` still carries none of the
prelude.

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

A `str` crossing an export is converted by two private module exports. The
public glue functions `str()` and `text()` call those helpers, so JavaScript
sees ordinary strings while Kite keeps its WasmGC scalar array internally.
Conversion uses the same fixed scratch page as declared host calls.

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

## Phase 4 — WebAssembly backend ✅ **complete, language-owned strings included**

`kite-codegen-wasm` emits WebAssembly directly via `wasm-encoder` — no LLVM.

**Done — steps 1 and 2: functions, locals, arithmetic, control flow, and
WasmGC structs.**

```bash
kitec build examples/hello.kite --emit wasm --out dist
# wrote dist/app.wasm (1009 bytes) and dist/app.js
```

**1,009 bytes**, against a 10 KB budget. Shipping no garbage collector is what
makes that reachable, and it is the single reason this design was not viable
before WasmGC reached cross-browser baseline.

It was 346 bytes before `str` became a language-owned GC array, and 1,867 when
every module carried the whole string runtime regardless of what the program
did. A module now contains only the runtime functions it can reach; the
remainder is the conversion bridge, which stays unconditional because the
module exports `str` and `text` for its JavaScript API.

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
4. ~~Language-owned `str` storage~~ ✅ — one WasmGC `array i32`, with one
   Unicode scalar value per element. Every language operation is emitted in
   Wasm, including concatenation, comparison, slicing, searching and indexed
   access. Literals are scalar arrays in the module, and dynamic strings are
   ordinary traced GC values.

   JavaScript is a boundary format, not the representation. A fixed one-page
   memory import moves at most 4,096 scalars per synchronous bridge call; no
   table, cache, high-water arena or converted value survives the call.
   `crates/kite-driver/tests/strings.rs` compares the result with the bytecode
   VM and tests Unicode export and host-boundary round trips.
5. ~~Trait objects with typed function-reference vtables~~ ✅ — a tag and a
   dispatcher per method, because WasmGC compares types structurally and
   `ref.test` cannot separate two structs of the same shape.
6. ~~`extern` declarations driving the glue~~ ✅ — `@host("net") extern fn`
   becomes an import, the generated glue declares the group, and a page
   supplies it. The compiler's own builtins are still a fixed list, which is
   the honest shape of it: those are the language's, not the program's.

String concatenation and comparison operate directly on WasmGC scalar arrays.

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

## Phase 6 — Standard library core ✅ **complete, and derivation with it**

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

- **Returning a fallible call's result directly**, `return parse_object(input, at)`,
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

**Derivation landed, and it is a source-to-source expansion.** `@derive(Debug,
Hash, Encode, Decode)` in front of a `struct` or an `enum` writes the bodies
from the fields. What it produces is **ordinary Kite**, expanded before
resolution: lexed, parsed, checked and lowered like anything a person wrote, so
both backends handle it without knowing derivation exists, `--emit hir` shows
exactly what ran, and a derived method is not privileged over a hand-written
one. The whole expander is one file in the driver and nothing downstream
changed.

That choice costs precision and buys something worth more. It costs precision
because the expander works from what a field's type is *written* as rather than
from what it resolved to — resolution has not happened yet. It buys the absence
of a second implementation of anything: there is no derived-code path through
the checker, so there is nothing for the two to disagree about.

Four things derive, and the two that do not are the more interesting half:

- **`Display` does not derive**, for the reason recorded when it was built: a
  mechanical rendering is wrong more often than right.
- **`Eq` does not derive** either, and this came out differently from the plan.
  `==` is already structural on every Kite value on both backends, so a derived
  `Eq` would be a second spelling for what the language does anyway — and a
  second spelling is a chance for two answers. The roadmap asked for four
  things that wanted one machine; it turned out to be three.

**`json.decode<T>` is spelled `User.decode(doc)`.** Kite has no turbofish, so a
generic `decode` would take its type from the binding's annotation and nowhere
else. An associated function on the type says the same thing where it can be
read, and needs no new machinery: a trait method cannot return the implementing
type without `Self` in return position, and an associated function has no such
trouble.

**One primitive was added for this**, and it is worth naming because the
standard library has resisted adding any: `str.code_at(i)`, the code point at a
character index or -1 past the end. `Hash` needs to see a character as a
number, and so do an ordering and a number parser — three special boundaries,
or one general one. The bytecode VM and the glue both walk code points, so the
two agree on `"héllo日本"` the way they already did for `len`.

---

## Phase 7 — Layout engine and DOM renderer ⬛ **superseded at Phase 16**

> **Superseded.** This phase was completed as described and then removed. The
> DOM renderer it built painted absolutely-positioned elements, which is what
> made a Kite application impossible for anyone else's CSS to style — see
> [the direction change](#the-direction-changed-at-phase-16). `std/ui` is
> deleted. The layout engine's problem still exists wherever a program paints
> its own pixels, and this remains the answer to it.

Done: the flexbox subset in `std/ui.kite`, the Elm-shaped update loop, the DOM
renderer, events — click, key, wheel, pointer move, down, up, and resize, all
through one door — and a **widget layer**: buttons, text fields, checkboxes, a
theme, focus order, and hit-testing that answers with the control rather than
the label inside it.

**The DOM renderer makes real elements**, which is the whole reason it is the
default. A field is an `<input>` or a `<textarea>`, pooled by control id, so the
caret, the selection, IME composition, autofill, the password manager and the
mobile keyboard are the browser's rather than reimplemented. A picture is an
`<img>`, with the browser's decoding and cache. Every control carries an ARIA
role and a label — the label being a *required parameter* of every control
constructor, so a control without one does not compile.

It used to make one thing: a `div`. A field was a run of drawn text with a `|`
glyph standing in for the caret, and every element was `aria-hidden`, which made
the DOM path less reachable than the canvas one. Both are fixed, and the second
was the more embarrassing.

A widget is a function that returns a node, and interaction is a function that
returns a new model. There is no widget object with internal state and nowhere
to hide one, because Kite has no mutable globals — a widget that remembered
something would have to be handed it, which is what the model already is.

`examples/todo.kite` is the task-list application this phase asks for, and it
was driven in a browser with the keyboard alone: typing into the focused field,
`Enter` to add, `Tab` to move, space to toggle. Under the canvas renderer the
same controls reach the parallel semantics tree. **VoiceOver has not been run
against it**, so the accessibility half of the exit criterion is claimed no
further than what `kitec check --a11y` verifies mechanically: every control
labelled, every touch target 48dp, every run of text at 4.5:1 against what is
behind it.

A frame is **recorded before it is painted**, and an identical one is not
painted at all: a pointer moving over nothing costs one comparison rather than
a rebuilt tree.

**The retained scene graph is the recording, kept.** A new frame is compared
with the last rather than replacing it, and what comes out of the comparison is
where the two differ and which rectangles that covers. The DOM renderer now
holds one element per drawing call and **patches** the ones whose call changed,
so a frame differing in one label costs one `textContent` write rather than a
rebuilt subtree — `stage.replaceChildren()` per frame is gone. The canvas
renderer clips to the damage and replays through it, so pixels outside never
move.

The diff is a common prefix and a common suffix rather than a general edit
script, and that is the right limit for this shape of data: a `view` is a walk
over a tree in a fixed order, so a change to a model changes a contiguous run
of calls, and an inserted row shifts a suffix the suffix scan finds unmoved. A
general LCS would find fewer differences on pathological input and cost more on
every frame that is not pathological.

Two things it refuses to do rather than guess at:

- **A changed `clip` or `unclip` repaints everything.** A clip moves everything
  drawn inside it, and the diff does not track which calls those were.
- **Frames of different shapes are not patched in place.** Node *i* and call
  *i* would no longer describe the same thing, and a renderer that guessed at
  how nodes moved would be a second layout engine.

The diff and the damage calculation are pure functions of two recordings, so
they are tested directly, under Node, with no browser: `crates/kite-driver/tests/scene.rs`.
What they *drive* — patching elements, clipping a canvas — needs a real
document and is not tested, which is stated there rather than implied.

**A frame is now an event.** The seven kinds an application could receive were
all input — a click, a key, a wheel, a pointer, a resize — so an application
could only advance when somebody touched it, and a language that targets
WebAssembly for performance could not run an animation at all. `EVENT_FRAME`
carries the milliseconds since the last frame, so a simulation steps by *time*
and runs at the same speed on a machine managing thirty frames a second and one
managing a hundred and forty-four.

Whether the loop runs at all needs no new export and no way to ask, which is
the part worth recording. **It runs while the model keeps changing.** A model
is a value, so `update` returning the model it was given *is* the statement
that nothing is moving; a static application therefore pays one comparison at
startup and nothing after, and an animating one never has to declare itself. A
paused simulation that wants to keep the loop warm returns a model that differs
— a frame counter is enough — which is the explicit way to say "still going".

`examples/boids.kite` is what the frame event was added for: six hundred birds,
every one considering every other, 360,000 distance tests a frame. About 6 ms
to simulate and record one, against a 16.7 ms budget — measured under Node
rather than asserted, and the count is set where there is headroom for a slower
machine rather than where the number would look largest.

**Remaining:** validation against Taffy's fixtures, and a real screen-reader
pass. Layout is over ordinary slices rather than flat buffers, which is the
shape `buffer.F64` exists to change and has not yet.

---

## Phase 8 — Canvas renderer ⬛ **partly superseded at Phase 16**

> **Partly superseded.** Canvas survives; canvas as a *whole-application
> renderer* does not. A `<canvas>` is an element in a page that a program draws
> into — a chart, a game, a visualisation — which is what a canvas is on the
> web. The parallel accessibility tree, the hidden-overlay text input and the
> per-frame damage tracking existed to make a canvas pretend to be a document,
> and a document is what the document is for. They are deleted. The drawing
> calls, the glyph atlas and `std/text`'s line breaking remain, because a
> program painting its own pixels still has to break its own lines.

The renderers are **peers, and the DOM is the default** — Kite targets web
applications, and the DOM is how the web draws. This phase is not the one where
the real renderer arrives; it is the one where the second one does, for the
surfaces a canvas is genuinely better at. What it must never become is the path
an application is pushed onto because the DOM path was allowed to rot.

Done: `canvasRenderer` over Canvas2D, drawing the same eleven calls the DOM
renderer takes and switchable live in the same page against the same module.
Text is measured through `measureText` in the font that will be drawn, so
layout matches what is painted.

**Hidden-overlay text input** is done: typing on the canvas path goes to a real
input positioned where the pointer was, which is the trick every canvas editor
uses and what brings up the on-screen keyboard on a phone.

**A parallel accessibility tree** is done, and now earns the name. `ui.paint`
emits a `draw.semantics` call for every control — role, label, checked state,
focus — and the canvas renderer turns each into a positioned, focusable,
non-interactive element over the canvas, pooled between frames. The DOM
renderer reads the same declarations and puts them on the elements it was
already making, labelling the real `<input>` rather than announcing over it.
One description, two consumers, so they cannot drift.

The announcer — runs of text in order, in hidden DOM — remains beside it for
text that is not a control.

**UAX #14 landed, in Kite.** `std/text.kite` implements rules LB1–LB31 over a
Line_Break class table, so a URL breaks after its solidi, a non-breaking space
does not break, a closing bracket does not start a line, `$1,000.00%` is one
run, and CJK breaks between ideographs but not before a small kana. Two
departures the annex itself names: SA — Thai, Lao, Khmer — is treated as AL,
which §5.1 prescribes without a dictionary, and CB has nothing to break around.

What this replaced mattered more than what it added: the old rule called
anything wider than `W` *under the host's font* its own piece, which made line
breaking depend on the font. Two hosts could legitimately disagree about where
a paragraph broke — the one disagreement the two-renderer design exists to
prevent.

**UAX #9 landed, in Kite.** `std/text.kite` implements the bidirectional
algorithm — P2–P3, X1–X10 with the directional status stack and isolates, W1–W7,
N0 with BD16 bracket pairs, N1–N2, I1–I2, L1–L2 — and it lives beside the
layout engine rather than in a renderer, because a renderer that decided what
order text went in could disagree with the one next to it. The Bidi_Class table
is a **compact range table whose covered blocks are listed in the source**;
anything outside them is treated as `L`, and the comment says which scripts
that answer is wrong for. L3 is absent, L4 is delegated to the renderer, and
HL1–HL6 are not implemented.

**Shaping is the joining half, and says so.** `join_arabic` is Joining_Type over
the Arabic block mapped to Presentation Forms-B, with the mandatory lam-alef
ligature and marks transparent to joining; a combining mark advances nothing.
That is the difference between legible and illegible Arabic. It is **not**
HarfBuzz-quality shaping, which is OpenType GSUB/GPOS — a font's own
substitution and positioning programs — and cannot be written against a
boundary that only measures. Indic reordering, Thai mark placement and Burmese
cluster reordering are stated gaps: a browser-backed renderer gets them from
the host's font stack, and a glyph-at-a-time one does not.

**The glyph atlas** rasterises each tile once, keyed by code point, font and
colour, and blits it thereafter. It serves a run **whole or not at all**: where
the glyph advances do not sum to the run's measured width — kerning, ligatures
— it falls back to `fillText`, which is the correctness gate rather than a
special case. Supplementary-plane characters, emoji, ZWJ sequences, unjoined
Arabic and mixed-direction runs take the same road.

**Per-rectangle damage** is done, and is Phase 7's retained scene graph doing
the work.

**Golden transcripts, not golden images**, and the difference is stated rather
than glossed. Nine committed transcripts — Latin, Cyrillic, Arabic (twice, one
joined), Hebrew, Devanagari, Thai, CJK and Burmese — record the drawing calls a
frame produces, and are compared on the bytecode VM, against WebAssembly under
Node, and through the record-and-replay path both real renderers consume. They
catch reordering, line-breaking, advance and joining regressions. They cannot
catch a rasterisation difference: that needs a browser and pixels, and this
project takes no dependency it does not need.

**Remaining:** real golden *images*, which want a browser; and WebGPU, which
the "where to be willing to cut" list below puts third for a reason —
Canvas2D is a complete product and WebGPU is an optimisation on top of it.

---

## Phase 9 — Native backend ✅ **machine code, and a collector under it**

**Exit criterion met: the differential corpus runs on three backends.** Every
program is compiled to bytecode, to WebAssembly and to machine code, run on all
three, and the outputs compared. That is the criterion this phase was written
around, and it is the thing that makes a third backend safe to have rather than
a third place for bugs to hide.

Two crates, and the split is the usual one. `kite-codegen-clif` lowers MIR to
Cranelift IR — an object file for the linker, or straight into this process
under the JIT, which is what lets `kitec run --native` need no linker at all
and what lets the whole corpus run natively on any machine that can build the
compiler. `kite-rt` is the half that runs at run time: the collector, the host
functions and the scheduler, behind an `extern "C"` surface that is the same
whether the code was JIT-compiled or linked against the `staticlib` build.

**Every value is self-describing.** A heap object is a two-word header naming
what it is, then 8-byte slots. The bytecode VM's values carry their own tags
and the Wasm backend leans on the engine's typed GC records; a native heap has
neither, so the header plus registered *shape tables* — which slots of a
struct, of each enum variant, tuple and closure environment are references —
stand in. One collector traces everything, one routine renders any value the
way the VM does, one routine compares structurally the way the VM does, and
none of it is generated per type.

**The collector is precise, and precision was not optional.** New objects are
bump-allocated in a nursery; a minor collection evacuates the live ones into
the old generation and resets the bump pointer — and moving objects means every
reference to them must be found and updated, which conservative scanning cannot
promise. Roots come from **Cranelift's user stack maps**: every
reference-typed local is declared as needing one, so at each safepoint the live
references sit in recorded stack slots. Collection walks the frame-pointer
chain, visits the recorded slots of frames whose return address is a registered
safepoint, and skips every other frame — the runtime's own Rust frames
included — because nothing in one can hold a Kite reference the maps do not
already cover.

The old generation does not move: each object is its own allocation, swept by
an occasional mark-and-sweep once it has grown past a threshold. The **write
barrier** covers the one in-place heap mutation the language has — a `var`
field assignment. Everything else is copy-on-write and allocates afresh, so the
barrier lives in one function and nowhere else, and that is a property of the
language rather than a convenience.

**The bytecode VM is the specification for everything observable.** Float
formatting, character counting, map ordering, the virtual clock, the trap
messages: where a runtime function reads as a transcription of the VM's, it is
one deliberately, and a difference in any of them is a differential-test
failure rather than a subtle divergence somebody finds later.

`kitec bundle` is unchanged and is still **packaging rather than code
generation** — one executable that is this compiler with the program appended,
which runs on the bytecode VM. Conflating the two would have been the kind of
overstatement this document exists to avoid, and now that there is a real
backend it is worth saying that the bundle is still not it.

**Windows is refused, and the reason is the collector rather than the code
generator.** The root walk assumes the caller's stack pointer at a call is the
frame record's address plus sixteen — true on System V and on AArch64, which is
what makes macOS and Linux work. Cranelift's Win64 prologue establishes the
frame pointer differently, so the stack-map offsets do not land where the walk
expects and the collector traces a stack word that was never a reference. It
surfaces as a corrupted heap under a small nursery, which is exactly the
failure a precise collector must never have, so `--native` and `--emit native`
refuse there with that explanation and the corpus compares two backends on
Windows and three everywhere else. Finishing it needs a Windows machine to
read the prologue on; guessing at the offset from a distance is how a
collector acquires a second bug.

It was found the way it should have been. macOS forces frame pointers by ABI,
so the first version of the walk passed on the machine it was written on and
corrupted the heap on the other two — which is the argument for running CI on
three operating systems, and for the differential corpus being the thing that
noticed.

**What the collector does not do**, said plainly because a collector's gaps are
where the surprises live:

- **No aging.** Everything alive at a minor collection is promoted on its
  first one. A generational collector usually keeps a survivor space and
  promotes on the second or third; this promotes early, which costs old-space
  occupancy in exchange for a much simpler nursery.
- **The old generation is never compacted.** It is swept, not moved, so it can
  fragment under a workload that allocates long-lived objects of many sizes.
- **Native recursion is bounded by the machine stack**, not by a counter. The
  bytecode VM traps at 2048 frames with a message; native code runs until the
  operating system stops it.

Two more things are true and worth writing down. Integer overflow **traps**
natively, as the specification says and as the VM does — the Wasm backend wraps,
and that difference is the backend's, not the language's. And the runtime is
**single-threaded by design**, like the VM and the Wasm host: real parallelism
is not this phase's to give, and the `Share` marker is still what will make it
free when the platform allows it.

---

## Phase 10 — Tooling ✅ **including the package manager**

| Tool | State |
|---|---|
| `kitec fmt` | ✅ token-based, comment-preserving, and the whole tree passes `--check` |
| `kite-lsp` | ✅ diagnostics, hover, definition, references, rename, completion, symbols, inlay hints, formatting |
| `kitec test` | ✅ every `test_` function, with failures as values so one does not stop the rest |
| `kitec fix` | ✅ applies the machine-applicable suggestions diagnostics carry |
| `kitec doc` | ✅ the reference, from `///` comments, with signatures read from the parse |
| `kitec bundle` | ✅ one executable that needs nothing installed |
| `--explain` | ✅ |
| `kitec pkg` | ✅ manifest, path and git dependencies over `https`/`ssh`, semver resolution across the graph, and a lockfile of content hashes that is verified rather than merely written — a dependency whose bytes moved fails the command unless `--update` says otherwise |

The formatter works on **tokens**, not the tree: a tree has dropped the
comments and the blank lines a formatter must keep. It decides indentation,
spacing and blank lines, and leaves where the lines end to the author — the
same bargain `gofmt` makes. Running it over the tree found six bugs in it,
which is the argument for running a formatter on real code before believing it.

`kitec pkg` reads `kite.toml`, resolves each dependency — a path, or a git URL
and tag cloned into `.kite/vendor` — and writes `kite.lock` with a content hash
over every `.kite` file of each. A build never reaches the network: `pkg` is
the only thing that fetches, and only when asked. There is no post-install
script and no build-time code execution because there is nowhere to put one,
which is the npm supply-chain surface removed by construction rather than by
policy.

**Version resolution landed, and the package manager is written.** A dependency
may now say `version = "^1.2"` instead of pinning a tag, and `kitec pkg` walks
the whole graph — a dependency's own `kite.toml` names its dependencies — and
picks **one** version per name that satisfies every requirement on it. One
version per name, not one per requirer: a name means one thing, which is the
same stance the manifest took about hoisting.

The solver is chronological backtracking, candidates highest-first, a
candidate's own requirements withdrawn on unwind. PubGrub-style conflict-driven
learning would find failures faster on a graph far larger than any Kite package
has; this is a page of dependency-free code whose behaviour can be explained in
one sentence, which at this size is worth more.

Where nothing satisfies everyone, the diagnostic names **who wants what** —
because "resolution failed" is the message that makes a package manager
hateful:

```
error: no version of `shared` satisfies everyone who names it
    a 1.2.0  requires >=2.0.0
    b 0.3.0  requires >=1.0.0, <2.0.0
  the versions that exist: 1.4.0

note: a name means one thing — every package that names `shared` gets the same copy,
so the requirements on it must agree
```

Three rules came out of writing it:

- **Compatible means the same major, or the same minor while major is 0**, and
  it is written down rather than inherited by assumption. There is no third
  guess for `0.0.x`.
- **A pre-release is chosen only by a requirement that names it.** A `>=1.0.0`
  does not silently pick up `2.0.0-rc.1`, and when one was in numeric range the
  error says so rather than leaving a reader to wonder.
- **A tag and a version together is an error** that says to pick one: a tag
  pins and a version resolves, and a dependency doing both is a dependency
  saying two things.

Discovery for an unpinned git dependency is `git ls-remote --tags`, candidates
are cloned into `.kite/vendor/<name>@<version>`, and the chosen one is
materialised where a build reads it. `--offline` resolves only from what is
already vendored and says so when it cannot.

**A security review of this found two real holes, and both were the same
mistake: a string a dependency's dependency wrote, trusted.** The module header
above claims the npm supply-chain surface is removed *by construction*, and
that claim is exactly what fetching a transitive graph put back.

- **A dependency's name became a path.** It is a TOML key, it was stored
  unvalidated, and `.kite/vendor/<name>` is deleted recursively and written
  into — so a name of `../../.git/hooks` replaced the project's git hooks with
  executable files, and an absolute name went anywhere at all. Names arrive
  transitively, so it need never have appeared in a manifest anybody here
  wrote. A name is now ASCII letters, digits, `-` and `_`, checked once in the
  parser where names enter, and checked again at the join because that is
  where the damage would be.
- **A URL beginning with `-` was an option, not a URL.** `git ls-remote --tags
  --upload-pack=…`, left with no repository, falls back to the current
  directory's own remote and honours it. There is now a scheme allow-list, a
  `--` before every positional argument, and `GIT_PROTOCOL_FROM_USER=0` with
  every protocol denied but the four that are asked for.

One claim in that review did **not** survive checking, and it is worth
recording because the correction is the useful part: `ext::`, which hands the
rest of a URL to a shell, was reported as exploitable on the theory that its
default policy is `user` and an unset `GIT_PROTOCOL_FROM_USER` permits it. Git
defaults *known-dangerous* protocols to `never`, so it is denied
unconditionally and has been since 2.12. The allow-list stays anyway: the URL
is attacker-controlled through a manifest nobody here wrote, and defending
that on the far end's default configuration is not defending it.

---

## Phase 11 — Networking 🟡 **both halves work; WASI does not**

`std/http` is five host declarations and the rest Kite. Three requests started
together take as long as the slowest, which the tests demonstrate under Node
against `data:` URLs. A 404 is a `Response`, not an error — the request
succeeded and the answer was "no" — and only a transport failure is an `error`.

Waiting is `task.wait_host()`, which is its own thing: an awaiting task parks
for another task, and a fetching one must let the event loop run. Saying which
is what stops the scheduler spinning through a promise that cannot resolve
while it holds the thread.

The **server half was the part that needs no sockets**: `Request`, `Response`, a
router with `:name` captures, and handlers that are functions and can be tested
by calling them. That was true before anything could listen, and it is still
true — which is the point of having built it in that order.

**Now something listens.** `std/http` declares eleven more host functions —
open, poll, take, read, answer, close — in exactly the shape every other
boundary in the module takes: numbers and text cross it, a handle stands for
what the host holds, and **nothing is pushed at the program**. A request is
*taken*, when a task asks for one. A callback-driven server would be a second
way for control to enter a program, and the language has one.

```kite
let (server, err) = http.open(0)
check err
let (answered, rerr) = await http.run(server, routes)
```

Waiting is `task.wait_host()`, for the same reason a fetch waits that way: what
is being waited for is the host rather than another task, and saying which is
what lets the runtime hand the event loop back instead of spinning through a
queue that cannot fill while it holds the thread.

**The adapter is a separate generated file**, not another group in the glue.
`kitec build --emit wasm` writes `serve.mjs` beside the module **only when a
program declares the listening half** — a page cannot open a socket, and a
program that draws a rectangle should not have `node:http` in its import graph.
It reaches the module through the glue's own `provide("net", …)`, which already
existed for exactly this, so the boundary needed no change at all.

The socket is held open until the program answers, because a server that
replied before its handler ran would be a different program. Answering twice
returns 0 rather than doing it quietly.

Two things came out differently from the plan:

- **`close` became `shut`.** `close` already meant "stop listening to this
  event stream" in the same module, and Kite has no overloading: one name, one
  signature. Two things that are not the same thing do not get the same name.
- **The request handle is beside the `Request`, not inside it.** `Incoming`
  carries both. Putting the handle in the `Request` would have made a
  `Request` something only the runtime could produce — and a handler is only
  testable by calling it if a request is something anyone can write out.

**A security review found two things in the fetching half**, both of which had
put back the surface this design claims to remove by construction, and both are
recorded in Phase 10 where the package manager is. The server half contributed
one design change: **response headers cross as pairs, not as text.** Everything
else on this boundary is `name: value` lines, and for a response that encoding
is unsafe — the host splits on the newline, so a newline inside a value stops
being data and becomes a separator, and a handler putting a path or an
identifier from a request into a header would be one newline away from letting
the caller add a `Set-Cookie` of their own. Joining the lines inside
`http.respond`, from parts it was handed separately, is what makes "the program
meant two headers" and "somebody else added one" distinguishable at all; from
the joined string it is not. A value carrying a line break is refused rather
than stripped, because there is no escaping that keeps the meaning.

**Remaining: WASI.** `wasi:http/incoming-handler` is the other implementation
this boundary was shaped for, and it is not written — it is a component-model
export and `kitec` emits a core module. That is a real gap, recorded rather
than stubbed: a stub that looked like an answer would be worse than none.

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

## Phase 12 — Cryptography ✅ **complete, less Argon2, which no host has**

`std/crypto` binds SHA-256/384/512, HMAC-SHA-256, PBKDF2 password hashing with
a generated salt, cryptographically secure random bytes, and a constant-time
comparison. Comparing a secret with `==` is a warning (E0600) that says to use
`crypto.equal`.

**AES-GCM, Ed25519 and X25519 landed, and the key handling they were waiting on
got designed.** A key is a **host-side handle**, not text: `Key`, `SigningKey`
and `AgreementKey` are structs with one unmarked field, so a program cannot
construct one or read what is inside it. Three things follow, and they are why
it is shaped this way rather than as hex:

- **Private material never crosses the boundary**, so it cannot leak — the keys
  are created non-extractable, and even the glue could not export one.
- **A key cannot reach a log.** `Display` is never derived, so `io.print(key)`
  does not compile.
- **Comparing two keys compares handles**, so there is no secret for a
  comparison to leak — which closes a hole E0600's shallow analysis cannot see.

The API is what the design commitments demanded. `seal` generates its own
96-bit nonce and carries it with the ciphertext, because an API that accepts a
nonce invites the reuse that is the commonest failure in applied cryptography.
Authentication failure is an `error`, never a wrong answer, and the message is
deliberately unspecific — an oracle that said *what* failed would be worth
attacking. `agree` returns a sealing key rather than the raw X25519 output: the
agreement, HKDF-SHA-256 and the AES key are one chain inside the host, so the
structured raw secret never exists anywhere a program could reach it.

Durable symmetric keys come through `import_key(crypto.random(32))` — hex the
caller already holds — so there is no export path to invent. Public halves
export as hex, because those are the halves meant to be given away.

**Remaining:** Argon2, which is not in WebCrypto at all and waits on a runtime
that has it.

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

- **`kitec doc`** — extracts doc comments and produces the reference. The
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

## Phase 14 — Editor support ✅ **complete, rename and hints included**

`kite-lsp` answers over stdio: diagnostics as you type, hover, go to
definition, completion, document symbols and formatting. Every answer comes
from the same passes `kitec` runs — a language server that re-derives its own
is a second compiler, and the disagreement always surfaces as "the editor says
this is fine and the build says it is not".

The protocol is `Content-Length`-framed JSON, and both halves are written by
hand: the framing is six lines and the JSON two hundred, which keeps the
dependency list at nothing a build has to fetch. The VS Code extension has no
npm dependency either, not even the LSP client library.

**Rename, references and inlay hints landed**, all three over the resolver's own
binding table rather than over a second analysis. The hints are the ones worth
most here: the solved type of an unannotated `let`, and the **solved generic
arguments** at a call site — Kite has no turbofish, so there is nowhere else to
see what a call inferred.

Rename's rule is the interesting part, and it is a refusal rather than a
best-effort. It starts only when *every* occurrence of a name is recorded and
every recorded occurrence is editable, and it says why when it will not: a name
from the prelude or another module file, a method (whose call sites need the
receiver's type and were never in the table), a type, a host function whose
name is the contract, or a binding with a "mention" — a qualified path, or a
`Point{ x }` shorthand whose one identifier is also a field. The new name must
lex as a single identifier, must not be a keyword, and must not already be
bound where the old one is visible.

That is a smaller rename than an editor with a full index would offer. It is
also one that cannot quietly miss an occurrence, which is the failure that
makes rename untrustworthy.

**`kite-lsp`, then a thin VS Code extension over it.** The order matters: an
extension that implements its own analysis is one that only ever works in one
editor, and the same queries the compiler runs are the ones an editor needs.

- Diagnostics as you type, from the same `DiagBag` the CLI renders
- Go to definition, find references, hover types, completion
- Rename, using the resolver's binding table
- Format on save, once `kitec fmt` exists
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

**Properly, later:** the submission is written and waiting in
[`packaging/linguist/`](../packaging/linguist/) — the `languages.yml` entry, the
heuristics, and a script that copies the samples out of the library at
submission time rather than keeping a second copy to rot. The grammar written
for VS Code is the one it vendors, and the repository now carries an MIT
`LICENSE`, which is on Linguist's permitted list and was the one piece actually
missing.

**The gate is a number, and it is further off than "hundreds of
repositories" suggested.** Linguist asks for *at least 2000 files per
extension indexed in the last year, excluding forks*. GitHub currently indexes
about 480 `.kite` files, and most are not this language: `kite-lang/kite` and
`kitecorp/kite-language` are other projects with the same extension and the
same name, and Linguist has no entry for any of them.

That collision is worth knowing about in advance rather than discovering in a
review. Whoever submits first decides how the rest are told apart, so the
heuristic offered keys on things this language has and the others do not —
`check`, `-> (T, error)`, `@host(`, `@derive(` — and answers nothing when it
sees none of them. A heuristic that guesses is worse than none: it is
confidently wrong about somebody else's language.

None of that is a task. It is a consequence of adoption, and the right thing
to do with it is leave the submission ready and go and build something worth
using.

---

## Phase 15 — Distribution 🟡 **signed, packaged, and not yet published**

`.github/workflows/release.yml` cross-compiles `kitec` and `kite-lsp` for macOS
(arm64, x86-64), Linux (x86-64 and arm64, static musl) and Windows on a tag,
with a checksum file. The builds are reproducible — the path prefix is remapped
and debug info dropped — because a compiler is exactly the artefact where
"trusting trust" is not hypothetical. `install.sh` downloads one archive,
verifies it against the release's own checksums, and refuses rather than warns
when they differ.

**Signing landed, and it holds no key.** `SHA256SUMS` — the one file that names
every artefact — is signed with Sigstore's keyless flow: the workflow's own
OIDC token is the identity, the certificate goes in the public transparency
log, and there is no private key anywhere. For a compiler that is the right
trade. A signing key held by one person is a key that can be lost, stolen, or
unavailable when it is needed, and this is exactly the artefact where "trusting
trust" is not hypothetical.

`install.sh` verifies that signature **when `cosign` is already installed**, and
says so when it is not. It deliberately does not install `cosign` to check a
signature: a verifier fetched by the thing it is verifying proves nothing.

**The three package manifests are written**, in `packaging/`: a Homebrew
formula, a Scoop manifest with `checkver`/`autoupdate`, and an AUR `PKGBUILD`
for the binary package. Each is committed with a **placeholder checksum**, and
`packaging/render.sh` fills them in from a release's own signed `SHA256SUMS` —
so the manifests a tap needs come *out of* the release rather than being
written beside it and hoped to match. The release runs it and attaches the
results. A test asserts the five target triples in the release matrix are the
same five the installer and the three manifests know about, because a manifest
naming an archive no release produces fails in somebody else's install, long
after the change that broke it.

**`kitec` as a WebAssembly module is now a release artefact**, built and
checksummed alongside the binaries — the playground already ran it, and this
means anything that wants to compile Kite in a browser can have it without
building the site.

**Remaining, and it is the honest part: nothing has been published.** No tag has
been pushed, so none of this has run in anger, and the three manifests have no
homes — Homebrew and Scoop want a separate repository each, the AUR wants an
account. Those are decisions about identity and hosting rather than code, and
they are recorded as undone rather than implied to be done.

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

## Phase 16 — Demolition

**Goal:** the repository stops containing the old answer.

Deleting is a phase because it has an exit criterion and because doing it
half-way is worse than not starting: a deprecated `std/ui` left beside a new
`std/dom` is two ways to build a screen, and the whole argument for the change
was that there should be one.

Out: `std/ui.kite`, `packages/material`, the examples written against them, the
tests that assert their behaviour, and — in `crates/kite-codegen-wasm/src/glue.rs`
— the canvas renderer, the glyph atlas, the semantics layer, the frame
recording and replay, the frame diff and the damage tracking.

Staying: the drawing builtins, `std/canvas`, and `std/text`. A program painting
its own pixels still needs all three.

**Exit criterion:** the build is green, the test suite passes, and nothing in
the repository renders a user interface. That last clause is the uncomfortable
one and it is the point — the next four phases are built on an empty floor
rather than around furniture nobody wants.

**Done.** 19,700 lines out across 60 files; 738 tests pass. Three things came
out differently from the plan, recorded because each one is a thing the next
person doing a demolition would get wrong the same way:

- **Two pieces of the transcript machinery are not UI, and nearly went.**
  `textRenderer` writes the drawing calls as text and `setWriter` redirects
  where they go — both look like part of the renderer set, and both are how the
  differential suite captures a program's output and compares three backends.
  The lesson is general: in a codebase where the test oracle is *also* a
  renderer, the deletion boundary does not follow the module boundary.
- **The golden transcripts went with the layout engine.** Eight scripts —
  Latin, Cyrillic, Arabic, Hebrew, Devanagari, Thai, CJK, Burmese — were
  compared through `examples/scripts.kite`, which laid text out with `std/ui`
  before painting it. Keeping the test would have meant resurrecting a layout
  engine to feed it. The algorithms it exercised keep their direct unit tests
  in `tests/std/text_test.kite` and `tests/std/linebreak_test.kite`, which is
  the half worth having; the integration path is genuinely gone.
- **Nothing wraps text any more.** `ui.wrap` was the only line breaker in the
  standard library, and `std/canvas` draws text without one. `std/text` has the
  UAX #14 opportunities and `canvas.width_of` has the measurement, so this is a
  small function that has not been written rather than a missing capability —
  but it is a gap, and it belongs to canvas rather than to the document.

---

## Phase 17 — `JsValue`, the reference §15 promised

**Goal:** a Kite program can hold a browser object without a table of integers
underneath it.

Today it cannot. `std/dom` holds an `int` indexing `const NODES = [null]` on the
JavaScript side, and the spec describes a `HostRef` that was never built. Three
things are wrong with the integer, and none of them is fixable later:

- **It is `Share`.** An `int` crosses into a worker where the table does not
  exist, which is exactly what `Share` exists to prevent.
- **The table cannot shrink.** Nothing can tell JavaScript that Kite dropped a
  number, and WasmGC has no finalizers. The alternatives are a `release()` call
  in application code — manual memory management in a collected language — or
  unbounded growth.
- **Identity is broken.** `handle()` pushes unconditionally, so finding the same
  element twice yields two different numbers. Every event handler ever written
  asks "is this my element?", and the integer answers wrongly.

`JsValue` is a compiler-known opaque type lowered to `externref`. Not `Share`,
not comparable with `==` — `externref` is outside the `eq` hierarchy, so
identity is a primitive (`js.same`) and `==` on one is a compile error rather
than a quiet wrong answer. Unimplemented off the web, where it names a diagnostic
instead of a value.

This also settles lifetime, and settles it completely: on the web the Wasm heap
*is* the JavaScript heap. A Kite struct holding an element, whose listener holds
a Kite closure, is a cycle across the boundary that one collector collects. No
protocol, no ownership rules, no release calls.

The plumbing exists already: `JsValue` is an `externref`, and `task_spawn`
passes a Kite closure to the host as a reference and receives it back through
`kite_poll`.

**Exit criterion:** a Kite program holds an element across an await point, drops
it, and the browser collects it.

**Done, with the second half claimed no further than it was tested.** A host
object crosses into Kite, is stored in a struct field, comes back out, survives
an `await`, and answers identity correctly — all asserted under Node in
`crates/kite-driver/tests/host.rs`. **Collection is not asserted**, because
observing it needs a heap snapshot and there is no dependency here that takes
one. The argument for it is structural rather than measured: the field is an
`externref` in a GC struct, so the engine traces it like any other reference,
and there is no side table left for a leak to accumulate in.

Three things the work turned up:

- **`extern` had to admit the type.** Host signatures were restricted to
  numbers, booleans and strings, with a note saying structure crosses "as a
  handle the host made" — which was the right answer and had no way to be
  spelled. `JsValue` is that handle, given a type.
- **`Emit::Check` must stay permissive.** It is what `kitec check`, `kitec run`
  and the language server all use, so refusing there would make a web program
  stop type-checking in an editor. The refusal belongs to `--emit native` and
  `--emit kbc`, which are asked for an artefact that cannot hold a reference.
- **Wasm has two null references, and `asyncify` found it.** A host object held
  across an `await` becomes a field of the generated state machine, initialised
  with the backend's "value no program can observe" — which emitted `nullref`,
  from the internal hierarchy, into a slot typed `externref`. The module failed
  validation rather than misbehaving, which is the good way to get this wrong.

Two diagnostics were rewritten rather than left to be technically correct and
useless. `==` on a host object said "`JsValue` is not ordered", which is true
and not the reason; it now says there is no structure to compare and points at
`js.same`. `Share` said "two tasks holding one mutable value is a data race",
which is the wrong advice entirely — the field *is* immutable, and no change to
the type makes a reference meaningful in another isolate.

---

## Phase 18 — `std/js`, the primitives

**Goal:** the last JavaScript anybody writes.

The choice this phase settles is how a Kite program reaches a browser API. One
`extern` per feature is direct and fast, and it fails on contact with reality:
the browser has thousands of them, so the first API the standard library never
covered forces a user to hand-write a JavaScript host object and register it.
**A language whose extension mechanism is "go and write the other language" has
conceded its own argument.**

So instead: about fifteen primitives that never grow.

```
js.global(name)              js.get(v, name)          js.set(v, name, x)
js.call0(v, name) … call4    js.new(name, args)       js.func(f)
js.await(p)                  js.same(a, b)            js.is_nil(v)
js.instance_of(v, name)      of_str/of_num/of_bool    as_str/as_num/as_bool
```

Everything else — `std/dom`, and any browser API a user needs — is then ordinary
Kite code written over these. Adding an API is writing Kite, not touching the
compiler, and the generated JavaScript stays a fixed size forever.

**Every primitive catches.** A JavaScript exception must never cross the
boundary raw: today one throws straight through the Wasm frames and takes the
scheduler with it, so a single mistyped method name does not fail a call, it
kills every running task in the program. `js.call` returns `(JsValue, error)`
and the taint analysis makes the check mandatory — which turns the one real
weakness of this design, that names are strings, from fatal into survivable.

Numbers cross as `f64`, not BigInt. JavaScript numbers *are* `f64`; an `int` is
an i64 and every crossing would otherwise allocate. The safe-integer check
happens on the Kite side, where an error is a value.

`std/js` is its own module so that importing it is visible. It is the floor
below the typed world, and a file that reaches for it should say so in its
first three lines — the marking Rust gets from `unsafe`, for the same reason.

**Exit criterion:** `std/dom`'s entire current surface is reimplemented over the
primitives, in Kite, with no `extern` declarations left in it.

**The layer is built; `std/dom` moves to Phase 20.** `std/js` is twenty-three
primitives — the "roughly fifteen" above was optimistic, and the extra eight are
arity variants of `call` and `new` rather than new ideas. Its host block is
about ninety lines of JavaScript and does not grow with anything a program
reaches.

Two of the primitives listed above are **not** here, and both were misplaced
rather than dropped. `js.func` hands a Kite closure to the host, which is not
glue but compiler machinery — a closure crossing as a reference through a
trampoline, generalising what `task_spawn` already does — and it lands in Phase
20, where events need it. `js.await` needs a promise and a scheduler that can
be woken, so it lands in Phase 21 with the rest of the effects.

Three things the work turned up:

- **A host function cannot return `(T, error)`.** A fallible pair is a Kite
  aggregate, and aggregates do not cross the boundary. So a call that throws
  returns a **marker object** carrying the message, and the Kite side turns it
  into an ordinary error. The marker is made fresh per throw rather than kept in
  a slot, which matters for a specific reason: a host call can fire a Kite
  handler synchronously in the middle of itself, and an `errno` would let that
  handler's failure overwrite the one being reported.
- **Concrete error types are not built yet**, so the failures are `errors.new`
  strings rather than an enum implementing `Error` — `std/errors` already
  records that gap. Call sites are unaffected either way, because what a caller
  holds is an `error`.
- **The int/float conversion belongs on the host's side of the boundary.**
  Doing it in Kite means a lossy cast the compiler warns about, and the loss it
  warns about is exactly what `of_int` exists to refuse. `Number.isSafeInteger`
  is asked where the number lives.

---

## Phase 19 — A runtime that can live in a page

**Goal:** a Kite program that is idle, and then is not.

Everything before this assumed a program that starts, does its work and ends.
An island in a page is idle almost always, which the current runtime cannot
express. Three separate faults, all of them fatal to it:

- **`drive()` returns when the last task finishes.** After that a click has
  nothing left to run it.
- **It busy-waits.** While anything waits on the host it spins on
  `setTimeout(0)`, burning a core for as long as the page is open.
- **The clock is virtual.** When every task is waiting it jumps to the earliest
  deadline, so `task.sleep(500)` returns immediately and `time.now()` is not
  wall time.

The virtual clock is not a bug and must not be deleted. It is what makes three
backends differentially testable — a scheduler racing real timers could not be
compared against anything — and it stays as the mode the test suite runs in. The
browser needs the other mode: `performance.now`, real `setTimeout`, a pump woken
by callbacks and asleep otherwise. **A `sleep` that lies about time is hidden
control flow by this language's own definition**, and that is the argument for
splitting the two rather than picking one.

The pump also needs a reentrancy guard. A `js.call(el, "focus")` can fire a Kite
handler synchronously in the middle of a Kite call, and the current loop mutates
its task list by index while iterating it.

**Exit criterion:** a page open for an hour with a Kite island on it uses no CPU
until something happens, and `task.sleep(500)` takes half a second.

**Done, as a second driver rather than a change to the first.** `drive` is
untouched and still runs a program to completion on the virtual clock, because
that is what makes three backends comparable and every existing test depends on
it. `resident` sits beside it: one pass over the task list, then a timer set to
the earliest deadline — or no timer at all when every task is waiting on the
host, since a callback will `wake` it and until then the program costs nothing.

The two share `step`'s polling order deliberately. If they did not, a program
would interleave differently depending on which driver ran it, and the test mode
would stop predicting the real one — which is the property the virtual clock
exists to provide in the first place.

Three assertions, one per fault: a task runs after `main` returned; a 150 ms
sleep takes at least 150 ms of wall time while a 5-second sleep in the batch
driver costs none; and an idle program schedules **zero** timers over 300 ms,
counted by wrapping `setTimeout`. That last one is the difference between an
island you would put on a page and one you would not.

The reentrancy guard is in and not yet exercised, because nothing can re-enter
until Phase 20 gives the host a way to call a Kite closure. A wake-up during a
pump sets a flag and the pump goes round again rather than recursing, which is
what keeps a handler that spawns a task from mutating the list underneath the
loop walking it.

---

## Phase 20 — `std/dom`

**Goal:** the typed layer, and the first page that works.

Ordinary Kite over `std/js`. An opaque `Element` wrapping a `JsValue`, so the
dynamic layer cannot spread into application code by accident — and
`dom.raw(e)` / `dom.wrap(v)` as the one blessed door, because the alternative is
worse than it looks. Seal it completely and the user who needs a single method
the library never wrapped cannot reach their own element; what they do next is
rebuild a whole parallel untyped world beside yours. One greppable escape is a
hygiene boundary that holds. A wall is a hygiene boundary that gets climbed.

Absence is `Option<Element>`. The current design returns handle 0 and documents that
every call tolerates it — a null object, in a language whose specification calls
the tolerated zero value Go's most common production bug and taints every
unchecked result. Silent no-op on absence is worse than a null dereference,
because it does not even fail where the mistake is.

Events are closures crossing as references, the `task_spawn` pattern
generalised: `js.func(f)` hands JavaScript something it can call, and the
closure's lifetime is the listener's lifetime, traced by the one collector. A
registry of numbered handlers would pin every listener forever. Registration
returns a `Subscription` with an explicit `cancel`, because nothing else can
cancel it for you, and explicit teardown is squarely inside "verbosity is
acceptable".

The rule that a closure may not capture a `var` is not an obstacle here and must
not be weakened for this. Capturing a `let` struct handle and passing it to a
function taking `var` is exactly how a handler should change state, and this
phase is where that stops being a workaround and becomes the documented idiom.

Two admissions to write down rather than discover:

- **`innerHTML` is reachable again.** `std/dom` today refuses `set_html` and
  says it never will, on the grounds that building markup from strings is the
  commonest way to grow an injection bug. With an escape hatch, anyone can set
  it. The comment has to be rewritten to what stays true: the typed layer does
  not hand you the loaded gun, and the escape hatch is not the typed layer.
- **String conversion is an explicit boundary cost.** DOM calls receive plain
  JavaScript strings, but Kite stores scalar arrays. The generated bridge pays
  one linear conversion through fixed scratch storage and retains nothing
  afterwards; resident programs do not accumulate host-side string state.

**Exit criterion:** a plain HTML page with a real stylesheet, a `<script
type="module">`, and Kite making a form work. No canvas, no framework, nothing
generated. That page is the whole thesis in one file.

**Done.** `examples/page` is that file — `index.html`, `style.css`, `main.kite`
— and the module is 5,697 bytes. The page is complete before the module
arrives; the count turns red below zero because the stylesheet says so and the
program's entire contribution is `set_class(count, "negative", …)`; hover and
focus never reach Kite at all.

`std/dom` is ordinary Kite with **no `extern` declarations in it**, which is
what makes the primitives underneath a real answer rather than a promise.

Four things the work turned up, three of them the language refusing to let
something sloppy through:

- **`kitec build` was overwriting the page.** It wrote `index.html` on every
  build, which for this target destroys the actual work — and does it most
  often to whoever is furthest along. An existing page is now left alone and
  the build says so.
- **Narrowing does not reach inside a closure**, and should not: the closure
  runs later, and what was known at the point it was made is not known when it
  fires. Handlers capture a narrowed element rather than an optional that
  happened to be present.
- **One guard each, not one guard with `||` in it.** Testing three optionals in
  a single condition narrows none of them, because passing says only that
  *some* were present.
- **`check` needs a fallible return.** A function answering with a bare `error`
  writes the test out. Worth keeping in mind for the shape of the rest of the
  module.

And one bug in the compiler, recorded because the comment predicting it was
already in the file: a builtin missing from the backend's returns-a-value list
has its result silently dropped. `js.func` was missing, so every handler arrived
as `null` and nothing failed anywhere.

---

## Phase 21 — A rejection that cannot be dropped

**Goal:** the one thing the platform's own arrangement gets wrong for this
language.

**This phase was wrong when it was written, and it is corrected rather than
quietly edited.** It said "every modern browser API returns a promise" and
planned `js.await` as though promises were a capability Kite lacked. They are
not. A promise is an object, `then` is a method on it, and `js.func` supplies
the callback — so `fetch` and everything shaped like it work with the
primitives from Phase 18 and no compiler support whatever. There is a test
asserting exactly that, named for the claim it disproves.

The mistake is worth recording because of its shape: it came from assuming that
because JavaScript needed a language feature for this, Kite must too. What
JavaScript needed the feature *for* was ergonomics — escaping nested callbacks —
and ergonomics is not a capability gap.

What is genuinely wrong is smaller and worse. **`then` with one callback throws
a rejection away.** Nothing runs, nothing is reported, and the failure is gone —
in a language whose central claim is that an error cannot be ignored. So
`js.settle(promise, done, failed)` takes both halves and requires both. Ignoring
a rejection is then something somebody wrote down, rather than something nobody
noticed.

The failure arrives as a `str` rather than an `error`. An `error` in Kite is
either nil or a failure, so a handler taking one would nil-test it at every call
site — for a callback that only runs when something has gone wrong. A parameter
that can never be nil should not be typed as though it can.

**Done.** Twelve lines of Kite over the existing primitives. No compiler work,
no new host functions, nothing added to the fixed glue block.

### Still open, and clearly marked as comfort

Reading a response body needs a callback inside a callback, and three levels is
ordinary. That is the thing JavaScript spent a decade escaping, and Kite already
has `async`/`await` that does not meet the browser's version.

It is deliberately **not** built yet. It is a convenience rather than a
capability, and the honest way to decide whether the nesting hurts is to write
enough real code to find out. If it does, the shape is a primitive that parks a
task and resumes it on settle — `task_wait_host` already marks a task as
waiting, and `wake` already brings the pump back, so the machinery is in place
and only the joining is missing.

Elm needs a command algebra because it has no way to *do* anything; effects must
be described as data for a runtime to perform. Kite does not, and that
observation stands whether or not the `await` form is ever built.

---

## Phase 22 — Interop backwards

**Goal:** somebody adopts Kite without rewriting anything.

This is the phase that decides whether the language gets used, and it has
nothing to do with UI.

**TypeScript won by being adoptable one file at a time.** Not by being better in
the abstract — by letting a team rename one file and leave four hundred alone.
Kite can have exactly that shape: `@export` makes a function a real ES module
export, and the compiler emits a `.d.ts` beside it so a TypeScript project can
call it and still type-check. The type information already exists; writing the
declaration file is cheap, and it is what makes "an alternative to TypeScript" a
claim rather than a slogan.

A team replaces one module — a parser, a validator, a decimal-money layer, a
hot loop — sees it get faster and stop having a class of bug, and comes back for
the next one. That is a far easier thing to ask than "adopt a language with no
ecosystem".

`kitec` therefore emits **a module a bundler can import**, not a generated HTML
page. And the highest-value tooling here is a Vite plugin, not a Kite bundler:
meet the ecosystem where it is.

**Exit criterion:** an existing TypeScript project imports a Kite function,
type-checks against it, and builds with no configuration beyond the plugin.

**Done, and `@export` turned out to be unnecessary.** A `pub fn` has been a real
Wasm export the whole time — JavaScript could always call one. Two things
stopped anyone doing it, and neither was a missing feature:

- **The calling convention was undocumented.** An `int` arrives as a `BigInt`, a
  `str` is an index into a table you must intern into and read back out of, and
  a `bool` is an i32. A calling convention nobody wrote down is a reason not to
  adopt something.
- **There were no types.** A TypeScript project had nothing to check against.

So `kitec build` now writes `api.js` and `api.d.ts` beside the module: one
wrapper function per export with the conversions applied, and a declaration for
each. Parameters keep their own names, because a signature in an editor is one
of the few places a wrapper is actually *seen*. `int` is `bigint`, not `number`
— a `number` holds it to 2^53 and loses the rest in silence, which is the sort
of edge that reaches production.

Real `tsc` is run in the test suite, where it is installed: correct use
type-checks, and `add(2, 3)` is rejected because the parameter is a `bigint`.

A signature JavaScript has no representation for — anything taking or returning
a struct, enum, slice or map — is **left out of both files and named in a
comment saying why**. Describing it wrongly would type-check code that cannot
work, and dropping it silently would send someone looking through a changelog.

One bug, found by a test rather than by reasoning: a `bool` crosses as an i32,
so a caller passing `true` was sending `undefined` and a returned `false`
arrived as `0` — which the declaration file called a `boolean`. A wrapper whose
types are a lie is worse than no wrapper.

**Still open:** the Vite plugin. Nothing depends on it — the generated files are
ordinary ES modules a bundler already understands — so it is convenience rather
than capability, and it is marked as such.

---

## Phase 23 — The size gate

**Goal:** a number, in CI, that fails the build when it grows.

Under the old design a large module was an annoyance. Under this one it is
fatal: an island that makes a form work competes with a four-kilobyte
JavaScript file, and nobody ships three hundred kilobytes for that.

WasmGC is the reason this is winnable. A linear-memory module ships its own
allocator and a chunk of runtime; a WasmGC module ships neither, because the
collector belongs to the browser. The existing dead-code elimination and
identical-code-folding do the rest.

But it only stays winnable if it is measured from the start, which is why this
is numbered here and not last. A budget adopted after the fact is a budget that
is already blown.

---

## Phase 24 — Concrete error types

**Goal:** `impl Error for MyType` compiles, and the specification stops
describing a language that is not there.

This is **the largest gap between the specification and the compiler**, and it
is in the feature the language leads with. §7.2 declares an `Error` trait with
`message` and `cause`. §7.6 promises `errors.chain`, `errors.is<T>` and
`errors.as<T>`. §7.5 shows a `LoadError` enum being returned in an error slot.
None of it exists: an `error` today is a struct with one string field in all
three backends, `errors.new` makes one, `err.message()` reads it, and there is
no `Error` trait to implement.

The section used to make this conditional — *once trait objects land it becomes
an alias for `Option<dyn Error>`*. Trait objects landed in Phase 2. What is left
is the work, not a dependency, and the conditional was doing the job of hiding
that.

`std/errors` has been honest about it the whole time, in a comment on a function
that exists because the real one does not:

> A stand-in for `errors.is<T>`, which needs concrete error types carrying their
> own data — the trait-object half of the error design. Matching on text is
> weaker and says so.

**What it touches.** An `error` value is `error_record`, a one-field GC struct,
in the Wasm backend, the bytecode VM and Cranelift alike, with dedicated
`ErrorNew` and `ErrorMessage` operations in MIR. Making it hold any value
implementing `Error` means changing that representation in three places that
must agree, with `message()` becoming dynamic dispatch through the vtable
machinery that already exists for `dyn Trait`.

The order that keeps each step runnable:

1. **`Error` in the prelude**, beside `Display` and `Debug`. It is an ordinary
   trait declaration; nothing else changes yet.
2. **Coercion into the error slot.** A value whose type implements `Error`,
   returned where an `error` is expected, is accepted. This is where the taint
   analysis and `check` have to keep working unchanged.
3. **`message()` becomes dispatch.** `errors.new` keeps working by producing a
   built-in type that implements the trait, so every existing call site and
   every test that reads a message is unaffected.
4. **`cause`, and `errors.chain`.** The trait's second method, with the default
   the specification already gives it.
5. **`errors.is<T>` and `errors.as<T>`.** Downcasting, over the type tags that
   dynamic dispatch already carries.

**Exit criterion:** Appendix A gets its `LoadError` back. The test that compiles
it will say whether that worked, which is the point of having added it.

**Steps 1 and 2 are done, and the exit criterion is met.** `impl Error for
MyType` compiles, a value of such a type may be returned wherever an `error` is
expected, and Appendix A has its `LoadError` — checked on every run, on the
checker and the web target, and in the differential corpus so all three backends
are compared on it.

The conversion turned out to need no change to the error representation at all.
`coerce` inserts a call to `message` at the point of conversion, so what reaches
MIR is an ordinary call followed by the `ErrorNew` that already existed — three
backends lowered it without knowing the trait is there.

One thing was got wrong and is worth recording, because the wrong version
compiled and ran. The first attempt made `expect_ty` *accept* a type
implementing `Error` without inserting the conversion. Every check passed, and a
raw struct reached `err.message()` and trapped at run time. **A type rule that
permits something the lowering does not perform is worse than no rule**: the
acceptance was removed, the conversion is applied at the two return sites, and
any site that still needs it fails to compile rather than trapping.

### What is left of this phase

**The error still carries only its message.** The conversion renders the text
where the failure happened — which is where its context is freshest — and drops
the value. So:

- `cause`, the trait's second method, has nowhere to live yet.
- `errors.chain`, `errors.is<T>` and `errors.as<T>` need the original value
  alongside the message.

That is a change to `error_record` — one field today, in the Wasm backend, the
bytecode VM and Cranelift alike — to carry the value and its type tag beside the
text. Steps 3, 4 and 5 of the plan above, and they are one indivisible change to
the type the taint analysis is built on, so they want a clear run rather than
the end of one.

---

## Phase 25 — `std/html`

**Goal:** elements described as values, and only what changed written to the
page.

`std/dom` makes a program say `create`, `set_text`, `append` in sequence.
`examples/page` had thirty lines of that to draw one table row.

**The description.** A `Node` is a tag, its attributes and its children, held as
an ordinary value. Two constructors — `el` and `txt`, taking the tag as a string
— rather than a hundred and ten named tag functions, so the module is a page of
code and a tag added to HTML next year works the day it ships. A mistyped tag
becomes a `<flase>` in the document rather than a compile error, and that is the
price.

**The diff.** `mount` builds and remembers; `update` compares a new description
against the remembered one and writes only the difference. A tag change rebuilds
that element; everything else is reused in place, so it keeps its focus, its
scroll position and its listeners.

Children are matched **by key where there is one, by position where there is
not**, and a reused element is moved only when its position actually changed.
Measured on the demo, filtered to thirty-five rows:

| | elements created | text writes | moves |
|---|---|---|---|
| Re-render, same data | 0 | 0 | 0 |
| Sort by name | 0 | 0 | 33 |
| Reverse that sort | 0 | 0 | 34 |

Zero creations and thirty-three moves for a thirty-five row reorder is what the
key buys. Without one the same sort rewrites the text of every cell it moved
past — five hundred writes instead of thirty-three moves.

**Held by the caller, not in a table here.** `mount` returns a `Mounted` and
`update` takes it. A page with three islands has three of them, and nothing in
this module has to know how many exist or when one goes away.

Two things were settled before the phase and did not change: element trees are
built with **ordinary functions**, not a template language and not a change to
the grammar; and updates work by **comparing trees**, not by a reactive graph,
because a value that silently registers a dependency when it is read is hidden
control flow in a much larger dose than the sigils the language already refuses.

The diff costs about 4.5 KB, which is why the island budget moved from 16 KB to
24 KB. The budget that measures the language rather than the demo did not move.

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
| 4 — WebAssembly backend | ✅ every construct the language has; `str` is a language-owned WasmGC Unicode-scalar array with bounded JavaScript boundary conversion |
| 5 — Concurrency | ✅ `async`/`await`, the state machine, `Task<T>`, the combinators, `Share`. ❌ real parallelism on any target — the platform forbids it today |
| 6 — Standard library | ✅ thirteen modules written in Kite, tested on both backends, and `@derive(Debug, Hash, Encode, Decode)` as a source-to-source expansion |
| 7 — Layout and DOM renderer | ⬛ built, then removed at Phase 16. The renderer painted positioned elements, which is what made a Kite application unstylable by anyone else's CSS |
| 8 — Canvas renderer | ⬛ partly removed at Phase 16. Drawing, the glyph atlas and `std/text` stay; canvas as a whole-application renderer, with its parallel accessibility tree and damage tracking, does not |
| 9 — Native backend | ✅ Cranelift AOT and JIT on macOS and Linux, with a precise generational collector over Cranelift's stack maps; three backends compared. ❌ Windows, which the collector's frame walk cannot read yet and which is refused rather than left to corrupt |
| 10 — Tooling | ✅ fmt, doc, fix, test, bundle, `--explain`, the language server, and a package manager that resolves semver across the whole graph |
| 11 — Networking | 🟡 the client, and a server that listens on a real socket through a generated Node adapter. ❌ `wasi:http/incoming-handler`, which needs the component model |
| 12 — Cryptography | ✅ hashing, HMAC, PBKDF2, randomness, constant-time comparison, E0600, and AES-GCM, Ed25519 and X25519 over opaque key handles. ❌ Argon2, which no host has |
| 13 — Documentation site | ✅ four pages, the reference generated from the library, and a playground that is the compiler |
| 14 — Editor support | ✅ the language server and a VS Code extension over it, with rename, references, and inlay hints for solved generic arguments |
| 15 — Distribution | 🟡 CI, cross-compiled builds, Sigstore signing, Homebrew/Scoop/AUR manifests rendered from the release's own checksums, `kitec.wasm` as an artefact. ❌ nothing published: no tag has been pushed |
| 16 — Demolition | ✅ complete — 19,700 lines out; build and tests green with nothing rendering |
| 17 — `JsValue` / `externref` | ✅ complete — crosses, is held, survives an `await`, refused off the web. ❌ collection asserted, which needs a heap snapshot |
| 18 — `std/js` primitives | ✅ complete — 23 primitives, a fixed ~90-line host block, throws caught as errors. `js.func` moved to 20, `js.await` to 21 |
| 19 — Resident runtime, real clock | ✅ complete — `resident` beside `drive`, real clock, zero timers when idle |
| 20 — `std/dom` | ✅ complete — no externs, `Option` for absence, events with cancel, and a real page in `examples/page` |
| 21 — Rejections as errors | ✅ complete, and the phase was rescoped: promises never needed language support. The straight-line `await` form is open, and marked as comfort |
| 22 — Interop backwards | ✅ `api.js` and `api.d.ts` from `kitec build`, verified with real `tsc`. `@export` proved unnecessary. ❌ the Vite plugin |
| 23 — Size gate | ✅ complete — four budgets in CI; 388 B for hello world, 2 KB for a DOM change, 5.7 KB for the island |
| 24 — Concrete error types | 🟡 `impl Error for MyType` works on all three backends and Appendix A compiles with its `LoadError`. ❌ the error carries only its message, so `cause`, `chain`, `is<T>` and `as<T>` wait on the representation |
| 25 — `std/html` | ✅ complete — descriptions, a keyed diff that writes only what changed, and `examples/page` written against it |

769 tests: unit tests per crate, an annotated compile-fail corpus, a
differential corpus that runs every program on **three** backends and compares,
the standard library's own suite on two of them, the host boundary and a real
socket under Node, both string
representations compared against each other and against the VM, every example on
the site, size budgets, and the specification's own Appendix A.

### What is deliberately not done

Four things are absent on purpose rather than pending, and each is recorded
where the decision was made:

- **Real parallelism.** WasmGC references cannot cross a thread boundary, and
  the VM's values are `Rc`-based. `Share` is enforced *now* so the day either
  changes, no source does.
- **`Eq` as a derivable trait.** `==` is structural on every Kite value on
  every backend already. A derived `Eq` would be a second spelling for what the
  language does anyway, and a second spelling is a chance for two answers. The
  roadmap asked for four things wanting one machine; it turned out to be three.
- **`Display` as a derivable trait.** How a type presents itself to a person is
  a design decision, and a `Password` whose derived form printed its field is
  the case where a mechanical answer being wrong actually costs something.
- **WebGPU.** The list below puts it third among things to cut, and it is still
  right: Canvas2D is a complete product, and WebGPU is an optimisation on top
  of one.

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

The estimates above were for the original plan and held roughly. Phases 16–23
are not estimated, for a reason worth stating: the two lines that cost the most
in the first fifteen — layout and the canvas renderer, around five months
between them — are the two being removed. **A schedule is not the thing this
roadmap got wrong. What it got wrong was building a UI layer before asking
whether anyone else's stylesheet could touch it**, and no amount of estimating
would have caught that.

---

## Where to be willing to cut

This list was written before any of it was built, and it held up: the two
things at the top were the two that stayed unbuilt longest, and the one thing
still uncut is the one that was third.

1. ~~**Native backend (Phase 9).**~~ Built. The bytecode VM covered native
   execution adequately for a long time, and Cranelift was a performance
   upgrade rather than a capability — which is exactly why it could wait until
   everything a language actually needs was there.
2. ~~**Canvas renderer (Phase 8).**~~ Built, then mostly cut at Phase 16 — and
   the ordering advice was righter than it knew. DOM rendering shipped first
   and stayed the accessible one; what the list failed to ask was whether a
   canvas renderer should be a *peer* of the document at all. It should not.
   The thing worth cutting was one level up from the thing this line named.
3. **WebGPU path.** Still cut, still deliberately. Canvas2D is a complete
   product; WebGPU is an optimisation on top of one.
4. ~~**Generics.**~~ Built, in Phase 2. The note that error handling could not
   be added later is the half that mattered, and it was heeded.

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
