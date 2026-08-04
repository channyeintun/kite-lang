# Review of Proposal 0001 — typed messages, and the loop that delivers them

**Reviewed:** August 2026

**Recommendation:** Request changes; keep the direction and produce a third
draft.

## Overall assessment

The proposal's central choice is sound: activation should ride on a node as a
`Msg` value, while functions stay at the application and effect boundaries.
That removes the worst string parsing from `update`, preserves a structurally
comparable tree for ordinary message types, and gives effects an explicit,
unstarted representation.

The proposed public surface is not implementable against Kite's current
application ABI, however, and it removes capabilities that the Material package
already uses. Several event guarantees also cannot be produced from the raw
events described in the proposal. These are contract gaps rather than migration
chores, so they should be settled before implementation.

Severity in this review means:

- **P0:** the stated API cannot be implemented or cannot carry an in-scope
  package.
- **P1:** the API is implementable only after choosing currently unspecified
  observable behavior.
- **P2:** the design can land, but a claim or migration boundary needs
  correction.

## Findings

### P0 — `ui.run` cannot own the current host loop as an ordinary `std` function

[Section 4](0001-typed-messages.md#4-the-loop-moves-into-std) makes
`ui.run(App)` the application entry point and says the loop moves into `std`.
Today the generated host does the inversion of control:

- it recognizes an application only when the module exports `init`, `view`, and
  `update`;
- JavaScript owns the model reference and calls those exports for input and
  frames; and
- it owns renderer switching and `requestAnimationFrame`; task driving likewise
  lives in generated host glue rather than in `std`.

See
[`isApplication`](../../crates/kite-codegen-wasm/src/glue.rs#L1596) and the
[page loop](../../crates/kite-codegen-wasm/src/glue.rs#L2235). A program whose
`main` calls `ui.run` does not export the required `init/view/update` ABI; the
host classifies it as non-interactive and calls `main` again whenever it draws.
`std/ui` also has no inbound event stream or mutable global in which to retain
the `App` and model. Passing the `App` callbacks or model to JavaScript is not
an escape hatch: Kite's declared host boundary accepts only scalar host types
([`check_host_type`](../../crates/kite-types/src/lib.rs#L524)).

Effects make the missing bridge larger. A completed `Task<Msg>` must wake the
UI, enqueue its message, and repaint even when no pointer or frame event is in
flight. The present application page does not drive that lifecycle on behalf of
a function hidden inside Wasm.

The third draft needs to choose an ABI, not merely assign this work to `run`.
Viable directions include:

1. retain exported application wrappers and make `std/ui` expose a pure
   `init/step/render` kernel those wrappers call;
2. make `App` a compiler-recognized entry point and specify the exports the
   compiler synthesizes; or
3. add a host-backed event stream, make `run` an async task, and let the Wasm
   scheduler own the model loop.

Whichever direction wins must specify Web, bytecode, and native behavior,
including what `kitec run` does when no window event source exists. It also
belongs in the migration list; this cannot land through `std/ui` changes alone.

### P0 — `App.view -> Node` removes Material's post-layout rendering path

The proposed `App` lets the application return a tree, after which `run` lays it
out and paints it. There is no hook between layout and paint and no overlay
primitive in `Node`.

Material already needs exactly that hook. Its ripple is painted after the base
tree, using the laid-out control frame for position, shape, and clipping
([`material.paint`](../../packages/material/interaction.kite#L651)).
`examples/player.kite` calls that path for both ordinary and scrolled content.
A ripple cannot currently be represented as a child node: layout has no
absolute/stack child, and adding it as a normal child changes geometry.
`paint_scrolled` is another post-layout operation that a node-only `view`
cannot request.

This contradicts both the claim that Material migrates mechanically and the
claim that everything below the application boundary remains unchanged.
Choose one of these before accepting the `App` shape:

- add an application/package paint hook that receives the model and laid-out
  frames, with `ui.paint` as the normal implementation;
- add compositional overlay, clipping, and scrolling nodes to the returned
  scene; or
- narrow this proposal so `run` does not yet replace applications that need
  Material interaction or scrolling.

The first option is the smallest change and follows the proposal's own rule:
the hook is a function on `App`, never a value on `Node`.

### P1 — local coordinates are not enough to remove frame access

[Section 3](0001-typed-messages.md#3-events-one-door-typed) says translated
positions retire `frames_of` from application code entirely, and the migration
says Material's interaction entry points lose every frames parameter. The
existing package supplies two counterexamples:

- A ripple needs the control's ink as well as the local press point.
  `pressed_down` currently gets that from the control frame
  ([`with_ripple`](../../packages/material/interaction.kite#L577)).
  `Pressed(id, x, y)` supplies the already-translated point but not the ink.
- A slider needs its laid-out width to turn a local `x` into a fraction.
  [`slider_value_at`](../../packages/material/selection.kite#L390) uses the
  frame width, and the responsive seek bar in
  [`examples/player.kite`](../../examples/player.kite#L1231) is the worked
  application case. A local `x` without the target size is insufficient.

At minimum, pointer events need a target value containing stable identity,
local position, and laid-out size. Material's visual data needs a separate
answer: retain frame access in a package event/paint hook, put the required
visual facts in package-authored control metadata, or revise the ripple design.
Do not claim the frames parameters are mechanically removable until the
Material and player call sites have been sketched end to end.

### P1 — the raw-to-typed event contract loses distinctions the typed contract requires

Four observable cases need a precise translation rule.

First, the current host reports `pointercancel` as `EVENT_UP` and deliberately
omits the later click
([glue](../../crates/kite-codegen-wasm/src/glue.rs#L2446)). If activation fires
on `Released` as proposed, a cancelled gesture can activate a control. If the
loop instead uses raw `CLICK` as the commit signal, say so and define how it
retains the armed control across the preceding `Released`.

Second, `Key` carries only a key string. The host sends `e.key` and discards
`shiftKey`, so the promised `Shift+Tab` reverse traversal cannot be
distinguished from `Tab`
([keyboard input](../../crates/kite-codegen-wasm/src/glue.rs#L2496)). A modifier
value or an explicit traversal event is required.

Third, the advertised eight raw events have no DOM value-edit event. A native
`input` event can occur without a useful key and must carry the resulting full
value. `Edited(id, value)` therefore needs a ninth raw event or a specified
renderer-to-loop channel.

Fourth, one raw gesture can produce several application messages: a focus
proposal, `Pressed`/`Released`, and the node's activation `Msg`; a move can
produce both `Hovered` and `Moved`. Specify:

- their order;
- whether `view` and layout are recomputed between messages;
- which tree supplies the attached `Msg`;
- when effects returned by each update start; and
- whether all messages from one raw event are processed before another raw
  event may enter.

Without that transaction rule, a focus update can change or remove the control
before its activation is looked up, and two correct loop implementations can
produce different models.

### P1 — control metadata permits ambiguous and invalid states

The private `role -> (msg, edits, focused)` table assumes roles are unique, but
the proposal never requires uniqueness or defines duplicate behavior. A
duplicate role can make hit-testing select one node and message lookup select
another. The same ambiguity affects focus order.

The four independent public fields also permit states the loop cannot interpret
reliably:

- `msg` or `focused` without a role;
- `edits` without a role;
- more than one node with `focused: true`;
- a visually disabled Material control that still has a message and remains in
  focus order.

Disabled controls are already part of `material.State`, so this is not a future
widget concern. Direct loop activation must know whether a control is enabled;
the application can no longer compensate with string dispatch after the loop
has fired its message.

Prefer one atomic control field, for example
`Option<Control<Msg>>`, containing identity, enabled/focusable state, logical
focus, edit state, and optional activation. Keep its fields private if
constructors are intended to enforce the invariants. Require unique identities
per rendered tree and define the diagnostic or deterministic failure used when
the requirement is violated.

Logical focus must also be separate from animated focus paint. Material's
focus-out track can remain visible while a new control's focus-in track starts;
using the visual state for `Node.focused` temporarily marks both controls.

### P1 — effect execution and message serialization are underspecified

[Section 7](0001-typed-messages.md#7-effects-unstarted-tasks) says the loop
"starts each returned effect, awaits it, and feeds" its message through
`update`. That does not say whether effects in one slice are concurrent or
serial, nor what happens when input and effect completions arrive together.

Specify a single-threaded message queue and at least these rules:

- whether every effect in a returned slice is started before any is awaited;
- whether completion order, declaration order, or another order determines
  message delivery;
- that `update` is never re-entered;
- when a result-triggered model change repaints and wakes animation;
- how effects returned by an effect-result update are scheduled; and
- what happens to outstanding effects when the application exits.

Cancellation can remain out of scope. Ordering and wake-up behavior cannot:
they are observable even in the first version and are required to implement
the loop/host bridge from the first finding.

### P1 — `Node<Msg>` is not structurally comparable for every legal `Msg`

The proposal says a `Msg` is structurally equal and `Share`, then uses that to
claim `Node<Msg>` is structurally comparable end to end. Kite does not enforce
either property merely because a type is named `Msg`: an enum payload can
contain a function, mutable aggregate, trait object, or host-backed value.
Function-containing aggregates are explicitly non-equatable in the compiler
([type tests](../../crates/kite-hir/src/ty.rs#L1048)).

That lets an application smuggle the exact non-comparable handler state rejected
for nodes inside a message payload, defeating the stated reason for this
design and re-blocking diffing.

Add an enforceable constraint to `Node`, `App`, or `run`. `Msg: Share` may be
the existing spelling closest to the intended value discipline; if structural
comparability needs a distinct bound, the proposal must name the compiler work
because `Eq` is not currently a user-written generic bound. Also soften
"printable in a test failure" unless `Debug` is required.

### P2 — the migration and validation corpus is too narrow

Generic `Node` changes every node constructor, Material component, example, and
package test, not only counter and todo. More importantly,
`examples/player.kite` is the repository's existing evidence for responsive
layout, scrolling, slider geometry, Material interaction, ripples, resize,
wheel input, and animation. Leaving it outside the migration hides most of the
contract gaps above.

The migration should include the generated Web glue, scheduler integration,
`examples/player.kite`, Material tests, UI golden transcripts, and a raw-loop
differential harness. The guarantee that `Resized` arrives before first layout
also needs a host change: the current page draws in `show("dom")` before calling
`measured()`.

## What should remain in the next draft

The following choices survived the review and should be retained:

- activation messages are values on controls, not closures;
- identity and activation meaning remain separate;
- raw events remain available as a lower-level testable layer;
- pointer coordinates are translated once by the owner of layout;
- focus is application-directed rather than hidden mutable loop state;
- effects are unstarted thunks, with cancellation explicitly deferred; and
- animation remains event/model driven rather than becoming an effect.

The scope should be described more narrowly as **typed activation plus a typed
application update boundary**. Payload-bearing controls still route through
`Event` and application/package translation, so event wiring is not yet typed
end to end. That is a reasonable first boundary as long as the draft says so.

## Minimum acceptance checklist for a third draft

1. Select and document the application ABI that makes `run` reachable from host
   events and effect completions.
2. Preserve a post-layout path for Material ripples and scrolling, or add scene
   primitives that replace it.
3. Carry target size (and settle Material's required visual metadata) in the
   event/package contract.
4. Distinguish release from cancellation, carry keyboard modifiers, and add a
   raw DOM value-edit channel.
5. Define the ordered message transaction for focus, pointer events, activation,
   and effects.
6. Enforce unique control identity, enabled/focusable state, exactly one logical
   focus, and valid edit/activation combinations.
7. Define concurrent effect start, serialized completion delivery, and wake-up
   behavior.
8. Constrain `Msg` so the tree retains the equality/share properties used to
   justify the design.
9. Migrate and test `examples/player.kite`, not only counter and todo.

With those changes, the proposal's core can be accepted without revisiting the
decision to keep functions off the tree.
