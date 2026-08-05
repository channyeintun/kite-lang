# Review before the freeze

A line-by-line pass over every file in the repository, against one question:
**is the language that ships the language the specification describes, and is it
the one worth never changing again?**

Kite's release promise is Go's — v1, and no breaking change after it. That makes
this review's job narrow and severe. A wrong number in a README is a nuisance
that can be fixed on any afternoon. A hole in a rule the compiler is supposed to
enforce becomes permanent the day the tag is pushed, because closing it later
rejects programs that used to compile.

So the findings are ordered by *how expensive they are to leave*, not by how
much work they are.

**State at the time of review:** `cargo build --workspace --all-targets` clean;
`cargo test --workspace --all-targets` **767 passed, 0 failed**; every `.kite`
file passes `kitec fmt --check`; every example compiles.

Nothing below was found by a failing test. That is the point of the exercise —
these are the things the suite is not looking at.

---

## A. The language itself

### A1. An `error` can be silently dropped — the headline guarantee has a hole

**Severity: highest. This is the finding this review exists for.**

```kite
fn risky() -> error {
    return errors.new("boom")
}

fn main() {
    risky()                 // compiles, runs, prints nothing
    io.print("carried on")
}
```

Output: `carried on`. Exit 0. No diagnostic, not even a warning.

The same holds for the pair shape:

```kite
fn pair() -> (int, error) { return 1, nil }

fn main() {
    pair()                  // both halves discarded, silently
}
```

This contradicts the language's central claim, in the exact words the project
uses to make it:

- `SPECIFICATION.md` §7.1 lists Go's flaws, first among them: *"**An error can be
  silently dropped.** `v, _ := f()` compiles, and so does simply never testing
  `err`."* — presented as the thing Kite fixes.
- `README.md`: *"**Errors are values, and the compiler enforces it** … Go's
  single biggest flaw, removed."*
- `crates/kite-diag/src/codes.rs:209`, E0302: *"Silently dropping errors is the
  single most common source of production failures in languages that permit
  it."*

The rules in §7.3 are written about **bindings** — R3 says *"An Unchecked
**binding** going out of scope is a compile error"* — and a discarded return
value creates no binding, so nothing in the analysis ever sees it. The taint
machinery is correct and complete for `let (v, e) = f()`. It simply never runs
for `f()`.

This is not an edge case on the target the language is built for. `std/dom` is
made almost entirely of functions returning a bare `error` — `set_text`,
`set_class`, `add_class`, `remove_class`, `set_attribute`, `remove_attribute`,
`set_style`, `set_value`, `set_checked`, `append`, `insert_before`, `remove`,
`prevent_default`, `stop_propagation`, `set_title` — so on the web the dropped
error is the *ordinary* shape, not an unusual one.

The standard library and the flagship example already do it:

- `std/html.kite:155` — `dom.set_text(made, node.body)`
- `std/html.kite:209` — `dom.set_text(old.el, next.body)`
- `examples/page/main.kite:102` — `dom.set_text(note, "…")`

Three sites, none deliberate, none visible to a reader as a decision.

**Why it cannot wait for v1.1.** Turning this into an error later rejects
programs that compile today. Turning it on now costs three call sites in this
repository and buys the guarantee the language is sold on.

**Proposed rule.** An expression statement whose type is `error`, or whose type
is a `(T, error)` pair, is `E0302`. To discard on purpose, write `_ = expr` —
`_` already means *a hole where a value would go* in return position and in
destructuring, so this spells the same idea in the one place it was missing.

---

### A2. `std/html`'s keyed reconciliation puts elements in the wrong order

`std/html.kite:286` skips the DOM move when the matched child's **old** index
equals its **new** index:

```kite
if from != i {
    dom.append(parent, live.el)
}
```

That test is not sound. `i` counts positions in the new list and `from` counts
positions in the old one; once a child has been inserted before this one, or
removed from ahead of it, the two indices can agree while the element's actual
position is wrong.

Smallest failing case — one insertion at the front:

| | |
|---|---|
| old children | `A` (key `a`), `B` (key `b`) — DOM: `A, B` |
| new children | `N` (key `n`), `B` (key `b`) |

- `i = 0`: `N` is new → built and appended → DOM `A, B, N`
- `i = 1`: `B` matches old index 1 → `from == i` → **no move** → DOM unchanged
- cleanup: `A` was not taken → removed → DOM **`B, N`**

The correct result is `N, B`. The list renders reversed, and stays reversed
until something forces a rebuild.

This is the module the README's headline demo rests on ("a thirty-five row sort
moves thirty-three elements and creates none"), and the bug needs only one
insertion or one removal ahead of a keyed child — the commonest thing a list
does.

The doc comment's promise — *"a list that did not reorder costs no DOM writes at
all"* — is worth keeping. It survives a watermark: skip the move only while the
new list is still matching the old one position for position, and append
everything once anything has been created, moved, or skipped past.

---

## B. Where the specification is wrong about the language

The specification says of itself: *"Where this document and the compiler
disagree, the compiler is right and the disagreement is a bug in this file."*
Ten such bugs. `crates/kite-driver/tests/spec.rs` compiles Appendix A and
nothing else — deliberately, and reasonably — which is why every one of these
survived.

### B1. `?T` is not Kite (§5.4 ×3, §11, §15.3)

`SPECIFICATION.md:509`, `:512`, `:515`, `:1134`, `:1553` write `?int`, `?V` and
`?T` as type syntax. `?` is not a token:

```
error[E0002]: invalid character `?` in source
  │ fn f(xs: [int]) -> ?int {
  │                    ^ not part of any Kite token
```

`docs/05-grammar.ebnf:291` states it outright — *"`?` is not a token in Kite"* —
and `crates/kite-lexer` has a test named `question_mark_is_not_a_token`. The
document and the grammar contradict each other on a point of syntax.

`:1134` is the worst of the five, because it is not a comment: the `Cache<K, V>`
example in §11 is presented as a working declaration and does not compile.

The spelling is `Option<T>`.

### B2. §12.1 calls `json.decode<User>`, which §10.4 says does not exist

`SPECIFICATION.md:1168`:

```kite
let (user, err) = await json.decode<User>(res.body)
```

§10.4 of the same document: *"There is no `json.decode<T>(text)`. Kite has no
turbofish."* There is no such function, the syntax has no parse, and
`json.decode` is not `async`. `User.decode(doc)` is the form, and §10.4 already
explains why.

This is precisely the failure `spec.rs`'s own header describes as having been
caught in Appendix A — *"used `use std/io`, `impl Error for LoadError` and
`json.decode<[Task]>`, none of which exist"* — still present one section
earlier, because the test only reaches the appendix.

### B3. §12.3 lists `sync.Channel<T>`, which does not exist anywhere

`SPECIFICATION.md:1248` names `sync.Channel<T>` as one of the explicit `Share`
wrappers. `std/sync.kite` has `Mutex` and `Atomic` and nothing else, and
`docs/02-concurrency.md:144` lists only those two.

§12.1 of the same document, fifty lines earlier: *"There is no channel type."*

### B4. `sync.Atomic<T>` is not generic

`SPECIFICATION.md:1248` and `docs/02-concurrency.md:144` both write
`sync.Atomic<T>`. The implementation is a non-generic `Atomic` over `int`, and
`std/sync.kite:101` gives the reason — *"an atomic of an arbitrary type is a
lock wearing a misleading name"*. The reasoning is right and the two documents
never got it.

### B5. §15.2's `std/js` table names three primitives that do not exist

`SPECIFICATION.md:1498–1510`:

| Specification | Actual |
|---|---|
| `js.new(name, args)` | `js.new0` … `js.new3` — a slice does not cross the boundary |
| `js.await(p)` | `js.settle(promise, done, failed)` — and the module explains at length why both halves are required |
| `js.is_nil(v)` | `js.is_nothing(v)` — one question rather than two, deliberately |

Absent from the table entirely: `at`, `length`, `kind_of`, `nothing`, `of_int`,
`as_int`, `str_or`, `num_or`, `bool_or`, `settle`, `SAFE_INTEGER`.

`:1498` says *"about fifteen primitives"*; `std/js.kite:9` says *"About twenty"*;
`README.md:156` says *"about twenty"*. The public surface is thirty-two
functions plus the `js.func` builtin.

### B6. §12.2 claims parallelism on the web that does not exist

`SPECIFICATION.md:1207`, in the target table:

> `wasm32-gc` (web) | Cooperative loop on the main thread; `task.parallel`
> offloads to an isolate pool backed by Web Workers | **Partially, today**

and `:1224` — *"For CPU-bound work on the web today, `task.parallel` runs a
function in a separate isolate."*

`std/task.kite:85`, in its own doc comment:

> **This is not parallelism today, on any target**, and the reason is a platform
> one rather than a design one.

The body is a sequential `for` loop with a `task.yield()` between items.
`README.md` agrees with the implementation — *"**No real parallelism, on any
target**"*. The specification is the only document claiming otherwise, and it is
claiming it in the table a reader consults first.

### B7. §3.1 promises `math.wrapping_add` and `math.checked_add`

`SPECIFICATION.md:188`: *"`math.wrapping_add` and `math.checked_add` are
available when the behaviour must be explicit regardless of build mode."*
Neither is in `std/math.kite`. Since overflow traps in debug and wraps in
release, this is the only escape the section offers from a build-mode-dependent
semantics, and it is not there.

### B8. §4.3's example uses a `Duration` type that does not exist

`SPECIFICATION.md:367` — `timeout: Duration`, built with `time.seconds(30)`.
`std/time.kite`'s `seconds` returns `int`; there is no `Duration`.

### B9. §15.5 cites "§17", and the document has sixteen sections

`SPECIFICATION.md:1596` — *"the reason §17 rejects reflection is untouched"*.
There is no §17. The rationale it points at is nowhere in the file.

### B10. §12.1 gives `task.timeout` the wrong shape

`SPECIFICATION.md:1194` — `task.timeout(t, duration)`. The function is
`timeout(work: Task<T>, ms: int) -> Option<T>`.

---

## C. Documentation that is stale or wrong

### C1. `std/canvas.kite` documents a design that was deleted

- `:30` — *"`std/dom` is being rewritten. Until then the host supplies one
  surface"*. `std/dom` is finished, 429 lines, and is not being rewritten.
- `:79` — *"`ui.wrap` is where it lives"*
- `:119` — *"`ui.paint_into` opens and closes one around a canvas node for you"*

`std/ui` was deleted. These are not archived notes; they are the module's live
reference text, published to `site/reference/canvas.md` and served from the
site.

### C2. `std/text.kite`'s header describes the two-renderer design

`:1–38` — *"beside the layout engine and away from the renderers, because a
renderer that made these decisions could disagree with the one next to it … the
point of computing everything in Kite is that **neither renderer decides
anything**"*, and *"which the browser-backed renderers can and a glyph-at-a-time
renderer cannot"*.

There is one renderer and no layout engine. `:1775` and `:2096` refer to
`ui.wrap`. Also published, as `site/reference/text.md`.

The decision to **keep** `std/text` is sound and recorded
(`docs/06-roadmap.md:1572` — a program painting into a `<canvas>` still needs
UAX #14). What is stale is only the account of who consumes it.

### C3. "Four", where the answer is five, six, or seven

`str` has **five** methods — the type checker's own note at
`crates/kite-types/src/lib.rs:3685` says so: *"`str` has: len, slice, index_of,
trim, code_at"*.

- `std/prelude.kite:315` — *"`str` has four methods … `len`, `slice`,
  `index_of` and `trim`"* — in a file whose own `hash_str` at `:733` calls
  `code_at`
- `std/json.kite:3` — *"the four string primitives the language has"*
- `docs/06-roadmap.md:271` — same claim
- `site/reference/prelude.md`, `site/reference/json.md` — the generated copies

`std/task.kite:9` — *"The compiler supplies four primitives — `task.yield`,
`task.park`, `task.wake_at` and `time.now`"*. The same file also calls
`task.finished` and `task.get`; `std/socket.kite` and `std/http.kite` call
`task.wait_host`. Seven.

- `README.md:84` and `site/README.md:84` — *"all written in Kite over four
  compiler primitives"*

### C4. CI's header says two backends

`.github/workflows/ci.yml:3–6` — *"every program in the corpus is compiled to
both backends, run on both … **Two** independent implementations that must
agree is what makes codegen bugs findable"*. It is three — Wasm, bytecode and
Cranelift — as `README.md:191` and `crates/kite-driver/tests/differential.rs`
both have it.

### C5. `size.rs`'s recorded numbers are stale

`crates/kite-driver/tests/size.rs` records "today's" size in each doc comment,
which is the right habit; two of the three have drifted.

| Test | Recorded | Measured |
|---|---|---|
| `hello_world_stays_small` | 388 bytes | **399** |
| `a_library_of_four_functions_stays_small` | 393 bytes | **618** |
| `a_dom_island_stays_under_twenty_four_kilobytes` | ~18.5 KB | 18,450 — correct |

The 393 → 618 drift is 57%. The budgets are generous enough that nothing fired,
which is exactly the case the file's own header warns about: *"a gate that fires
on every ordinary change gets raised until it means nothing"* — the inverse also
holds.

### C6. README's build transcript is 148 bytes out

`README.md:199`:

```bash
kitec build examples/hello.kite --emit wasm --out dist
# wrote dist/app.wasm (500 bytes), dist/app.js and dist/index.html
```

That command writes **648 bytes**.

### C7. The test count

`README.md:291` — *"768 tests"*. The suite reports **767** passing, and there
are no doc tests. `crates/kite-driver/tests/spec.rs:83` says *"765 tests"*.

### C8. `docs/02-concurrency.md` §5 describes machinery that was never built

`:205–235` describes, as present tense:

- a Worker isolate pool with `Share` values structured-cloned in and out
- `SharedArrayBuffer` zero-copy transfer of `buffer.*` payloads
- a COOP/COEP header requirement
- *"`kite build --target web` warns when a program uses `task.parallel` and
  prints the two header lines needed"*

None exists. There is no `--target` flag — the compiler takes `--emit wasm` —
and no such warning. The whole section is the plan, written as the state.

Its example also breaks a language rule the specification states without
qualification:

```kite
return filters.gaussian_blur(img, radius: 4.0)
```

`SPECIFICATION.md` §4.3: *"There are … no named arguments at call sites."* That
is a named argument. `filters` does not exist either.

### C9. `install.sh` points at a domain the site is not on

`install.sh:4`:

```
curl -fsSL https://kite-lang.org/install.sh | sh
```

`wrangler.jsonc:23–24` deploys to `kite-lang.dev` and `www.kite-lang.dev`. The
`.org` is not the site. `install.sh` is also not copied into `site/` by
`site/build.sh`, so even with the domain corrected the documented one-liner
fetches nothing.

### C10. `.gitattributes` has a rule for a directory that does not exist

`.gitattributes:25` pins line endings on
`crates/kite-driver/tests/golden/*.txt`. There is no `golden/`. `README.md:275`
records why: the golden transcripts went with the layout engine.

### C11. `--explain E0205` explains the wrong rule

`crates/kite-diag/src/codes.rs:134` gives E0205 the label *"not callable"* and
the text *"This expression is not a function."* The code is raised ten times in
`crates/kite-types/src/lib.rs`, and only one of them (`:3006`) is that. The rest
are method lookups — *"`str` has no method"*, *"a map has no method"* — for
which the explanation is simply about something else.

The file's own header: *"Codes are never reused for a different meaning."*

### C12. `E0102` is declared and never raised

`crates/kite-diag/src/codes.rs:77`. Dead, and listed by `kitec --explain`.

---

## D. Defects in text the compiler prints

Two diagnostic strings have a run of spaces where a line was joined without
trimming. Both reach users.

- **`crates/kite-diag/src/codes.rs:207`** — E0301, ten spaces:
  `"…test `err != nil` explicitly — in the          branch where the error is nil…"`.
  Printed by `kitec --explain E0301`, the code the specification uses as its
  worked example.
- **`crates/kite-types/src/lib.rs:3775`** — thirty spaces:
  `"a map has: len, keys, values; read with `m[key]`, which yields                              an optional…"`.
  Printed on every map-method error.

A mechanical sweep for the pattern across all Rust found these two and nothing
else.

---

## E. Tooling

### E1. `kitec fmt` lets two spellings of the same code through

```kite
if a < 0.5 * b{        // kitec fmt --check: silent, exit 0
```

`kite-fmt` decides whether a `{` opens a block or a struct literal by looking at
what precedes the name (`is_literal_head`, `crates/kite-fmt/src/lib.rs:252`).
Its list of "a value could start here" positions includes `+`, `-`, `*` and `/`
— but **Kite has no operator overloading**, so a struct literal can never follow
an arithmetic operator. Any condition ending in `<ident>` after one of those
four keeps whatever spacing it was written with.

`std/math.kite:270` is the live instance:

```kite
if abs(next - guess) < 0.0000000001 * guess{
```

The `kite-fmt` CI job asserts *"Every `.kite` file in the tree is formatted, and
formatting it again changes nothing"* — and passes over this, because the
formatter agrees with it. A formatter with two answers is the one thing a
format-on-day-one policy exists to prevent.

### E2. `serve.rs` leaves Node processes and temp directories behind

Twelve `kite-serve-*` directories in the system temp directory after a run, two
with a live `serve.mjs`. The suite is otherwise clean; this one leaks.

---

## F. Repository

### F1. A 3.3 MB unreferenced audio file is tracked

`assets/khin-maung-toe.m4a`. Nothing in the repository references it.
`wrangler.jsonc:8–12` records the reason: *"There was one, briefly … the music
demo's audio element … That demo is gone and nothing else on the site is
seekable, so the Worker went with it."*

It is also a commercial recording, in a repository about to be published under
MIT with a release pipeline attached. It is the largest file in the tree by an
order of magnitude.

### F2. Generated files tracked beside identical generated files that are ignored

`examples/page/api.js` and `examples/page/api.d.ts` are tracked.
`examples/page/app.js` and `examples/page/app.wasm` are gitignored
(`.gitignore:27–28`). All four come out of one `kitec build` invocation. The two
that are tracked are currently correct, and nothing checks that they stay so.

---

## G. Grammar

`README.md:248` calls `docs/05-grammar.ebnf` the *"Complete formal grammar."*

### G1. No production for `//!`

`DocComment = "///" { AnyCharButNewline }` is the only doc form. `std/html.kite`,
`std/dom.kite`, `std/js.kite` and `std/canvas.kite` all open with `//!`, the
lexer accepts it, and `kitec doc` renders it as the module's own text.

### G2. No production for `@derive(…)`

The grammar has `HostAttr` for `@host("…")` and nothing else.
`SPECIFICATION.md` §10.4 calls `@derive` *"one of the two attributes Kite
has"*, and Appendix A — the block the test suite compiles — opens with
`@derive(Decode)`. A grammar that cannot derive the program the specification
ships is not complete.

### G3. `RangeExpr` is misplaced

Listed under `PrimaryExpr` (`:229`) while defined as `Expr ".." [ "=" ] Expr`
(`:248`) — left-recursive through a production that is meant to be the base
case.

---

## What this adds up to

Two findings change the language: **A1** and **A2**. A1 is the one that must be
settled before a tag exists, because it is a rule that can only be tightened
while nothing depends on it. A2 is an ordinary bug, in the module the project's
main demonstration rests on.

Everything else is the documents having drifted from an implementation that kept
moving — which is what the specification's own preamble predicts and what its
own test was written to catch, one appendix at a time.

The pattern worth naming: **every one of these lives in the gap between what is
tested and what is claimed.** The suite checks that Appendix A compiles, that
sizes stay under budget, that every file is formatted, that the brand assets
match. It does not check that a number in a sentence is the number the tool
prints, that a function named in a table exists, or that a module's doc comment
names a module that is still here. The nine "four"s, the three `js.` primitives,
the `?T`, the isolate pool — none of them could survive a test, and all of them
survived the suite.

---

## Resolution

Every finding above was fixed, one commit each, in the order they are numbered
here. What changed in the language, as opposed to in the prose:

- **A1** added rule **R6** to [§7.3](SPECIFICATION.md#73-correlated-results-and-taint-analysis):
  an expression statement whose type is `error` or `(T, error)` is `E0302`, and
  `_ = expr` is the deliberate discard. That is a new statement form and a new
  rejection — the only change here that makes a previously-compiling program
  fail, which is exactly why it had to happen before a tag exists.
- **A2** changed no syntax, but it changed what `std/html` puts on a page.
- **B6** and **C8** turned out larger than the review said. The specification
  claimed real parallelism on `native-*` and `kbc`, not merely partial
  parallelism on the web; there is no thread spawned anywhere in the
  repository. `docs/02` described a Worker pool, COOP/COEP headers,
  `SharedArrayBuffer`, a `kite-rt/scheduler/` of three files and a
  `task.scope()` with `.cancel()` and `.start()`, none of which exist — and its
  cancellation section contradicted a decision `std/task` had made on purpose.
- **C2** had one instance the first pass missed, in `std/math`.

Two things are recorded as decided-not-to-do rather than done:

- The audio file is out of the tree and **still in git history**. Removing it
  from there means rewriting a published branch, which is the author's call.
- `math.wrapping_add` and `math.checked_add` (**B7**) are now recorded as absent
  instead of promised. Writing them is a small job; deciding whether the
  language wants them at v1 is not, and it is a decision, not a correction.

The suite is 769 passing, 0 failing, and a full run now leaves nothing in the
temp directory.

### What would have caught these

Nothing here was found by a test, and most of it could not be. But three of the
patterns are mechanical enough to be worth automating before the next drift:

1. **A number in prose beside a number a tool prints.** `size.rs` already keeps
   its measurements in a doc comment; the two that drifted drifted because
   nothing compares the comment to the assertion. The test could print both.
2. **A name in a table beside a name in a module.** §15.2's `std/js` table, the
   `STD_MODULES` list, and `site/reference.html`'s `MODULES` array are three
   hand-maintained copies of what the standard library contains. Two of them
   have been wrong this month.
3. **A doc comment naming a module.** `ui.wrap` survived in three files across a
   deletion of fourteen thousand lines. A grep for module names that no longer
   exist is four lines of shell.
