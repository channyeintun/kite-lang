# Proposal 0001 — typed messages, and the loop that delivers them

> **Superseded, and kept as a record.** Everything this proposal touches —
> `std/ui`, `packages/material`, the two example applications, `docs/04` in its
> old form — was removed when the UI direction changed. See
> [the roadmap](../06-roadmap.md#the-direction-changed-at-phase-16) and
> [docs/04](../04-the-web.md). The links below point at a document that no
> longer exists; they are left as written because a superseded proposal that
> has been quietly edited is no longer a record of anything.
>
> One conclusion outlived the design it was written for: **an event should
> carry a value the program defined, matched exhaustively, rather than a string
> recovered later.** That survives into whatever view layer eventually lands.

**Status:** Superseded — was Draft, §§2, 5 and 9 implemented
**Date:** August 2026
**Resolves:** [docs/04 §10 question 4](../04-stdlib-ui.md#10-open-questions) (event wiring), and delivers the effect shape [docs/04 §6](../04-stdlib-ui.md#6-events-and-state) forward-references
**Unblocks:** question 2 (incremental view diffing)
**Touches:** `std/ui`, `packages/material` (components and `interaction.kite`), `examples/counter.kite`, `examples/todo.kite`, `docs/04`

**Revision:** second draft. The first carried only click, key and focus — too
small a vocabulary to host `packages/material`'s interaction layer, whose
ripple needs the press point and whose hover needs the pointer — and it
deferred effects while docs/04 §6 pointed here for them. Both are repaired.
The first draft also gave the loop custody of focus; §5 takes it back.

---

## Implementation status

*Added after the [review](0001-typed-messages-review.md). This records what is
in the tree, not a third draft.*

**Landed.** The tree half, with the review's corrections:

- `Node<Msg>` carries **one** `Option<Control<Msg>>` rather than four loose
  fields — the review's P1, adopted. `Control` holds `id`, `msg`, `edits`,
  `focused` and `enabled`, so "is this a control" is one question.
- `enabled` is real: a disabled control is laid out and hit-tested but leaves
  focus order and means nothing. `Frame` gained `enabled` to derive that.
- Focus is set by `ui.with_focus(tree, id)` at the root rather than by each
  component, which makes "exactly one control is focused" true by construction
  and keeps logical focus away from the animated focus ring — the trap the
  review names.
- Setters compose: `means`, `editable`, `control` and `disabled` each preserve
  what the others set, so the order of two independent statements does not
  matter.
- `ui.control_of` and `ui.msg_at` derive dispatch by walking the tree (§9), so
  nothing is kept in parallel with it.
- `packages/material` and every example are migrated; `examples/player.kite`
  included, which the review asked for. All six examples paint byte-identically
  to before.
- `ptr.same`, which §4 and §12 both need and which did not exist despite
  [SPECIFICATION.md §5.2](../../SPECIFICATION.md#52-equality) claiming it did.

**Not landed, and why.** Everything that needs the loop: `App`, `run`, `Event`,
`Effect`. The review's first P0 is correct and unresolved — the generated host
recognises an application only by its `init`/`view`/`update` exports, owns the
model and `requestAnimationFrame`, and never drives the task scheduler at all,
so an effect has no pump. That needs an ABI decision, not more `std/ui`.

Nothing here forecloses it. The tree now states everything the loop will need
to read.

**Found on the way.** Three inference gaps, all fixed, none specific to this
proposal: a struct literal's spread base did not settle its type arguments; a
generic call ignored the type it was used as; and inside a generic function,
argument expectations were dropped because a bound parameter of the enclosing
function is indistinguishable from an unsolved one after substitution.

**Still open.** `Msg` is unconstrained, so §12's diffing claim does not hold
yet — the review is right, and the precise gap is `Map`, which is `Share` and
is not equatable.

---

## Summary

Three changes, one sentence each. What `update` is handed becomes a value the
application defined, not a string it parses. What the world sends becomes one
`Event` enum the loop has already interpreted, not four floats and a string.
What `update` returns gains a slice of effects — tasks not yet started — so
asking for I/O is data and performing it is the loop's job.

```kite
pub fn update(model: Model, msg: Msg) -> (Model, [ui.Effect<Msg>])
```

The tree stays plain data — no function ever rides on a node — and everything
below layout is untouched.

---

## 1. The problem

A control today is a string id. The application mints `"task3"` when it builds
the row, `ui.control_at` hands the string back when the row is clicked, and
`update` compares it against its own vocabulary — `if hit == "add"`, and a
`task_id` helper that pattern-matches ninety-nine possible names to recover an
index it knew when it built the node.

That is pure data, and it works, and it has the failure mode string dispatch
always has: nothing is exhaustive. Add a control and forget the branch, and it
lays out perfectly, focuses correctly, and does nothing when pressed. The
compiler that lists every uncovered variant of an `enum` has nothing to say
about an uncovered string.

There is also a lesson already paid for once. `role` exists because
`checkbox:read3:tick` carried two meanings in one string with punctuation to
part them, and the fix was to give identity its own field. Dispatch is the
other half of the same mistake: `role` today names a control *and* stands for
what pressing it means.

And beneath both, every application runs a second layout. `update` calls
`frames_of(model)` — the same tree, laid out again — because a raw `x` and `y`
can only be interpreted against rectangles, and only the frames have them.
The ripple in `interaction.kite` documents the extreme of it: a press point
useful to a ripple is *relative to the control's own top-left*, so the
application re-lays-out the world to subtract two numbers the loop already
knew.

## 2. The tree carries meaning

`Node` takes a type parameter — the application's message enum — and grows
three fields, each one fact a control states about itself:

```kite
pub struct Node<Msg> {
    pub name: str
    pub style: Style
    pub content: Content
    pub children: [Node<Msg>]
    /// Who this control is — focus order, hit-testing, the semantics tree.
    pub role: Option<str>
    /// What activating it says: a value, built when the node was built. The
    /// row for task 3 carries `Msg.Toggle(3)`, constructed by the code that
    /// knew the index rather than parsed back out of a name.
    pub msg: Option<Msg>
    /// The text this control is editing, or nil for not editable. What the
    /// loop needs to turn keystrokes into values — see §6.
    pub edits: Option<str>
    /// Whether this control holds focus, as the model believes. The loop
    /// reads it; it never writes it — see §5.
    pub focused: bool
    pub decor: Decor
}

/// The same node, made a control that says something when activated:
/// `id` is who it is, `msg` is what it means. A meaning without an identity
/// would be unreachable — hit-testing and focus find controls by role — so
/// the two are set together.
pub fn means<Msg>(node: Node<Msg>, id: str, msg: Msg) -> Node<Msg>

/// The same node, made an editable control currently showing `value`.
pub fn editable<Msg>(node: Node<Msg>, id: str, value: str) -> Node<Msg>

/// A control that is a place rather than an action — focusable and
/// hit-testable, saying nothing. What `control` has always made.
pub fn control<Msg>(node: Node<Msg>, id: str) -> Node<Msg>
```

The rule that makes this safe is the design's centre:

> **The tree holds values. Functions live only on the `App`.**

Things in the tree are compared, diffed, and shared — and a function value
survives none of that: two closures have no structural equality, the `@derive`
walk already refuses function fields, and a handler rebuilt each frame never
compares equal to last frame's. A `Msg` value is an enum — structural `==`,
`Share`, printable in a test failure. So the tree carries values only, and the
places a function is genuinely needed — translating an event, starting an
effect — are fields on structs the loop consumes and nothing ever compares:
`App` and `Effect`.

## 3. Events: one door, typed

The host sends eight raw events — down, up, move, click, key, wheel, frame,
resize — as an integer and three payload slots. Today every application
decodes them by hand. Under this proposal the loop decodes them once, into an
enum, with the hit-testing and focus bookkeeping already done:

```kite
pub enum Event {
    /// A focus transition the loop proposes: Tab reached this control, or a
    /// click landed on it. The model decides whether to believe it — §5.
    Focused(Option<str>)
    /// An editable control's text changed. Every renderer speaks this — §6 —
    /// so an application never interprets a keystroke in order to edit.
    Edited(id: str, value: str)
    /// A key that was not traversal, not activation, and not editing: the
    /// focused control if any, and the key itself. Arrows arrive here — what
    /// they mean belongs to the application (§5).
    Key(focused: Option<str>, key: str)
    /// The pointer moved onto a control, or off onto nothing.
    Hovered(Option<str>)
    /// The pointer went down on a control, at a point in *its* space.
    Pressed(id: str, x: float, y: float)
    /// The pointer moved. Over a control, the point is in that control's
    /// space; while a control is armed, in the armed control's space wherever
    /// the pointer has gone; over nothing, in the window's.
    Moved(over: Option<str>, x: float, y: float)
    /// The pointer came up, over whatever it was over.
    Released(over: Option<str>)
    /// Scroll intent: the control under the pointer, and the deltas.
    Wheel(over: Option<str>, dx: float, dy: float)
    /// Milliseconds since the last frame. Sent while the model keeps
    /// changing, which is how an application asks for the next one — §4.
    Frame(float)
    /// The viewport, delivered before the first frame and on every change,
    /// so a breakpoint is in the model before anything is laid out.
    Resized(Size)
}
```

**Positions arrive in the space of the control they name.** The loop has the
frames; a coordinate handed over pre-translated is one the application never
needs the frames to interpret. This retires `frames_of` from application code
entirely — the second layout that today runs inside `update` — and it is
exactly what the ripple asks for: `Ripple.x` is documented as *"relative to
the control's top-left rather than to the window"*, and today the package
earns that by re-laying-out and subtracting. Under this proposal it is simply
what `Pressed` says.

**While a control is armed, it keeps its coordinates.** From `Pressed` until
`Released`, `Moved` stays in the armed control's space wherever the pointer
goes — negative and beyond-the-edge values included. That is pointer capture,
stated: a slider dragged past its own end keeps computing, and Material's
slide-off-to-cancel can watch the pointer leave.

**Activation is the loop's, and its rules are short.** A control's `msg`
fires on `Released` over the same control that was pressed — sliding off
cancels. `Enter` fires the focused control's `msg`. Space does too, unless
the control is editable, because an editable control types its space. A key
consumed by none of traversal, activation, or editing arrives as `Key`.

The application receives all of this through one field:

```kite
/// The world, translated into the application's vocabulary — or declined.
pub on_event: fn(Event) -> Option<Msg>
```

One field, not one per kind. A field per kind was the first draft's shape,
and it fails on arrival of the ninth kind: a new required field in every
application ever written. An enum arm is the todo example's own comment
upgraded — *a new kind of event is a new constant, not a new export* — with
the constant now a variant, and a program that ignores a kind writing the arm
that says so: `_ => nil`.

## 4. The loop moves into `std`

```kite
pub struct App<Model, Msg> {
    pub init: fn() -> (Model, [Effect<Msg>])
    pub view: fn(Model) -> Node<Msg>
    pub update: fn(Model, Msg) -> (Model, [Effect<Msg>])
    pub on_event: fn(Event) -> Option<Msg>
}

pub fn run<Model, Msg>(app: App<Model, Msg>)
```

Every field is required — Kite has no zero values, and that is right here
too: an application with no starting I/O writes `[]`, which is a statement,
not an omission.

`run` owns what every application currently hand-writes: it lays out `view`'s
tree at the real viewport, paints, hit-tests, translates coordinates,
synthesises `Edited`, proposes focus, fires activations, starts effects and
feeds their results back through `update`. The raw door does not go away — it
is the layer `run` is built on, it stays public, and the differential test
suite keeps driving it directly. `run` is the door with ten years of
convention written for you.

**Frames keep the settled semantics.** Docs/04 §10.1's rule — the host sends
`Frame` while the model keeps changing — becomes `run`'s rule: it requests
another frame while `update` returns a model that is not `ptr.same` as the
one it was given. Returning the model unchanged is how an application says
"nothing is moving", exactly as `interaction.kite`'s `advance` already does,
and it is what keeps a settled screen from warming a laptop.

## 5. Focus: the model owns it, the loop proposes

The first draft gave `run` custody of focus and the model a mirror. That
breaks the moment an application needs to *direct* focus — returning it to
the draft field after Add, trapping it in a dialog — because a mirror has no
way to talk back.

So custody stays where it is today: **focus lives in the model, and the tree
declares it** — a component sets `focused` on its node from the same
`when_focused` state it already renders a ring from. The loop reads the tree
to know where focus is; it never decides.

What the loop does is *propose*. `Tab` and `Shift+Tab` walk `focus_order` —
derived from the frames, as always — and arrive as `Event.Focused(id)`. A
click on a control proposes the same way. The application accepts by writing
the id into its model, and the next tree says so; an application that
declines has refused the focus change, which is what a modal wants and what
no mirror could express.

Arrows are deliberately not traversal. They arrive as `Key`, because their
meaning belongs to the control: a focused slider adjusts, a list navigates,
and an application that wants arrow-traversal writes `ui.next_focus` into its
own `update` — it owns the field; it can.

## 6. Editing: the loop speaks values, on every renderer

The line the first draft drew — text stays on the raw path — baked in a
canvas assumption. Under `DomRenderer`, the whole argument of docs/04 is that
an `Input` is a real element: the browser edits it, and what comes back is
the *value*, not keystrokes. A channel shaped like keys would be
unimplementable as specified on the renderer the design exists to keep.

So the channel is shaped like values. An editable control declares what it
currently shows — `edits` on the node, set by the field component from the
model — and every renderer produces the same event:

- **DOM:** the element edits itself; an input event becomes
  `Edited(id, value)`.
- **Canvas:** the loop applies the keystroke to the node's `edits` —
  `ui.typed`, which already exists — and the result becomes
  `Edited(id, value)`. The hidden IME overlay of docs/04 §5 lands in the same
  variant, which is what makes composition input identical to typing.

The application handles one variant, mirrors the value into its model, and
the next frame's tree shows it. The edit buffer never lives in the loop: the
tree carries the current value down, the event carries the next value up, and
the model remains the only thing that remembers.

## 7. Effects: unstarted tasks

`std/task`'s own header states the constraint: *calling an `async fn` starts
it* and yields the running `Task`. An effect built by calling would therefore
be I/O performed inside `update` — a fetch fired by a unit test that only
wanted to inspect a model. So an effect is a task **not yet started**: a
closure the loop calls, never the application.

```kite
pub struct Effect<Msg> {
    /// Called by the loop, after update has returned. Calling is starting,
    /// so an effect that arrived already called would be an update that had
    /// already performed its I/O.
    pub start: fn() -> Task<Msg>
}

pub fn effect<Msg>(start: fn() -> Task<Msg>) -> Effect<Msg> {
    return Effect{ start: start }
}
```

An effect's reply is a message — which is why docs/04 §6 said effects arrive
with this decision. The application wraps its I/O in an `async fn` that
returns `Msg`, and the error handling happens where the vocabulary is:

```kite
async fn fetch() -> Msg {
    let (tasks, err) = await store.load()
    if err != nil {
        return Msg.LoadFailed(err.message())
    }
    return Msg.Loaded(tasks)
}

// in update:
Refresh => (model, [ui.effect(|| fetch())])
```

The loop starts each returned effect, awaits it, and feeds the resulting
message back through `update`. A test calls `update`, inspects the model, and
the effects it was handed have done nothing — starting them is the test's
choice, and an integration test that does can `await` the task like any
other.

Two boundaries, stated so they are not discovered: **animation is not an
effect** — `Frame` is an event, and the fixpoint rule in §4 is the whole
mechanism, unchanged from docs/04 §10.1. And **there is no cancellation** in
this proposal: an effect whose answer stopped mattering delivers a message
the model ignores, and real cancellation — with subscriptions to things that
push, like sockets — is a sequel with its own design burden.

## 8. What an application looks like

`examples/todo.kite`, whole in shape if not in line count:

```kite
pub enum Msg {
    Focused(Option<str>)
    Drafted(str)
    Add
    Toggle(int)
    Loaded([Task])
}

fn view_node(model: Model) -> ui.Node<Msg> {
    let s = material.dark()
    var rows: [ui.Node<Msg>] = []
    for i in 0..model.tasks.len() {
        let task = model.tasks[i]
        rows.push(material.checkbox(
            s,
            "task\(i)",
            Msg.Toggle(i),
            task.title,
            task.done,
            material.when_focused(model.focused == "task\(i)"),
        ))
    }
    // Header with the draft field and the Add button, list, status — as in
    // docs/04 §3, unchanged but for the message each control now carries.
    return assembled(s, model, rows)
}

fn start() -> (Model, [ui.Effect<Msg>]) {
    return (blank(), [ui.effect(|| fetch())])
}

pub fn update(model: Model, msg: Msg) -> (Model, [ui.Effect<Msg>]) {
    return match msg {
        Focused(id) => (Model{..model, focused: or_else(id, "") }, []),
        Drafted(text) => (Model{..model, draft: text }, []),
        Add => (added(model), []),
        Toggle(at) => (toggled(model, at), []),
        Loaded(tasks) => (Model{..model, tasks: tasks }, []),
    }
}

fn translate(e: ui.Event) -> Option<Msg> {
    return match e {
        Focused(id) => Msg.Focused(id),
        Edited(id, value) => if id == "draft" { Msg.Drafted(value) } else { nil },
        _ => nil,
    }
}

fn main() {
    ui.run(ui.App{
        init: start,
        view: view_node,
        update: update,
        on_event: translate,
    })
}
```

What is gone from today's file: `EVENT_CLICK` and its siblings, `clicked`,
`pressed`, `task_id`, the hand-rolled Tab handling, and `frames_of` — the
second layout. What is gained: `match msg` is exhaustive, so adding a variant
makes the compiler name every place that must change, which is the safety
`enum` was brought into the language to provide, now covering the UI's
wiring. Enum variant constructors are not first-class values, so `translate`
writes `Msg.Focused(id)` out by hand — one arm per borrowed variant, and it
reads as what it does.

## 9. Below the application boundary, nothing moves

- **`layout` becomes generic** — `layout<Msg>(root: Node<Msg>, viewport)` —
  but reads none of `msg`, `edits`, or `focused`, so every instantiation's
  body is byte-identical and identical-code-folding merges them.
- **`Frame` stays exactly as it is.** Frames are the paint layer's flat,
  non-generic output; renderers, `paint`, golden tests and the scroll helpers
  are untouched. The three new facts live on the tree, and the loop reads
  them from the tree.
- **Dispatch is a derived table.** Each frame, `run` walks the tree once and
  keeps its private `role → (msg, edits, focused)` map, the way `focus_order`
  is derived from frames. Nothing is hand-kept in parallel; derived things
  cannot drift.

## 10. Rejected alternatives

- **Closures on nodes** (`on_press: fn() -> Msg`). Reads best at the call
  site; costs carve-outs in `==`, `Share`, `@derive` and the differ — special
  cases in exactly the places the language is uniform. Revisitable only
  behind a handler-equality rule, in a proposal of its own.
- **A field per event kind on `App`** — the first draft's own shape. Fails on
  the ninth kind: a new required field in every application ever written,
  versus a new variant and the arms that already say `_ => nil`.
- **Started-task effects** (`[Task<Msg>]`). One closure shorter and no longer
  pure: building the slice performs the I/O, and every test of `update`
  becomes an integration test. The thunk is the entire difference between
  *describing* an effect and *having* one.
- **Inert command data, à la Elm's `Cmd`.** Purity without functions would
  need every async operation reified as a value the loop interprets — an
  instruction vocabulary for I/O, growing forever. The thunk is one closure
  and no vocabulary.
- **An application-authored `{str: Msg}` table beside the tree.** Keeps
  `Node` non-generic, but it is a second tree-shaped thing maintained by
  hand, and the reader demo already showed how parallel structures rot.
  Derived internally instead (§9).
- **The status quo.** Zero machinery, dispatch stays convention: no
  exhaustiveness, meaning parsed back out of names, and a second layout in
  every `update`. It remains available as the raw layer, which is where it is
  honest — a door, not a vocabulary.

## 11. Costs, plainly

- Every component signature gains `<Msg>`, and `packages/material` changes
  shape twice over: components take a `msg` parameter, and `interaction.kite`
  trades its `(frames, x, y)` entry points for event-shaped ones. This is the
  migration's bulk; all of it is mechanical.
- One monomorphised instantiation of the node constructors per application
  message type — in practice one per application, and the bodies that never
  touch the new fields fold.
- Messages are built eagerly, for every control, every frame: a few small
  enum allocations on a host GC, the same cost profile the
  rebuild-the-tree architecture already accepted.
- Every `update` arm writes its effect slice, `[]` almost always. Verbosity
  in exchange for a visible channel — the same trade `check err` makes on
  its own line.
- `_ => nil` in `on_event` swallows event kinds added later, silently. That
  is the catch-all's price everywhere in the language; an application that
  wants loud additions lists every arm and gets exhaustiveness instead.
- `Node` grew three fields, and a tree with no controls still names a
  message type. Any enum serves; the parameter is doing no work in that
  program, which is the honest description of that program.

## 12. What this unblocks

**Question 2 (diffing) falls open.** With functions banned from the tree,
`Node<Msg>` has structural equality end to end, so rebuild-and-diff can
compare honestly, and the `Html.Lazy` escape hatch — a node carrying a model
slice, compared with `ptr.same` before its subtree is rebuilt — is
expressible with what the stdlib already has. List keys belong to that
proposal, not this one.

**Question 3 (default field values) gets its evidence.** The migrated
material package and the two examples are the corpus: if the
one-`defaults()`-per-struct convention holds under a component set that is
now generic, the spec keeps its ten concepts; if a second setter layer grows,
that is the finding, recorded where §10.3 asks for it.

## 13. Out of scope, named

- **Payload messages in the tree** (`fn(str) -> Msg` on nodes) — blocked on a
  handler-equality rule; nothing here forecloses it, nothing here needs it.
- **Subscriptions** — sockets and anything else that pushes; arrives with
  effect cancellation as its own proposal.
- **Effect cancellation** — see §7.
- **The DOM renderer's internals** — how elements are created, pooled and
  synced is renderer work; this proposal only fixes the events it must emit.

## 14. Migration

1. `std/ui`: add `msg`, `edits`, `focused` to `Node`; add `means`,
   `editable`, `Event`, `App`, `Effect`, `effect`, and `run` beside the
   existing free functions. Existing signatures change only by the type
   parameter.
2. `packages/material`: components take `msg` after `id`; fields set `edits`
   and `focused` from the state they already hold; `interaction.kite`
   consumes `Event`s and loses every `frames` parameter — the coordinates
   arrive already interpreted.
3. `examples/counter.kite`, `examples/todo.kite`: rewritten on `run`. The raw
   door versions move into the differential test suite, which is where the
   raw layer earns its keep.
4. `docs/04`: §3 and §6 examples updated; §10 question 4 marked settled with
   a pointer here, in the same strikethrough style as question 1.
