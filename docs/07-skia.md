# What Kite takes from Skia, and what it does not

Kite's drawing boundary was designed before anyone read Skia. This note is the
reading, and the argument for the two calls it added and the four it did not.

The reading is of Skia's public headers on `main`: `SkColor.h`, `SkPaint.h`,
`SkCanvas.h`. Where this quotes Skia, it quotes those.

## The division Skia draws, and the one Kite already had

`SkPaint`'s class comment states the split outright:

> SkPaint controls options applied when drawing. SkPaint collects all options
> outside of the SkCanvas clip and SkCanvas matrix.

That is the same line Kite arrived at independently and for the same reason.
`std/ui`'s `Decor` is a `SkPaint`: fill, radius, border, ink, centring — all
appearance, none of it geometry. The clip is canvas state, as it is in Skia.
The layout never reads `Decor`, which is asserted by a test that lays out a
decorated tree and the same tree stripped bare and compares every frame.

The recorded call list in the generated glue is an `SkPicture`: a flat
sequence of draw calls, replayable into any renderer. Kite's version exists to
diff two frames and repaint only what changed, which Skia does not do; the
shape is the same.

So the parts of Skia that are about *organisation* were already here. What was
missing was primitives.

## Taken: the ring between two rounded rectangles

`SkCanvas::drawDRRect(outer, inner, paint)`:

> Draws SkRRect outer and inner using clip, SkMatrix, and SkPaint paint. outer
> must contain inner or the drawing is undefined.

Skia has a dedicated call for the region between two rounded rectangles,
described in its own documentation as useful for stroked rounded rectangles.
That is not a convenience. It is there because stroking a rounded rectangle is
common enough to deserve a primitive that does not go through the general path
stroker.

Kite had no such call, and the absence had spread. A border was drawn as *two
filled rectangles* — the box in the edge colour, then the box inset by the
border width drawn back over it. The inner fill is standing in for a hole that
cannot be punched, so it has to be painted in the colour of whatever is behind
the box. Which means the painter has to know that colour. Which means it
carried a **backdrop stack** down the tree, one entry per depth, resolving
"transparent" to the nearest enclosing fill — and that was correct only while
boxes nest visually, which the code said in a comment and could not enforce.

`draw.drrect(x, y, w, h, radius, width, colour)` draws the ring and nothing
else. The backdrop stack is deleted. `ui.paint` no longer takes the window's
colour, because nobody needs to name it: the background of a window is the
fill of the node at the root of it, like any other box.

Kite's version is narrower than Skia's. Skia takes two arbitrary rounded
rectangles; this takes a uniform inset, because a border is what the region
between two rrects is used for here and an arbitrary pair has no caller. If
one appears, widening this is a compatible change.

Both renderers already had it. A DOM renderer sets `border` and
`border-radius` with `box-sizing: border-box`. A canvas renderer builds two
subpaths and fills with the even-odd rule, so the inside is a hole rather than
a second fill.

## Taken: alpha, as a separate channel

`SkColor` is `uint32_t`, packed `(a << 24) | (r << 16) | (g << 8) | b`,
unpremultiplied. `SkPaint` *also* carries alpha separately, through
`setAlphaf`/`getAlphaf`, which overwrite the colour's alpha byte.

Kite took the second of the two, deliberately.

Packing alpha into the colour is the obvious move and is a trap. Every colour
in every Kite program is written `0xRRGGBB`. Reinterpreting that argument as
`0xAARRGGBB` gives all of them an alpha of zero, and a program that used to
draw would compile, run, and show nothing — the worst failure mode available,
because nothing reports it. Skia has the same hazard and answers it by making
the caller say opaque: `SkColorSetRGB` sets alpha to `0xFF`. That answer is not
open to us, because the argument already exists with the other meaning.

So `draw.alpha(a)` is state, like the font and the clip, and `Decor` gained an
`alpha` field beside `fill` rather than inside it. Every existing call is
unchanged and still correct.

What this buys, beyond translucency: Material's state layers are specified as
an on-colour at 8–12% opacity over a container. Today `packages/material`
*pre-computes* them by blending two known ints, which is exact only because
what they cover is a known flat colour — true over a token, false over an
image. With real alpha they can be drawn. Scrims and disabled states are the
same. Elevation shadows become expressible as concentric low-alpha rings,
though nothing draws one yet.

## Declined: paths

`SkPath` is how Skia draws anything that is not a rectangle, and it is the
single biggest thing Kite lacks. A checkmark is a glyph here, not a shape.

It is declined because a DOM renderer cannot honour it. Every other call at
this boundary maps to a CSS property on a positioned `div`; a path maps to an
inline SVG, which is a different rendering model with its own coordinate space,
its own stacking rules, and its own text handling. The value the boundary buys
— two renderers that *cannot* disagree, because neither decides anything — is
worth more than arbitrary geometry, and a path is exactly where the DOM
renderer would start deciding.

This is a real limit and it should be stated as one rather than worked around
quietly. It means icons are glyphs, a slider's thumb is a rounded rectangle,
and a chart is not drawable. If that becomes intolerable, the honest move is a
canvas-only renderer with a documented reduced fidelity on the DOM path — not
a path primitive that one renderer approximates.

## Declined: stroke style with caps and joins

`SkPaint::Style` is `kFill`, `kStroke`, `kStrokeAndFill`, with a width, a miter
limit, four cap styles and three join styles.

Almost all of that exists to stroke *paths*, and there are no paths here. The
only stroking this codebase does is the outline of a rounded rectangle, and
`draw.drrect` does that exactly — with no caps, because a ring has no ends, and
no joins, because the corners are arcs. Adding a stroke style would be adding
the vocabulary without the thing it describes.

## Declined: the matrix stack

`SkCanvas` carries a full matrix — `translate`, `scale`, `rotate`, `skew`,
`concat`, `setMatrix` — and `save`/`restore` push and pop it along with the
clip.

Nothing in Kite's layout produces a rotation, a scale, or a skew. `ui.layout`
computes axis-aligned rectangles in device pixels, which is the whole of what a
box layout does. A matrix stack would be a mechanism with no client, and its
cost is not zero: every hit test would have to invert it, and the two renderers
would have to agree about composition order.

`save`/`restore` is a smaller question. Kite has `draw.clip`/`draw.unclip`,
which is a stack of exactly one thing with no depth counter, and the canvas
renderer already tracks its own depth to refuse an unbalanced `unclip`. If
nested clips are needed — a scrolling region inside a scrolling region —
that is the moment to take Skia's save/restore properly rather than deepening
an ad-hoc pair.

## Declined: shaders, blend modes, colour filters, image filters

`SkPaint` carries `fShader`, `fBlender`, `fColorFilter`, `fMaskFilter`,
`fImageFilter`. A gradient is the only one with an obvious caller here, and
Material 3's baseline scheme is flat colour throughout. Blend modes beyond
source-over have no use in an opaque box layout.

## What is still wrong

- The `Decor.alpha` channel applies to a whole box, fill and border and text
  together, where `SkPaint` would let each draw carry its own. Splitting it is
  easy when something needs it.
- `packages/material` still pre-computes its state layers. Alpha makes drawing
  them possible; converting the components is a separate change.
- There is no shadow. Tonal elevation — Material's own answer for dark themes,
  and since 2023 the specified one — is what the package does, and it is
  correct rather than an approximation. A light-theme card still has no drop
  shadow.
