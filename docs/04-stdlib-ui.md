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

```kite
use std/ui

pub fn app(state: State) -> ui.Node {
    return ui.Box{
        dir:     ui.Col,
        gap:     16.0,
        padding: ui.all(24.0),
        children: [
            ui.Text{
                value: "Tasks",
                style: ui.TextStyle{ size: 28.0, weight: ui.Bold },
            },
            ui.Box{
                dir:      ui.Row,
                gap:      8.0,
                align:    ui.Center,
                children: [
                    ui.Input{
                        value:    state.draft,
                        hint:     "what needs doing?",
                        grow:     1.0,
                        on_change: |text| Msg.DraftChanged(text),
                    },
                    ui.Button{
                        label:   "Add",
                        on_press: || Msg.Add,
                    },
                ],
            },
            ui.Scroll{
                grow:     1.0,
                children: state.tasks.map(task_row),
            },
        ],
    }
}

fn task_row(t: Task) -> ui.Node {
    return ui.Box{
        dir:      ui.Row,
        gap:      12.0,
        align:    ui.Center,
        padding:  ui.xy(12.0, 8.0),
        radius:   8.0,
        bg:       ui.rgb(0xF5, 0xF5, 0xF7),
        children: [
            ui.Checkbox{ checked: t.done, on_toggle: |v| Msg.Toggle(t.id, v) },
            ui.Text{ value: t.title, grow: 1.0 },
            ui.Button{ label: "×", variant: ui.Ghost, on_press: || Msg.Delete(t.id) },
        ],
    }
}
```

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

Note that `Input`, `TextArea`, and `Select` are the widgets a canvas-only design
would force you to reimplement. Under `DomRenderer` they are real elements and
get IME, autofill, and selection for free. Under `CanvasRenderer` they are backed
by a hidden, positioned real element that receives the events — the same
technique Flutter and Google Docs use, and the reason those widgets are in the
standard set rather than left to users.

---

## 4. Layout

A **flexbox subset**, chosen because it is the layout model working web
developers already know, and because its algorithm is well specified and
independently implemented (Taffy, Yoga) — which means it can be validated against
a reference.

```kite
pub struct Box {
    // Direction and flow
    pub dir:     Direction     // Row | Col
    pub wrap:    Wrap          // NoWrap | Wrap
    pub justify: Justify       // Start | Center | End | SpaceBetween | SpaceAround | SpaceEvenly
    pub align:   Align         // Start | Center | End | Stretch | Baseline

    // Child participation
    pub grow:    float         // flex-grow
    pub shrink:  float         // flex-shrink
    pub basis:   ?float        // flex-basis

    // Sizing
    pub width:   ?Size         // Px(f) | Pct(f) | Auto
    pub height:  ?Size
    pub min:     ?Constraints
    pub max:     ?Constraints

    // Spacing
    pub gap:     float
    pub padding: Edges
    pub margin:  Edges

    // Appearance
    pub bg:      ?Color
    pub radius:  float
    pub border:  ?Border
    pub shadow:  ?Shadow
    pub opacity: float
    pub clip:    bool

    pub children: [Node]
}
```

Deliberately excluded from v1: grid, absolute positioning (use `Stack`), floats,
`z-index` beyond `Stack` ordering, and CSS cascade of any kind. There are **no
stylesheets** — style is a value, passed explicitly, and therefore type-checked,
refactorable, and dead-code-eliminable.

### Implementation

Two passes over a flat buffer, not a pointer-chasing tree walk:

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

Pass 1 measures intrinsic sizes bottom-up. Pass 2 resolves flexible lengths
top-down. Subtrees whose inputs are unchanged are skipped via a dirty flag, so a
typical frame re-lays out only the changed branch.

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

The application is a pure function of state, with messages as the only way to
change it. This is the Elm architecture, chosen because it needs no new language
concept — it is a `struct`, an `enum`, a `match`, and a `fn`.

```kite
pub enum Msg {
    DraftChanged(str)
    Add
    Toggle(id: int, done: bool)
    Delete(id: int)
    Loaded(tasks: [Task])
    LoadFailed(err: error)
}

pub fn update(state: State, msg: Msg) -> (State, ui.Effect) {
    return match msg {
        DraftChanged(text) => (State{ ..state, draft: text }, ui.none),

        Add => {
            let t = Task{ id: state.next_id, title: state.draft, done: false }
            (State{
                ..state,
                tasks:   state.tasks.push(t),
                draft:   "",
                next_id: state.next_id + 1,
            }, ui.save(t))
        },

        Toggle(id, done) => (State{
            ..state,
            tasks: state.tasks.map(|t| if t.id == id { Task{ ..t, done: done } } else { t }),
        }, ui.none),

        Delete(id)     => (State{ ..state, tasks: state.tasks.filter(|t| t.id != id) }, ui.none),
        Loaded(tasks)  => (State{ ..state, tasks: tasks, loading: false }, ui.none),
        LoadFailed(e)  => (State{ ..state, error: e.message(), loading: false }, ui.none),
    }
}

pub async fn main() {
    await ui.run(ui.App{
        init:   || (State.empty(), ui.load_tasks()),
        update: update,
        view:   app,
    })
}
```

State is immutable, so it is `Share` ([docs/02 §4](02-concurrency.md#4-share-the-invariant-made-nearly-invisible)),
so it can move across tasks without ceremony. `ui.Effect` is how a message
handler requests I/O without performing it — which keeps `update` pure and
therefore trivially testable:

```kite
let (next, effect) = update(state, Msg.Add)
assert(next.tasks.len() == 1)
assert(next.draft == "")
```

No mock DOM, no test renderer, no async in the test.

---

## 7. Accessibility

Under `DomRenderer` it is largely automatic — widgets emit correct elements and
ARIA attributes, and the browser does the rest.

Under `CanvasRenderer`, Kite maintains a parallel ARIA tree, the same technique
Flutter uses. Where Kite differs is that it fixes Flutter's known failures:

| Flutter Web problem | Kite's approach |
|---|---|
| Accessibility **off by default** behind an invisible button | **Always on.** The semantics tree is built during layout, which already walks every node, so the marginal cost is small. |
| Text fields announce as "edit, blank" — no `<label>` emitted | `Input` requires a `label` field. Omitting it is a compile error, not a runtime warning. |
| Lighthouse reports a perfect score inaccurately | `kite check --a11y` audits the widget tree at build time — contrast ratios, missing labels, focus order, touch target sizes. |
| Semantics drift from visuals | The semantics tree is derived from the *same* scene graph that renders, so it cannot go stale. |

Making `label` a required field on `Input` is the single highest-leverage
decision here. A required field is checked by the compiler on every build, for
every developer, forever — whereas a lint is disabled and a runtime warning is
ignored.

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
3. **Fonts on canvas.** Ship a default font subset (~50 KB) so first paint is
   correct, or always use `document.fonts` and accept a flash of unstyled text?
4. **Native windowing.** `winit` for the native target is the obvious choice, but
   it pulls a substantial dependency tree into what is otherwise a small
   toolchain.
