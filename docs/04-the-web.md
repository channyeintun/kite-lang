# The web model

What a Kite program is on the web, and how it reaches the browser.

> **This document replaces `04-stdlib-ui.md`.** That one described a UI layer
> that computed its own layout in Kite and painted it through two peer
> renderers. It was built, it worked, and it was removed — see
> [the roadmap](06-roadmap.md#the-direction-changed-at-phase-16) for why. The
> short version is below.

---

## 1. The division of labour

> **HTML is the document. CSS styles it. Kite replaces JavaScript, and nothing
> else.**

That sentence is the whole design. Everything below is a consequence of it.

The previous design took a different one: Kite owned the pixels, and the DOM was
one of two ways to put them on screen. Its renderer emitted absolutely
positioned `div`s, styled inline, under generated names, with every box
`aria-hidden` and the document's structure carried by a parallel semantics tree.
Nothing in a Kite application was a `<button>`.

The cost of that is exact and it is fatal: **no stylesheet written by anyone
else could address any part of a Kite application.** Not Tailwind, not
Bootstrap, not a design system a company already owns. A Kite application could
be styled by Kite and by nothing else, which made the language a competitor to
Flutter Web rather than an alternative to JavaScript.

## 2. What using somebody else's CSS requires

These are not preferences. Each one is what a third-party stylesheet needs in
order to function at all, and together they decide most of the API.

| Requirement | Why |
|---|---|
| **Real tags** | Bootstrap styles `.btn` on `<button>`, `.form-control` on `<input>`, `.table` on `<table>`. A tree of `div`s takes none of it. This is what rules out a `Style` struct that lowers to CSS: generated classes on generated elements are exactly as unreachable as inline styles were. |
| **Class names pass through unchanged** | No hashing, no scoping, no rewriting. `class="flex items-center gap-2"` arrives as written. |
| **Light DOM only** | Blocking outside stylesheets is what a shadow root is *for*. Any design built on web components fails here. |
| **No inserted elements** | `.card > .title`, `li + li` and `:nth-child` read the real tree. A framework that adds a wrapper for its own bookkeeping breaks third-party CSS in ways that are miserable to find. |
| **State in attributes** | `:hover`, `:focus-visible`, `:checked`, `[aria-expanded]`, `[data-state]`. Hover stops being something the model records and the program repaints — it stops crossing into Wasm at all. |

One further consequence, easy to miss: **class names want to be literal strings
in `.kite` source.** Tools that generate CSS on demand scan source files for
them. A name assembled at run time is a name the scanner cannot see — the same
discipline JSX users already live with.

## 3. Where a Kite program sits

**Inside a page, owning parts of it.** Not owning the whole page as a
single-page application.

The general case contains the special one: attaching to `<body>` *is* the
single-page application, so nothing is lost by starting at the more general
end. Starting at the other end means the island case needs a second mechanism
bolted on afterwards.

It also makes the cost of a Wasm module optional rather than structural. A page
can be served complete — by a server, a static generator, anything — and have
Kite enhance the three parts that need real logic. Downloading and starting a
module is then a choice made per feature, not a tax on the first paint of every
page.

## 4. Reaching the browser

Wasm cannot touch the DOM. Every browser feature is a JavaScript function
handed to the module, and the only question is how those functions are written.

### Three layers

```
    application code
          │
          ▼
    ┌───────────────┐
    │  std/dom      │   typed, opaque Element, Option<T> for absence
    └───────┬───────┘   ordinary Kite — no extern declarations
            ▼
    ┌───────────────┐
    │  std/js       │   ~15 primitives, fixed forever
    └───────┬───────┘   every call returns (JsValue, error)
            ▼
      the host, and
      about 40 lines
      of JavaScript
```

**`std/js`** is the floor: `global`, `get`, `set`, `call0`…`call4`, `new`,
`func`, `await`, `same`, `is_nil`, `instance_of`, and conversions both ways.
Full list and rationale in [the specification](../SPECIFICATION.md#15-foreign-function-interface).

**`std/dom`** is written over it, in Kite, with no `extern` declarations left in
it at all.

**Anything else is written the same way.** A user who needs an API the standard
library never covered writes it in Kite. They do not write JavaScript, and they
do not wait for the compiler to change.

That last point is the argument for the whole arrangement. One `extern` per
browser feature is faster and better typed — and the browser has thousands of
features, so the first one the library missed would send a user off to
hand-write a JavaScript host object. **A language whose way to extend it is "go
and write the other language" has already lost its own argument.**

### What it costs

A name looked up when the program runs instead of fixed when it compiles. Two
consequences:

- **A little speed.** Property lookup by a repeated string is something
  JavaScript engines optimise heavily, and a DOM call spends most of its time
  inside the browser. Where it matters — inside the standard library, on calls
  made constantly — `extern` is still available and still used.
- **A mistyped name compiles.** This is the real weakness. It is survivable
  because every primitive catches (below), because the typed layer is written
  once and tested, and because the long tail can eventually be generated from
  the browser's own interface definitions.

### Handles are references, not numbers

A host object is a `JsValue`, lowered to `externref`. The previous design used
an `int` indexing a table on the JavaScript side, and that cannot be made to
work:

- an `int` is `Share`, so it can be sent to a worker where the table does not
  exist;
- the table can never shrink, because nothing can tell JavaScript that Kite
  dropped a number and WasmGC has no finalizers;
- **identity is broken** — finding the same element twice yields two different
  numbers, and every event handler ever written asks "is this my element?"

A reference fixes all three, and settles lifetime completely: on the web the
Wasm heap *is* the JavaScript heap, so an element held by Kite, whose listener
holds a Kite closure, is a cycle the one collector collects.

### Everything catches

```kite
let (node, err) = js.call1(document, "querySelector", js.of_str("#form"))
check err
```

A host exception must never cross the boundary raw. Today one unwinds through
the Wasm frames and takes the scheduler with it, so a single mistyped name does
not fail a call — it stops every running task in the program.

The taint analysis makes the check mandatory, which also removes JavaScript's
commonest bug by construction: a missing property yields `undefined`, and
`undefined` silently becoming `0` is untraceable. `as_num` returns an error
instead.

**Absence is `Option<Element>`.** Never a zero handle. The old `std/dom` returned
handle 0 and documented that every call tolerated it — a null object, in a
language whose specification calls the tolerated zero value Go's commonest
production bug.

### Keeping the untyped part contained

`JsValue` is untyped, and if it spreads into application code the type system
has stopped helping.

It is contained by ordinary visibility. A `pub struct` with unmarked fields can
be held and passed but not read or built, so `Element` outside `std/dom` is a
closed type:

```kite
pub struct Element {
    raw: JsValue        // no `pub`
}
```

With **one deliberate door**: `dom.raw(e)` and `dom.wrap(v)`. Sealing it
completely sounds safer and is not — the user who needs one uncovered method
cannot reach their own element, and what they do instead is rebuild a parallel
untyped world beside the typed one. One marked escape is a boundary that holds.

An admission that belongs here rather than in a comment nobody reads: **this
puts `innerHTML` back within reach.** The old `std/dom` refused a `set_html` and
said it never would, because building markup from strings is the commonest way
to grow an injection bug. That guarantee is now narrower and must be stated
narrowly — the typed layer does not hand you the loaded gun; the escape hatch is
not the typed layer.

## 5. Events

A listener is a Kite closure crossing as a reference. `js.func(f)` hands
JavaScript something it can call; the closure's lifetime is the listener's
lifetime, traced by the one collector. A registry of numbered handlers would
pin every listener that ever existed.

Registration returns a `Subscription` with an explicit `cancel`, because nothing
else can cancel it and explicit teardown is squarely inside this language's
tolerance for verbosity.

**A closure cannot capture a `var`**, and that rule is not weakened for this.
The idiom is to capture a `let` handle to a struct and change it through a
function taking `var` ([spec §4.5](../SPECIFICATION.md#45-closures)) — mutation
spelled out in a signature rather than implied by a capture list.

### A handler belongs to the node, not to a table

`std/html` takes handlers as attributes — `html.click(|e| { … })` sits in the
same list as `html.class`. This was not the first design, and the first one is
worth recording because it looked fine.

A `Node` used to be pure data with nowhere to put a closure, so the only way to
react was a `data-action` naming the control and one delegated listener that
matched on the name. It works, and it stays readable at about forty controls.
The largest program written against it had a hundred and forty-six, and
**eighty-six of them were never matched**: they drew, took the press, and did
nothing. Nothing could have reported it — a string on a button and a string in
a `match` are not connected by anything a compiler can see. Two more controls
carried no `data-id`, so the handler built `/sessions//register` from the
context the closure would have captured for free.

Both are unrepresentable now. What it costs is that `Node` is no longer
`Share`, which is nothing here: a description exists to become elements in one
document, and `Mounted` already held a host reference.

The mechanism is one listener per element and event, attached the first time a
description asks for it, whose closure reads the newest handler out of the
element's `Live` record when the event arrives. So a repaint replaces a field
rather than a listener, and re-describing a tree costs no registrations at all.
`Live` is patched in place for exactly this reason — returning a fresh one, as
the diff used to, would leave every listener reading the description its
element was born with.

Two hazards designed for rather than discovered:

- **Reentrancy.** A call like `focus()` can fire a Kite handler synchronously in
  the middle of a Kite call. The task pump needs a guard.
- **Dropped errors.** A handler that starts an `async fn` returning
  `(T, error)` and drops the `Task` has dropped an error, and the taint analysis
  cannot see inside a `Task`. Either handlers return nothing, or the runtime
  reports an unawaited failing task. Silence is not available to a language
  whose main argument is that errors cannot be ignored.

## 6. Effects

Every modern browser API returns a promise. `js.await(p)` bridges one into a
`Task`, with a rejection arriving as an `error`.

Kite gets a simplification here that Elm structurally cannot have. Elm needs a
command algebra because it has no way to *do* anything — an effect must be
described as data for a runtime to perform. Kite has `async`/`await` and a
runtime that manages tasks, so an effect is a function that does the work and
sends a message back. No `Cmd`, no `Sub`, no interpreter.

## 7. Canvas

A `<canvas>` is an element in a page that a program draws into — a chart, a
game, a visualisation. That is what a canvas is on the web, and it is all it is
here now.

`std/canvas` and the drawing builtins are unchanged. What is gone is canvas as a
*whole-application renderer*: the parallel accessibility tree, the hidden
overlay input, and the per-frame damage tracking all existed to make a canvas
pretend to be a document, and the document is what documents are for.

The evidence for that split was already in
[docs/01 §6](01-platform-research.md#6-html-in-canvas-correcting-a-common-misreading)
and it reads more strongly now than it did then. What a canvas application
rebuilds — IME composition for Chinese, Japanese and Korean; password managers
and autofill; selection and clipboard across elements; spellcheck; find-in-page;
bidirectional cursor movement; screen-reader semantics — is not a feature list.
It is decades of behaviour people already know how to use, and the rebuild is
never as good. `std/text`'s line breaking stays, because a program painting its
own pixels still has to break its own lines.

## 8. Testing

The old design needed a third renderer that wrote a transcript, so that a frame
could be verified without a browser. That machinery is gone and the property it
protected is not: a program's output is now elements and attributes, which can
be read back and compared directly.

This matters more than it sounds. Driving a real browser from a tool is
unreliable for anything time-dependent — a tab that is not visible does not run
animation frames at all, and screenshots of it look convincing and are stale.
Verification belongs in a headless comparison of what the program produced, and
producing HTML rather than rectangles makes that comparison readable: a
difference reads as `<button class="btn" disabled>`, not as
`rect 12.0 40.0 88.0 32.0 0x2a2f3a`.

## 9. Describing elements

`std/html` is a description: a `Node` is a tag, its attributes and its children,
held as an ordinary value, and nothing exists in the page until `mount` walks
it.

```kite
html.el("tr", [], [
    html.txt("td", [html.class("num")], "\(r.id)"),
    html.txt("td", [], r.name),
])
```

**Two constructors, not one per tag.** `el` and `txt` take the tag as a string,
so the module is a page of code rather than a hundred and ten near-identical
functions — and a tag the specification adds next year works the day it ships.
A mistyped tag is a `<flase>` in the document rather than a compile error, which
is the price.

**`update` compares and writes the difference.** `mount` builds and remembers;
a new description is matched against the remembered one, and a reused element
stays where it is with its focus, its scroll position and its listeners intact.

Children are matched by key where there is one and by position where there is
not, and a reused element moves only when its position actually changed. On the
demo, a thirty-five row sort creates nothing and moves thirty-three elements;
without keys the same sort would rewrite the text of every cell it passed.

Element trees are built with **ordinary functions** — no template language and
no change to the grammar — and updates work by comparing trees rather than by a
reactive graph, because a value that silently registers a dependency when it is
read is hidden control flow.

**`update` reports what left.** State kept per row — what a `useState` inside a
list item would be, and what here is a `{str: T}` in the program's own store —
has to be dropped when the row goes, and the diff is the only thing that knows
which rows went. `html.departed(view)` is that list. Without it the map grows
for the life of the program, and the pruning is hand-written against a set the
caller has to re-derive.
