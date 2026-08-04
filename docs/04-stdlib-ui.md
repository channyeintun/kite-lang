# Standard library: the UI layer

One API. Two renderers. Layout computed in Kite so both agree exactly.

---

## 1. The architecture

```
        your Kite code
              │
              ▼
    ┌─────────────────────┐
    │  ui — Box, Text,    │   declarative widget tree
    │  Button, Input, …   │
    └──────────┬──────────┘
               ▼
    ┌─────────────────────┐
    │  Retained scene     │   diffed against the previous frame;
    │  graph              │   only changed subtrees are re-laid-out
    └──────────┬──────────┘
               ▼
    ┌─────────────────────┐
    │  Layout engine      │   flexbox subset, written in Kite,
    │  (Kite, no host)    │   over flat f64 buffers
    └──────────┬──────────┘
               ▼
        ┌──────┴───────┐
        ▼              ▼
  ┌───────────┐  ┌──────────────┐
  │DomRenderer│  │CanvasRenderer│
  │           │  │              │
  │ real DOM  │  │ 2D / WebGPU  │
  │ real a11y │  │ + ARIA tree  │
  └───────────┘  └──────────────┘
```

**Layout runs in Kite, not in the browser.** This is the decision that makes two
renderers viable: both receive the same computed rectangles, so a program cannot
look different between them. It also means layout is identical on native, where
there is no browser to ask.

The renderer is chosen at build time and can be switched at runtime:

```toml
[targets]
web = { entry = "src/main.kite", renderer = "dom" }     # or "canvas", or "auto"
```

`auto` picks DOM when assistive technology is detected or the viewport is small,
canvas otherwise — the same heuristic on both, because layout already agrees.

---

## 2. Why both, and not canvas alone

The full evidence is in [docs/01 §6](01-platform-research.md#6-html-in-canvas-correcting-a-common-misreading).
The short version:

| Assumption | Finding |
|---|---|
| HTML-in-Canvas is the future of the web | **Chrome-only**, origin trial M148–M151. Firefox and WebKit have *no implementation announced*. |
| HTML-in-Canvas lets you replace DOM with canvas | **Backwards.** `drawElementImage()` draws *DOM into canvas* — it exists so canvas apps can embed real accessible DOM. |
| Canvas UI is a solved problem | Flutter's CanvasKit needs a parallel hidden DOM semantics tree, and it is **off by default for performance**. Users must click an invisible "Enable accessibility" button. Text fields announce as "edit, blank." |

What canvas-only costs you, concretely, because the browser stops helping:

- IME composition for Chinese, Japanese, Korean input
- Password manager and autofill integration
- Native text selection across elements, and clipboard semantics
- Spellcheck
- Right-to-left cursor movement and bidirectional text
- Browser find-in-page
- Screen reader semantics, unless you rebuild the whole tree yourself

The instinct that GPU-composited retained-mode UI is the future is **right** —
Figma, Zed, Google Docs and Flutter all took it deliberately. What does not
follow is that the *standard library* should be able to target only canvas. So
canvas is first-class, and it is not the only option.

---

## 3. Widgets

The idiom before the widgets, because every example below is written in it.

Kite structs have no default field values: a literal names every field, or
extends a base with `..`
([spec §5.3](../SPECIFICATION.md#53-struct-literals)). A `Style` has ten
fields, and a call site that wrote out all ten to say *a centred row* would
bury the two that matter. So the layer ships **one authored default per
struct** — `ui.style()` is a function, which means there is exactly one place
to read to learn what a default is — and a call site extends it, or composes
the combinators that do:

```kite
ui.Style{..ui.row(), align: ui.Align.Center }    // name the two that matter
ui.spaced(ui.padded(ui.column(), 16.0), 12.0)    // or chain the common ones
```

The same constraint decides what is required and what is optional, with no
named arguments and no overloading needed: **required is a parameter, optional
is a field behind a default.** A widget that cannot work without an id and a
caption takes them positionally, and a call site that forgets one does not
compile — every call site, forever. Everything optional lives on a struct
reachable with `..`, where omitting it means accepting an authored default
rather than a compiler-invented zero.

A component is a **function that returns a node**. There is no component class
and no internal state — Kite has no mutable globals, so a component that
remembered something would have to be handed it, and the application's model
is what it would be handed.

```kite
use std/ui
use material

fn view_node(model: Model) -> ui.Node {
    let s = material.dark()

    var rows: [ui.Node] = []
    for i in 0..model.tasks.len() {
        let task = model.tasks[i]
        rows.push(material.checkbox(
            s,
            "task\(i)",
            task.title,
            task.done,
            material.when_focused(model.focused == "task\(i)"),
        ))
    }

    let header = ui.box_of(
        "header",
        ui.spaced(ui.padded(ui.Style{..ui.row(), align: ui.Align.Center }, 12.0), 8.0),
        [
            material.outlined_field(
                s,
                "draft",
                model.draft,
                "What needs doing?",
                260.0,
                material.when_focused(model.focused == "draft"),
            ),
            material.filled_button(
                s,
                "add",
                "Add",
                material.when_focused(model.focused == "add"),
            ),
        ],
    )

    let list = ui.box_of(
        "list",
        ui.spaced(ui.padded(ui.Style{..ui.column(), align: ui.Align.Stretch }, 12.0), 4.0),
        rows,
    )

    return ui.decorated(
        ui.box_of("app", ui.Style{..ui.column(), align: ui.Align.Stretch }, [header, list]),
        ui.filled(s.surface),
    )
}
```

[examples/todo.kite](../examples/todo.kite) is this program complete, with the
keyboard.

Declarative here means exactly one thing: **the tree is a value.** `view_node`
is a pure function from the model to a `Node`; nothing in it draws, and
nothing it returns can reach back and change the model it was shown. What it
does not mean is markup. The sparse literal this document used to sketch —
`ui.Box{ dir: ui.Col, gap: 16.0, children: [...] }`, every unnamed field
silently defaulted — is not legal Kite: it needs default field values, which
the spec does not have, and whether it should is
[§10 question 3](#10-open-questions). Until it does, the constructor functions
cost a few characters over markup and keep every omission an authored decision
with one place to read.

### The widget set

Kept small on purpose. Everything else composes from these.

| Widget | Purpose |
|---|---|
| `Box` | Layout container — the flex primitive |
| `Text` | Text with shaping and wrapping |
| `Image` | Raster and vector images |
| `Button` | Pressable, with focus and keyboard activation |
| `Input` | Single-line text entry |
| `TextArea` | Multi-line text entry |
| `Checkbox` / `Radio` / `Switch` | Boolean and choice input |
| `Slider` | Range input |
| `Select` | Dropdown choice |
| `Scroll` | Scrollable viewport |
| `Stack` | Z-ordered overlay |
| `Canvas` | Escape hatch for custom drawing (charts, games, visualisations) |
| `Portal` | Renders outside the parent's clip — modals, tooltips |

Today the split is: `std/ui` owns the tree, the layout and the paint —
`box_of`, `text_of`, `control`, `decorated` — and the interactive widgets are
functions in design-system packages
([packages/material](../packages/material) ships the Material 3 set: buttons,
fields, selection, navigation, progress). The table above is the set `std/ui`
itself grows as the renderers land, because the middle rows cannot stay a
package's problem: `Input`, `TextArea`, and `Select` are the widgets a
canvas-only design would force you to reimplement. Under `DomRenderer` they
are real elements and get IME, autofill, and selection for free. Under
`CanvasRenderer` they are backed by a hidden, positioned real element that
receives the events — the same technique Flutter and Google Docs use, and the
reason those widgets are in the standard set rather than left to users.

**None of that is built yet, and this is the gap that shows.** `domRenderer`
creates one kind of element — a `div` — for every node, because the paint
boundary is eight drawing calls (`rect`, `rrect`, `text`, `drrect`, `alpha`,
`font`, `clip`, `unclip`) and none of them can say *this one is a text input*.
So an editable field is a run of drawn text, and its caret is a literal `|`
glyph in that run: `packages/material`'s search field has to buy the caret's
width back out of its own padding to stop the placeholder moving. Every native
ability an `<input>` would have given free — the real caret, selection, IME,
autofill, the mobile keyboard — is either faked or absent.

There is a specific structural reason it cannot simply be added to the
renderer. A renderer never sees the tree; it sees `Frame`s, and a `Frame`
carries `content: str` — the text to draw — with no notion of that text being
*editable*. The edit state lives on `Control.edits`, on the tree, which is
flattened away before anything paints. Closing this needs three things
together: `Frame` carrying the edit state, a ninth drawing call meaning "a
field goes here", and an answer for what the canvas renderer does with it —
the hidden positioned element described above, which is why that paragraph
exists. It is the first call whose three implementations would differ in
*kind* rather than in medium, which is the property the differential suite
exists to protect, and the reason it is a proposal rather than a patch.

---

## 4. Layout

A **flexbox subset**, chosen because it is the layout model working web
developers already know, and because its algorithm is well specified and
independently implemented (Taffy, Yoga) — which means it can be validated against
a reference.

```kite
pub struct Node<Msg> {
    pub name: str
    pub style: Style           // where the box goes — the layout reads only this
    pub content: Content       // Empty, or Text(str); a box holds children instead
    pub children: [Node<Msg>]
    pub control: Option<Control<Msg>>   // this subtree is one control — see below
    pub decor: Decor           // how the box looks — the layout never reads it
}

/// Everything a control states about itself, in one value rather than five
/// loose fields, because the combinations are not independent: a meaning with
/// no identity is unreachable, and text edited by something that is not a
/// control is not a state anything can act on.
pub struct Control<Msg> {
    pub id: str                // who this is: hit-testing, focus order, semantics
    pub msg: Option<Msg>       // what activating it says, or nil for a place
    pub edits: Option<str>     // the text it is editing, or nil for not editable
    pub focused: bool          // set by `with_focus` over the whole tree
    pub enabled: bool          // out of focus order and inert when false
}

pub struct Style {
    pub axis: Axis             // Row | Column
    pub justify: Justify       // Start | Center | End | SpaceBetween
    pub align: Align           // Start | Center | End | Stretch
    pub width: Option<float>   // fixed, or nil to size from the content
    pub height: Option<float>
    pub grow: float            // share of the leftover main-axis space
    pub padding: Insets
    pub gap: float             // between children, not around them
    pub size: float            // the font the node is measured *and* drawn in
    pub weight: int
}
```

Appearance — fill, ink, border, corner radius, opacity, centring — is `Decor`,
a separate struct riding on the node. The separation is the invariant the
two-renderer design rests on: **geometry is a function of style and content
only.** The layout never reads `decor`, so a renderer may draw a box any way
it likes and cannot move one; a test asserts it, by laying out the same tree
with every decoration stripped and comparing frames.

Deliberately excluded from v1: grid; wrapping; `flex-shrink` and `flex-basis`;
margins (gap and padding say the same thing without collapsing rules);
percentage sizes; minimum and maximum constraints; absolute positioning (use
`Stack`); floats; `z-index` beyond `Stack` ordering; and CSS cascade of any
kind. There are **no stylesheets** — style is a value, passed explicitly, and
therefore type-checked, refactorable, and dead-code-eliminable.

### Implementation

What ships today in [std/ui.kite](../std/ui.kite) is the simple thing: a
recursive `measure` bottom-up, then `arrange` top-down, over the node tree
itself — correct first, and small enough to read in one sitting. Where it goes
when profiling says the walk is the cost is a flat arena:

```kite
// Layout works over parallel flat buffers, not GC object graphs.
// buffer.F64 is linear memory, so this is cache-friendly and avoids
// WasmGC's lack of flat aggregates in arrays.
struct LayoutArena {
    var x:      buffer.F64
    var y:      buffer.F64
    var width:  buffer.F64
    var height: buffer.F64
    var flags:  buffer.U32
    var first_child: buffer.U32
    var next_sibling: buffer.U32
}
```

This is exactly the case
[docs/01 §3](01-platform-research.md#3-wasmgcs-limitations-and-how-kite-avoids-each)
flagged: WasmGC arrays of structs are arrays of *references*, so a hot numeric
inner loop would chase pointers. `buffer.F64` gives a flat linear-memory buffer,
and the layout engine is the main consumer of that escape hatch.

The passes are the same two either way — measure intrinsic sizes bottom-up,
resolve flexible lengths top-down — and nothing in the API names the
representation, which is what makes the arena a swap rather than a rewrite.
With it come dirty flags: subtrees whose inputs are unchanged are skipped, so
a typical frame re-lays out only the changed branch.

---

## 5. Text

Text is the hardest part of any UI toolkit and the place a canvas renderer is
most likely to be wrong.

| | `DomRenderer` | `CanvasRenderer` |
|---|---|---|
| Shaping | Browser | HarfBuzz compiled to Wasm |
| Line breaking | Browser | UAX #14, in Kite |
| Bidirectional text | Browser | UAX #9, in Kite |
| Font fallback | Browser | Explicit font stack + `document.fonts` query |
| Selection | Native | Reimplemented over hit-test rectangles |
| IME | Native | Hidden real `<input>` overlay |

Both paths must produce identical line breaks and identical measured widths for
layout to agree, so the text measurement API is a single interface with two
implementations, and it is validated by a golden-image test suite across scripts:
Latin, Cyrillic, Arabic (RTL), Hebrew (RTL), Devanagari (complex shaping), Thai
(no word spaces), Chinese, Japanese, Korean, and Burmese (complex reordering).

Shipping HarfBuzz costs roughly 200 KB compressed. It is loaded lazily and only
by the canvas renderer, so DOM-rendered applications never pay for it.

---

## 6. Events and state

An application is three exported functions and a model the host holds between
events:

```kite
pub fn init() -> Model
pub fn view(model: Model)
pub fn update(model: Model, event: int, x: float, y: float, key: str) -> Model
```

The model never crosses the boundary as data — it is a Wasm reference the page
holds and hands back, opaque to JavaScript, which is what lets it be any Kite
type at all — and `update` returns a new model rather than changing one. There
is nowhere else to keep state: Kite has no mutable globals, so the shape every
state-management library eventually converges on arrives without a library,
and with no way to draw from an update or to mutate from a view. It is the Elm
architecture, chosen because it needs no new language concept — a `struct`, a
`fn`, and a `match`.

Events come through one door. A click fills `x` and `y`; a key press fills
`key`; a new kind of event is a new constant rather than a new export, and a
program that ignores a kind never tests for it. Turning a point into a
decision is a hit-test against the same tree `view` drew. `ui.control_at`
answers with the id of the control the point landed in, and `ui.msg_at` answers
with what that control *says* — a value the application built when it built the
node, rather than a string it has to recognise. Dispatch is therefore an
exhaustive `match` over the application's own vocabulary, so a control whose
branch was never written fails to compile instead of laying out perfectly and
doing nothing when pressed:

```kite
fn frames_of(model: Model) -> [ui.Frame] {
    return ui.layout(view_node(model), viewport())
}

pub fn view(model: Model) {
    ui.paint(frames_of(model))
}

fn clicked(model: Model, x: float, y: float) -> Model {
    let tree = view_node(model)
    let frames = ui.layout(tree, viewport())
    let hit = ui.control_at(frames, x, y)
    if hit == nil {
        return model
    }
    // A click both moves focus and activates, and the two are separate facts:
    // every control takes focus, and only a control with a meaning acts.
    let focused = Model{..model, focused: hit }
    let said = ui.msg_at(tree, frames, x, y)
    if said == nil {
        return focused
    }
    return act(focused, said)
}

/// Exhaustive, which is the point: a control added with a new variant will not
/// compile until it is handled here.
fn act(model: Model, msg: Msg) -> Model {
    return match msg {
        Add => added(model),
        Toggle(at) => toggled(model, at),
    }
}

fn added(model: Model) -> Model {
    let title = model.draft.trim()
    if title.len() == 0 {
        return Model{..model, message: "nothing to add" }
    }
    return Model{
        ..model,
        tasks: concat(model.tasks, [Task{ title: title, done: false }]),
        draft: "",
        message: "added \(title)",
    }
}
```

Because `update` is pure, testing the application is calling it —
`EVENT_KEY()` is the application's own name for the door's keyboard constant:

```kite
var model = init()                                    // two tasks in it
model = update(model, EVENT_KEY(), 0.0, 0.0, "m")     // typed into the draft
model = update(model, EVENT_KEY(), 0.0, 0.0, "Enter")
assert(model.tasks.len() == 3, "enter adds the draft")
assert(model.draft == "", "and clears it")
```

No mock DOM, no test renderer, no async in the test.
[examples/todo.kite](../examples/todo.kite) drives itself exactly this way in
its `main`, which is how the bytecode target — where there is no page to click
on — exercises the same program.

The model is immutable, so it is `Share`
([docs/02 §4](02-concurrency.md#4-share-the-invariant-made-nearly-invisible)),
so it can move across tasks without ceremony.

One layer is deliberately absent, not forgotten. An **effect value** — how an
update would ask for I/O without performing it, keeping the purity the test
above leans on — is specified in
[proposal 0001 §7](proposals/0001-typed-messages.md#7-effects-unstarted-tasks)
and not yet built: it needs the loop to start what an update describes, and the
loop needs an application ABI this document does not yet name.

The **typed message layer** it used to wait beside has landed; see
[§10 question 4](#10-open-questions).

---

## 7. Accessibility

Under `DomRenderer` it is largely automatic — widgets emit correct elements and
ARIA attributes, and the browser does the rest.

Under `CanvasRenderer`, Kite maintains a parallel ARIA tree, the same technique
Flutter uses. Where Kite differs is that it fixes Flutter's known failures:

| Flutter Web problem | Kite's approach |
|---|---|
| Accessibility **off by default** behind an invisible button | **Always on.** The semantics tree is built during layout, which already walks every node, so the marginal cost is small. |
| Text fields announce as "edit, blank" — no `<label>` emitted | A field takes its label as a positional parameter — `outlined_field(s, id, value, label, …)`. Omitting it is a compile error, not a runtime warning. |
| Lighthouse reports a perfect score inaccurately | `kite check --a11y` audits the widget tree at build time — contrast ratios, missing labels, focus order, touch target sizes. |
| Semantics drift from visuals | The semantics tree is derived from the *same* scene graph that renders, so it cannot go stale. |

Making the label a required parameter is the single highest-leverage decision
here — the *required is a parameter* rule from §3, doing accessibility work. A
parameter is checked by the compiler on every build, for every developer,
forever — whereas a lint is disabled and a runtime warning is ignored.

---

## 8. The canvas renderer

```kite
pub trait Renderer {
    fn begin_frame(var self, size: Size)
    fn draw_rect(var self, r: Rect, style: RectStyle)
    fn draw_text(var self, run: TextRun, at: Point)
    fn draw_image(var self, img: ImageHandle, dst: Rect)
    fn push_clip(var self, r: Rect, radius: float)
    fn pop_clip(var self)
    fn end_frame(var self)
}
```

`CanvasRenderer` implements this over Canvas2D by default and WebGPU when
available. It batches draw calls by material, keeps a glyph atlas on the GPU, and
only redraws damaged rectangles.

Because `Renderer` is an ordinary trait, a third backend is a normal library —
a Metal or Vulkan renderer for native desktop, an SVG renderer for server-side
rendering, or a test renderer that records draw calls for golden-image tests.

### If HTML-in-Canvas ships broadly

It becomes an *optimisation inside* `CanvasRenderer`, not a change to the API:
`Input` and `TextArea` would be composited via `drawElementImage()` instead of a
hidden overlay, recovering native text behaviour inside the canvas path. Because
it is confined to one trait implementation, adopting it is a library change with
no effect on application code — and if it never ships beyond Chrome, nothing was
staked on it.

That containment is the whole point of the two-renderer design.

---

## 9. The rest of the standard library

```
std/
  core        int, float, str, slice, map, option, result, iterate
  errors      Error trait, new, wrap, chain, is<T>, as<T>
  fmt         Display, Debug, number and date formatting
  math        arithmetic, trig, random, checked/wrapping/saturating ops
  time        Instant, Duration, Date, timezone-aware formatting
  io          print, error, read_line
  fs          files and directories (native + WASI; unavailable on web)
  net         http client and server (native), fetch (web)
  json        encode/decode with compile-time derivation
  toml        parse/emit
  task        Task, scope, all, both, race, timeout, parallel
  sync        Mutex, Atomic — only for genuinely shared mutable state
  buffer      flat typed buffers over linear memory
  test        assertions, table tests, snapshots, property tests
  ui          everything in this document
  canvas      low-level drawing, for the Canvas widget
  dom         low-level DOM access (web only, escape hatch)
```

`json.decode<T>` derives its decoder at compile time from `T`'s structure. There
is no reflection — this is a compile-time derivation, which is what keeps dead
code elimination sound and therefore keeps binaries small.

---

## 10. Open questions

Genuinely undecided, and worth deciding before implementation rather than during:

1. ~~**Animation.**~~ **Settled: neither.** A transition is a *value in the
   model*, and the design system owns it rather than `std/ui`.

   The host already sends `EVENT_FRAME` with the milliseconds since the last
   one, and keeps sending it *while the model keeps changing* — so an
   application asks for animation by returning a model that differs, and stops
   by returning the one it was given. That needs no new export, no
   `requestAnimationFrame`, and nothing declarative in `Style`.

   `packages/material` is the worked answer. `motion.kite` has the easing
   curves, duration tokens and springs; `interaction.kite` holds one
   `Interaction` value that the application threads through `update`, which
   carries every control's hover, focus, press and ripple between frames.
   Geometry stays a function of style and content — the invariant in §4 — so an
   animated state layer changes colours and never moves anything.

   The reason it is not in `std/ui`: what a hover *looks like*, how long it
   takes and which curve it follows are design-system decisions. An iOS package
   would answer differently against the same layout engine, and a core that had
   already decided would have decided for both.
2. **Incremental view diffing.** Rebuild the whole `Node` tree each frame and
   diff (simple, allocates), or track dependencies and rebuild only dirty
   subtrees (faster, needs a reactivity concept the language currently lacks)?
   Leaning toward rebuild-and-diff for v1, since Kite's allocation is host-GC
   and cheap.
3. **Default field values.** §3 is written with authored defaults and `..`
   because that is what the spec permits: a literal that omits a field without
   `..` is an error ([§5.3](../SPECIFICATION.md#53-struct-literals)). A field
   with a declared constant default — `grow: float = 0.0` — would let a
   literal name only what it means, and §3 would shed most of its constructor
   calls. It is not Go's zero value: the value is authored, visible at one
   declaration, and nothing executes. But it is a literal that no longer lists
   what it sets, and it is a candidate eleventh concept. The widget layer is
   so far the only customer asking; whether one customer justifies a spec
   change is the question.
4. ~~**Event wiring for the widget layer.**~~ **Settled** by
   [proposal 0001](proposals/0001-typed-messages.md), as messages-as-data. A
   node carries `Option<Control<Msg>>`, and a control carries an
   `Option<Msg>` built where the node was built — so `update` matches
   exhaustively over the application's own enum instead of comparing strings,
   and a control whose branch was never written fails to compile.

   The closure alternative (`on_press: fn() -> Msg`) was rejected for the
   reason it was always going to be: two closures have no structural
   equality, the `@derive` walk already refuses function fields, and a handler
   rebuilt each frame never compares equal to last frame's — so `Node` would
   have needed carve-outs from `==`, `Share` and the differ in exactly the
   places the language is uniform. Functions live on the `App` and on an
   `Effect`, and nowhere in the tree.

   What is *not* settled is payload-bearing activation: a slider says a
   number, which is not known when the node is built, so
   `material.slider_of` stays identified and unmeaning and its value is
   recovered from the pointer. That is a proposal of its own, and it is the
   one still entangled with question 2.
5. **Fonts on canvas.** Ship a default font subset (~50 KB) so first paint is
   correct, or always use `document.fonts` and accept a flash of unstyled text?
6. **Native windowing.** `winit` for the native target is the obvious choice, but
   it pulls a substantial dependency tree into what is otherwise a small
   toolchain.
