//! JavaScript glue generation.
//!
//! There is no standardised way for Wasm to call a Web API without JavaScript
//! glue — no Web IDL bindings proposal has landed, and none is imminent. Kite
//! therefore declares the host boundary explicitly and *generates* the glue
//! from that declaration, so the two cannot drift apart.
//!
//! The generated module is deliberately tiny and has no dependencies.

/// The JavaScript module that instantiates a compiled Kite program.
/// Strip the commentary from generated output.
///
/// The template below is heavily commented, and those comments are for whoever
/// maintains *this file* — they explain why the glue is shaped the way it is.
/// They are not for the person who ran `kitec build`: that person gets an
/// artifact, and an artifact carrying three hundred lines of someone else's
/// reasoning is an artifact that reads as though it were written by hand and
/// meant to be edited. It is neither. The banner at the top says so, and the
/// rest is deleted on the way out.
///
/// Line-based on purpose. A line is dropped only when it is *entirely* a
/// comment, so nothing inside a string can be touched by accident — and a
/// check confirms no template literal in this file begins a line with `//`.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_block = false;
    let mut blanks = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if in_block {
            if trimmed.contains("*/") {
                in_block = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            in_block = true;
            continue;
        }
        if trimmed.starts_with("//") || (trimmed.starts_with("/*") && trimmed.ends_with("*/")) {
            continue;
        }
        if trimmed.is_empty() {
            blanks += 1;
            // One blank line between sections, never a gap where a paragraph
            // of commentary used to be.
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn generate_glue(wasm_path: &str) -> String {
    generate_glue_with_hosts(wasm_path, &[])
}

/// The glue, including the host groups a program declared.
///
/// Every `@host("group") extern fn` becomes an entry the host must supply.
/// The generated module declares each group as an object a page can fill in
/// with `provide("group", { … })` before running — and a call to something
/// nobody supplied fails saying which declaration it was, rather than doing
/// nothing.
pub fn generate_glue_with_hosts(wasm_path: &str, hosts: &[crate::HostImport]) -> String {
    let strings_section = String::from(
        r#"// Kite owns strings as WasmGC arrays of Unicode scalar values. This page is
// only a synchronous conversion window at the JavaScript boundary.
const SCRATCH = new WebAssembly.Memory({ initial: 1, maximum: 1 });
const SCRATCH_VIEW = new DataView(SCRATCH.buffer);
const SCALAR_CHUNK = 4096;

// Host declarations already receive and return JavaScript strings: the module
// converts immediately around each call.
const textOf = (value) => value;
const hostText = (value) => String(value);

let inputIterator = null;
let inputRemaining = 0;
let outputChunks = [];

function textLength(value) {
  const body = String(value);
  let length = 0;
  for (const _ of body) length += 1;
  inputRemaining = length;
  inputIterator = length === 0 ? null : body[Symbol.iterator]();
  return length;
}

// A JavaScript string is UTF-16 and may hold a lone surrogate; a Kite `str` is
// an array of Unicode scalar values, and a surrogate is not one. Substituting
// U+FFFD is what every other decoder on this boundary does, and it is the only
// point a surrogate could enter: literals come from Rust `char`s and
// `from_code` already refuses the range. Left through, `str("\uD800")` gave a
// value the VM and native backends cannot represent, so the same program
// compared and hashed differently depending on where it ran.
const SCALAR = (code) => (code >= 0xd800 && code <= 0xdfff ? 0xfffd : code);

function textFill(requested) {
  if (inputIterator === null) return 0;
  const count = Math.min(Number(requested), inputRemaining, SCALAR_CHUNK);
  let written = 0;
  while (written < count) {
    const next = inputIterator.next();
    if (next.done) break;
    SCRATCH_VIEW.setUint32(written * 4, SCALAR(next.value.codePointAt(0)), true);
    written += 1;
  }
  inputRemaining -= written;
  if (inputRemaining === 0 || written !== count) {
    inputIterator = null;
    inputRemaining = 0;
  }
  return written;
}

function textPush(count, first, last) {
  if (first) outputChunks = [];
  const length = Math.min(Number(count), SCALAR_CHUNK);
  const points = new Array(length);
  for (let i = 0; i < length; i += 1) {
    points[i] = SCRATCH_VIEW.getUint32(i * 4, true);
  }
  outputChunks.push(String.fromCodePoint(...points));
  if (!last) return null;
  const answer = outputChunks.join("");
  outputChunks = [];
  return answer;
}

let stringExports = null;

function stringRuntime() {
  if (stringExports === null) {
    throw new Error("instantiate the Kite module before converting strings");
  }
  return stringExports;
}

export function str(value) {
  return stringRuntime()["$kite.str.from_host"](String(value));
}

export function text(value) {
  return stringRuntime()["$kite.str.to_host"](value);
}
"#,
    );
    let compile_step = String::from(
        "  const { instance } = await WebAssembly.instantiate(bytes, imports());\n\
         \x20 stringExports = instance.exports;",
    );
    let mut groups: Vec<&str> = Vec::new();
    for h in hosts {
        if !groups.contains(&h.group.as_str()) {
            groups.push(&h.group);
        }
    }
    let host_entries = groups
        .iter()
        .map(|g| {
            let entries = hosts
                .iter()
                .filter(|h| h.group == *g)
                .map(|h| {
                    format!(
                        "      {}: (...args) => missing({}, {})(...args),",
                        json_ident(&h.name),
                        json_string(g),
                        json_string(&h.name)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("    {}: {{\n{}\n    }},", json_string(g), entries)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let host_section = if hosts.is_empty() {
        String::new()
    } else {
        [
            "",
            "// Host groups this program declared with `@host(\"…\")`. Each entry is a",
            "// function the module imports, and `provide` replaces one before the module",
            "// is instantiated. A declaration nobody supplied fails saying which it was,",
            "// rather than quietly doing nothing.",
            "const HOSTS = {",
            &host_entries,
            "};",
            NET_HOST,
            CRYPTO_HOST,
            AUDIO_HOST,
            DOM_HOST,
            JS_HOST,
            "",
            "function missing(group, name) {",
            "  return () => {",
            "    throw new Error(`no host supplied for @host(\"${group}\") ${name}`);",
            "  };",
            "}",
            "",
            "/// Supply a host group before the module is instantiated.",
            "export function provide(group, functions) {",
            "  HOSTS[group] = Object.assign(HOSTS[group] ?? {}, functions);",
            "}",
            "",
        ]
        .join("\n")
    };

    strip_comments(&format!(
        r#"// Generated by kitec. Do not edit.
//
// Kite programs reach the host through a declared boundary rather than an
// ambient runtime, so everything the module can do is visible in this file.

{hosts}
{strings_section}

// An `int` is an i64, which reaches here as a BigInt.
const showInt = (v) => String(v);

// Floats print so they read back as Kite floats: `1.0`, not `1`.
const showFloat = (v) =>
  Number.isFinite(v) && Number.isInteger(v) ? v.toFixed(1) : String(v);

const showBool = (v) => (v ? "true" : "false");

// A string that has to survive being written into a line-oriented transcript.
// A field's value can hold a newline — that is what a text area is for — and a
// transcript is read a line at a time, so one call has to stay one line.
const showLine = (v) => String(v).replace(/\\/g, "\\\\").replace(/\n/g, "\\n");

// ---- rendering ------------------------------------------------------------
//
// A Kite program draws by calling `draw.rect` and `draw.text`, and knows
// nothing else about the host. Everything a layout produces is one of those
// two, which is what lets a DOM renderer and a canvas renderer meet the same
// interface — and what makes it impossible for them to disagree about where
// something went, because neither decides.

const hex = (colour) => '#' + (colour >>> 0).toString(16).padStart(6, '0');
// The host's font, and the only place it is decided. A Kite program never
// names a font: it asks `text.width` how wide a run is and places it, so the
// answer here changes what a layout *measures* as well as what it looks like.
// A proportional face is the honest default — it is what an application is set
// in — and a monospace one was only ever convenient for lining up the text
// renderer's output, which does not use this at all.
export const FAMILY = 'Roboto, "Helvetica Neue", "Segoe UI", system-ui, sans-serif';
export const NOMINAL_SIZE = 16;

// The font `draw.font` last selected. State on the host, because that is what
// the boundary says it is — and it governs measurement as well as drawing, so
// a run laid out at 22dp is not painted at 16.
// How opaque everything drawn is, until the next `draw.alpha`. Skia keeps
// this on the paint that accompanies each call; here it is host state, which
// is the same idiom the font and the clip already use.
export let alpha = 1;

export function setAlpha(a) {{
  alpha = a < 0 ? 0 : a > 1 ? 1 : a;
}}

export let fontSize = NOMINAL_SIZE;
export let fontWeight = 400;
export const fontCss = () => fontWeight + ' ' + fontSize + 'px ' + FAMILY;
export function setFont(size, weight) {{
  fontSize = size;
  fontWeight = weight;
}}

// The default font, for everything that has not asked for another.
export const FONT = fontCss();

// ---- font fallback ----------------------------------------------------------
//
// §5's table gives the canvas renderer "explicit font stack + `document.fonts`
// query", against the browser's own fallback on the DOM side. The stack is
// `FAMILY` above. This is the query, and the reason it is needed:
//
// A canvas measures with whatever font is resolved *at the moment it asks*.
// If the first choice in the stack is a web font that has not arrived, the
// measurement is taken in the fallback, the layout is computed from it, and
// then the web font loads and every line is the wrong width — the whole page
// reflows one frame late, or worse, does not reflow at all because nothing
// told the program anything changed.
//
// The DOM renderer is immune: the browser re-lays-out its own text nodes. So
// this exists for the canvas, which is exactly where §5 says it does.

/// The families in `FAMILY`, unquoted, in order.
///
/// Parsed rather than listed separately so the two cannot drift — the stack is
/// written once, at the top of this file.
export function fontStack() {{
  return FAMILY.split(',').map((f) => f.trim().replace(/^["']|["']$/g, ''));
}}

/// Whether a family is really there, decided by measuring rather than asking.
///
/// `document.fonts.check` is the obvious call and it does not answer this
/// question: it reports on the `FontFace`s in the document's own font set, so
/// for a family that is not a web font — every system font — it finds nothing
/// pending and returns true. Asked about a family the machine has never heard
/// of, it says yes.
///
/// So the family is measured against a generic instead. A string set in
/// `Family, monospace` renders in monospace if `Family` did not resolve, and in
/// `Family` if it did; the widths differ in the first case and match in the
/// second. Two generics are used because a family that happens to be metrically
/// identical to one of them would be a false negative against that one alone.
const FAMILY_PRESENT = new Map();

export function familyPresent(family) {{
  const cached = FAMILY_PRESENT.get(family);
  if (cached !== undefined) return cached;
  if (typeof document === 'undefined') return false;
  // Glyphs with wide metric variation between typefaces, so two different
  // faces are unlikely to measure the same by accident.
  const probe = 'mmmwwwiiilll@#%WM';
  const ctx = document.createElement('canvas').getContext('2d');
  const width = (spec) => {{
    ctx.font = '72px ' + spec;
    return ctx.measureText(probe).width;
  }};
  const present = ['monospace', 'serif'].some(
    (generic) => width('"' + family + '", ' + generic) !== width(generic),
  );
  FAMILY_PRESENT.set(family, present);
  return present;
}}

/// The first family in the stack the host can actually render.
///
/// The last entry is `sans-serif`, a generic that always resolves — so this
/// always answers, and always answers with something drawable.
export function resolvedFamily() {{
  const stack = fontStack();
  for (const family of stack) {{
    // A generic keyword always resolves and is never *loaded*, so it ends the
    // search wherever it appears.
    if (family === 'system-ui' || family === 'sans-serif' || family === 'serif') return family;
    if (familyPresent(family)) return family;
  }}
  return stack[stack.length - 1];
}}

/// Tell `whenReady` to run once the font situation has settled.
///
/// Two things have to happen and either may be first: the document's fonts
/// finish loading, and the family the stack resolves to stops changing. A page
/// with no web fonts at all settles immediately, which is the common case and
/// costs one promise.
export function watchFonts(whenReady) {{
  if (typeof document === 'undefined' || !document.fonts) return;
  let settled = resolvedFamily();
  const check = () => {{
    // A web font that has just arrived changes what the probe measures, so the
    // answer cached before it landed is stale.
    FAMILY_PRESENT.clear();
    const now = resolvedFamily();
    if (now === settled) return;
    settled = now;
    // The measurements every laid-out line was computed from have changed, so
    // this is not a repaint — it is a relayout, and only the program can do
    // that. The caller is expected to draw a fresh frame from the model.
    whenReady(now);
  }};
  document.fonts.addEventListener('loadingdone', check);
  // `ready` covers the fonts already in flight when this runs, which
  // `loadingdone` does not fire for if they finished first.
  document.fonts.ready.then(check);
}}

/// One rounded rectangle as a subpath. Shared, because `drrect` needs two of
/// them in one path and a second copy of the arc arithmetic is a second place
/// for the two renderers to drift apart.
///
/// The radius is clamped to half the shorter side by the caller, which is what
/// turns a radius past that into a pill rather than a shape the path builder
/// refuses.
function roundRectPath(ctx, x, y, w, h, radius) {{
  if (ctx.roundRect) {{
    ctx.roundRect(x, y, w, h, radius);
    return;
  }}
  // Four arcs and four edges: the same shape, for a context without
  // `roundRect`. A renderer that silently drew square corners here would
  // disagree with the DOM one about the picture.
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}}

export function canvasRenderer(ctx) {{
  // A clip nests with `save`/`restore`, so an unbalanced `unclip` would
  // restore state a caller never saved. The depth is tracked to refuse that
  // rather than corrupt the context.
  let depth = 0;
  // Glyphs the atlas can prove safe are blitted from cached tiles; anything
  // it cannot prove falls back to `fillText`, which is always correct.
  const atlas = glyphAtlas(ctx);
  const renderer = {{
    rect: (x, y, w, h, colour) => {{
      ctx.globalAlpha = alpha;
      ctx.fillStyle = hex(colour);
      ctx.fillRect(x, y, w, h);
      ctx.globalAlpha = 1;
    }},
    drrect: (x, y, w, h, r, width, colour) => {{
      ctx.globalAlpha = alpha;
      ctx.fillStyle = hex(colour);
      const outer = Math.max(0, Math.min(r, Math.min(w, h) / 2));
      const inset = Math.max(0, Math.min(width, Math.min(w, h) / 2));
      const inner = Math.max(0, outer - inset);
      ctx.beginPath();
      roundRectPath(ctx, x, y, w, h, outer);
      roundRectPath(
        ctx,
        x + inset,
        y + inset,
        Math.max(0, w - inset * 2),
        Math.max(0, h - inset * 2),
        inner,
      );
      // Even-odd, so the inner rectangle is a hole rather than a second fill.
      // This is what makes a ring a ring: nothing is painted inside it, so
      // whatever it sits on shows through without anyone naming that colour.
      ctx.fill('evenodd');
      ctx.globalAlpha = 1;
    }},
    rrect: (x, y, w, h, r, colour) => {{
      ctx.globalAlpha = alpha;
      ctx.fillStyle = hex(colour);
      ctx.beginPath();
      roundRectPath(ctx, x, y, w, h, Math.max(0, Math.min(r, Math.min(w, h) / 2)));
      ctx.fill();
      ctx.globalAlpha = 1;
    }},
    // Fitted into its box the way `object-fit: contain` fits an `<img>`, so a
    // picture whose proportions differ from its box is letterboxed rather than
    // stretched.
    image: (x, y, w, h, src) => {{
      const img = imageFor(src);
      if (!img || !img.complete || img.naturalWidth === 0) return;
      const scale = Math.min(w / img.naturalWidth, h / img.naturalHeight);
      const dw = img.naturalWidth * scale;
      const dh = img.naturalHeight * scale;
      ctx.globalAlpha = alpha;
      ctx.drawImage(img, x + (w - dw) / 2, y + (h - dh) / 2, dw, dh);
      ctx.globalAlpha = 1;
    }},
    text: (x, y, body, colour) => {{
      // The atlas caches tiles keyed by glyph, so it is only safe while the
      // font it was built for is the one in force.
      // Drawn a measured offset below the em box's top — see `baselineOffset`.
      const top = y + baselineOffset();
      // The atlas blits pre-rasterised opaque tiles, so it can only answer for
      // fully opaque text.
      if (alpha === 1 && fontCss() === FONT && atlas && atlas.text(x, top, body, colour)) return;
      ctx.globalAlpha = alpha;
      ctx.fillStyle = hex(colour);
      ctx.font = fontCss();
      ctx.textBaseline = 'top';
      // The anchor stays the left edge whichever way the run reads — the
      // layout computed an x, not an alignment.
      ctx.textAlign = 'left';
      ctx.direction = firstStrongRtl(body) ? 'rtl' : 'ltr';
      ctx.fillText(body, x, top);
      ctx.globalAlpha = 1;
    }},
    clip: (x, y, w, h) => {{
      ctx.save();
      depth += 1;
      ctx.beginPath();
      ctx.rect(x, y, w, h);
      ctx.clip();
    }},
    unclip: () => {{
      if (depth > 0) {{
        ctx.restore();
        depth -= 1;
      }}
    }},
  }};
  return renderer;
}}

// ---- the glyph atlas -------------------------------------------------------
//
// `fillText` rasterises every glyph of every run on every repaint. An atlas
// rasterises a glyph once — into an offscreen tile keyed by (code point,
// font, colour) — and blits the tile everywhere that glyph appears again,
// which is the difference between text costing a rasteriser and text costing
// a copy.
//
// The honesty is in the plan, not the cache: a run is served from the atlas
// only when drawing it one glyph at a time is provably the picture `fillText`
// would draw. That means every glyph is one code point with its own advance,
// the advances sum to the measured width of the whole run — a font that
// kerned or ligated the run fails that sum and falls back — and nothing in
// the run asks for shaping the atlas cannot do: unjoined Arabic is refused
// because a letter's shape there depends on its neighbours (presentation
// forms, already joined, are served; `std/text.join_arabic` produces them),
// and emoji, joiner sequences and everything outside the Basic Multilingual
// Plane are refused rather than guessed at. A combining mark is the one
// shaping the atlas does honour: it advances nothing and rides the glyph
// before it.
//
// What no plan can prove here is pixel identity — antialiasing a glyph alone
// and antialiasing it mid-run can differ, and tile blits are snapped to the
// device pixel grid, which `fillText` does not do. That is the stated cost of
// the cache, and the fallback path exists for everything the plan refuses.
//
// WebGPU is deliberately absent. The roadmap's own cut list defers it, and
// the atlas is the part of a GPU text pipeline that pays for itself on
// Canvas2D today — a WebGPU renderer would keep the same tiles and change
// where they are composited, so nothing built here is thrown away when it
// earns its place.

/// Whether the first strong character reads right-to-left — rule P2, over the
/// blocks this stack covers: Hebrew and Arabic with their presentation forms
/// answer right-to-left; Latin, Greek and Cyrillic answer left-to-right;
/// digits, marks and punctuation keep scanning. A run with no strong
/// character reads left-to-right, which is why `std/text` pre-reverses the
/// one run shape that would get this wrong.
export function firstStrongRtl(body) {{
  for (const ch of body) {{
    const cp = ch.codePointAt(0);
    if (
      (cp >= 0x0590 && cp <= 0x08ff) ||
      (cp >= 0xfb1d && cp <= 0xfdff) ||
      (cp >= 0xfe70 && cp <= 0xfeff)
    ) {{
      return true;
    }}
    if ((cp >= 0x41 && cp <= 0x5a) || (cp >= 0x61 && cp <= 0x7a) || (cp >= 0xc0 && cp < 0x0590)) {{
      return false;
    }}
  }}
  return false;
}}

/// How to draw a run one glyph at a time, or null when that cannot be proven
/// to match `fillText`. Pure — it needs a measurer and nothing else — so the
/// rules are testable under Node, where no canvas exists.
///
/// Each entry is a glyph and its x offset from the run's origin. A combining
/// mark — any character the font measures at zero advance — shares the pen
/// with whatever follows it: it advances nothing, exactly as `Mn` should. A
/// right-to-left run comes out in reverse cluster order, marks staying with
/// their bases, which is rule L2 applied to a run the resolver already made
/// single-direction; a run mixing strong directions is refused, because
/// ordering it is the resolver's job, not the painter's.
/// Whether one code point can be assumed to be one glyph, at a fixed advance,
/// in the order it was written.
///
/// Deliberately narrow. Being wrong in the permissive direction mangles text;
/// being wrong in the strict direction costs a `fillText`, which is what the
/// atlas is an optimisation over in the first place.
function simpleGlyph(cp) {{
  // ASCII and the Latin supplements, through Latin Extended-B.
  if (cp >= 0x20 && cp <= 0x24f) return true;
  // Greek and Cyrillic.
  if (cp >= 0x370 && cp <= 0x4ff) return true;
  // The combining diacriticals, which sit over a Latin, Greek or Cyrillic
  // base. These reposition vertically and advance nothing, which is the case
  // the cluster logic above already handles and proves with the advance sum.
  // What it cannot handle is a mark that moves its *base*, which is what the
  // Brahmic scripts do and why they are not here.
  if (cp >= 0x0300 && cp <= 0x036f) return true;
  // Hebrew, points included: the points are zero-advance marks over a base,
  // the same case as the diacriticals.
  if (cp >= 0x0590 && cp <= 0x05ff) return true;
  // Arabic *presentation* forms — the joined shapes `std/text.join_arabic`
  // produces, each already one glyph. Unjoined Arabic is refused by omission,
  // which is the whole point of listing rather than excluding.
  if (cp >= 0xfe70 && cp <= 0xfefe) return true;
  // General punctuation, minus the formatting characters refused above.
  if (cp >= 0x2010 && cp <= 0x205e) return true;
  // Currency, arrows, and the mathematical and geometric symbols.
  if (cp >= 0x20a0 && cp <= 0x2bff) return true;
  // CJK punctuation, kana, and the unified ideographs.
  if (cp >= 0x3000 && cp <= 0x30ff) return true;
  if (cp >= 0x4e00 && cp <= 0x9fff) return true;
  // Halfwidth and fullwidth forms.
  if (cp >= 0xff00 && cp <= 0xffef) return true;
  return false;
}}

export function atlasPlan(body, measureOne) {{
  const measurer = measureOne ?? measure;
  const clusters = [];
  let sawRtl = false;
  let sawLtr = false;
  for (const ch of body) {{
    const cp = ch.codePointAt(0);
    // The supplementary planes hold the emoji, and an emoji is not one glyph.
    if (cp > 0xffff) return null;
    // Joiners and variation selectors change the glyphs around them.
    if (cp === 0x200d || (cp >= 0xfe00 && cp <= 0xfe0f)) return null;
    // Formatting characters draw nothing and should never reach a painter.
    if (
      (cp >= 0x200b && cp <= 0x200f) ||
      (cp >= 0x202a && cp <= 0x202e) ||
      (cp >= 0x2060 && cp <= 0x206f) ||
      cp === 0x061c ||
      cp === 0xfeff
    ) {{
      return null;
    }}
    // Everything else is an allow-list, and it is an allow-list on purpose.
    //
    // This started as a list of scripts to *refuse* — emoji, joiners, unjoined
    // Arabic — and the trouble with refusing by name is that the ones nobody
    // named get drawn wrongly in silence. Burmese did: `အပြန်` encodes its
    // medial ra after the consonant and the font draws it wrapped around the
    // front, and `မွန်` stacks a medial underneath, so blitting one tile per
    // code point at its own advance scattered the marks across the line.
    //
    // The atlas is only ever valid where one code point is one glyph at a
    // fixed advance, in visual order. That is true of Latin, Greek, Cyrillic,
    // the CJK ideographs and kana, and unpointed Hebrew. It is not true of any
    // Brahmic script — Devanagari, Burmese, Thai, Khmer, Tamil — nor of
    // Hangul jamo, and it will not be true of the next script Unicode adds.
    // So the rule now runs the other way: prove a character is simple, or fall
    // back to `fillText`, which shapes properly and is always correct.
    if (!simpleGlyph(cp)) return null;
    const advance = measurer(ch);
    if (!(advance >= 0)) return null;
    if (advance === 0 && clusters.length > 0) {{
      clusters[clusters.length - 1].marks.push(ch);
      continue;
    }}
    const rtlChar =
      (cp >= 0x0590 && cp <= 0x05ff) || (cp >= 0xfb1d && cp <= 0xfdff) ||
      (cp >= 0xfe70 && cp <= 0xfefe);
    if (rtlChar) sawRtl = true;
    else if (cp !== 0x20) sawLtr = true;
    clusters.push({{ ch, advance, marks: [] }});
  }}
  if (sawRtl && sawLtr) return null;
  const ordered = sawRtl ? [...clusters].reverse() : clusters;
  let pen = 0;
  const entries = [];
  for (const cluster of ordered) {{
    entries.push({{ ch: cluster.ch, x: pen }});
    for (const mark of cluster.marks) {{
      entries.push({{ ch: mark, x: pen + cluster.advance }});
    }}
    pen += cluster.advance;
  }}
  // The proof: glyph advances must sum to what the font says the whole run
  // measures. A kerned pair or a ligature makes these differ, and a run the
  // font would draw differently is a run the atlas must not touch.
  if (Math.abs(pen - measurer(body)) > 0.5) return null;
  return entries;
}}

/// A tile per (code point, font, colour), rasterised once. `makeTile` is
/// injectable so the cache logic runs under Node; the default rasterises with
/// a real canvas and reports null where there is none, which turns the whole
/// atlas off rather than half on.
function defaultTileMaker(font, scale) {{
  if (typeof document === 'undefined') return null;
  const measurer = document.createElement('canvas').getContext('2d');
  measurer.font = font;
  measurer.textBaseline = 'top';
  return (ch, colour) => {{
    const m = measurer.measureText(ch);
    // Without the ink bounds a tile cannot be sized: a mark's ink sits left
    // of its origin, an italic overhangs its advance. No bounds, no atlas.
    if (m.actualBoundingBoxLeft === undefined || m.actualBoundingBoxRight === undefined) {{
      return null;
    }}
    const left = Math.ceil(Math.max(0, m.actualBoundingBoxLeft)) + 1;
    const top = Math.ceil(Math.max(0, m.actualBoundingBoxAscent ?? 0)) + 1;
    const w = left + Math.ceil(Math.max(m.actualBoundingBoxRight, m.width)) + 2;
    const h = top + Math.ceil(Math.max(0, m.actualBoundingBoxDescent ?? lineHeight())) + 2;
    const tile = document.createElement('canvas');
    tile.width = Math.max(1, Math.ceil(w * scale));
    tile.height = Math.max(1, Math.ceil(h * scale));
    const tctx = tile.getContext('2d');
    tctx.scale(scale, scale);
    tctx.font = font;
    tctx.textBaseline = 'top';
    tctx.fillStyle = colour;
    tctx.fillText(ch, left, top);
    return {{ canvas: tile, left, top, w, h }};
  }};
}}

/// The atlas over a context: plan, cache, blit — and `false` for any run the
/// plan refuses or any glyph the tile maker cannot serve, so the caller's
/// `fillText` path stays the answer of last resort. `stats()` reports how
/// many tiles were rasterised against how many were reused, which is the
/// measurable half of "prove it helps".
export function glyphAtlas(ctx, font = FONT, makeTile = null) {{
  const scale = (ctx.getTransform ? ctx.getTransform().a : 1) || 1;
  const rasterise = makeTile ?? defaultTileMaker(font, scale);
  if (!rasterise) return null;
  const tiles = new Map();
  let rasterised = 0;
  let reused = 0;
  let fallbacks = 0;
  const tileFor = (ch, colour) => {{
    const key = ch + '\0' + font + '\0' + colour;
    let tile = tiles.get(key);
    if (tile === undefined) {{
      // The cap is crude on purpose: past it, everything is dropped and the
      // atlas warms again. An eviction policy would be state to get wrong,
      // and 4096 tiles outlasts any plausible working set of glyphs.
      if (tiles.size >= 4096) tiles.clear();
      tile = rasterise(ch, hex(colour));
      tiles.set(key, tile);
      if (tile) rasterised += 1;
    }} else if (tile) {{
      reused += 1;
    }}
    return tile;
  }};
  return {{
    stats: () => ({{ tiles: tiles.size, rasterised, reused, fallbacks }}),
    text: (x, y, body, colour) => {{
      const plan = atlasPlan(body, measure);
      if (plan === null) {{
        fallbacks += 1;
        return false;
      }}
      // Every tile is fetched before anything is painted: a run is drawn
      // whole from the atlas or not at all, never half and half.
      const placed = [];
      for (const glyph of plan) {{
        const tile = tileFor(glyph.ch, colour);
        if (!tile) {{
          fallbacks += 1;
          return false;
        }}
        placed.push([tile, glyph.x]);
      }}
      for (const [tile, gx] of placed) {{
        // Snapped to the device pixel grid: a tile blitted at a fractional
        // position is resampled into blur. The snap is at most half a device
        // pixel, and it is the one place the atlas admits to differing from
        // `fillText`.
        const px = Math.round((x + gx - tile.left) * scale) / scale;
        const py = Math.round((y - tile.top) * scale) / scale;
        ctx.drawImage(tile.canvas, px, py, tile.w, tile.h);
      }}
      return true;
    }},
  }};
}}

// How wide a run of text is. Only the host knows the font, so this is the
// host's answer — and two hosts may legitimately differ, which is what
// different fonts are.
//
// The default is the nominal advance, matching the bytecode VM, so a layout is
// comparable across backends under test. A page that draws installs a real
// measurer and gets numbers that match what it will actually paint.
const NOMINAL_ADVANCE = 8;
export let measure = (body) =>
  [...body].length * NOMINAL_ADVANCE * (fontSize / NOMINAL_SIZE);

export function setMeasure(fn) {{
  measure = fn;
}}

// What one line of text occupies: ascent plus descent plus leading. A canvas
// reports the first two, and the third is what the difference between them and
// the em box amounts to.
// A function rather than a number, because the font it describes can change
// between one run of text and the next.
export let lineHeight = () => NOMINAL_SIZE * (fontSize / NOMINAL_SIZE);

export function setLineHeight(fn) {{
  lineHeight = fn;
}}

/// Pictures the canvas renderer has asked for, by source.
///
/// A canvas draws now and an image arrives later, which is the one place a
/// drawing call cannot be satisfied synchronously. Rather than let a frame
/// block, the first sight of a source starts the fetch and draws nothing; when
/// the bytes land, the page is asked to paint again and the second frame has
/// the picture in it.
///
/// The DOM renderer needs none of this — an `<img>` is the browser's own
/// version of exactly this cache, and a far better one.
const IMAGES = new Map();
/// What to call when a picture arrives and the frame that wanted it is stale.
let onImageLoad = null;

export function setImageLoad(fn) {{
  onImageLoad = fn;
}}

function imageFor(src) {{
  const cached = IMAGES.get(src);
  if (cached !== undefined) return cached;
  if (typeof Image === 'undefined') {{
    IMAGES.set(src, null);
    return null;
  }}
  const img = new Image();
  // A record with no picture in it yet. `complete` is what the draw path
  // tests, so a half-loaded entry draws nothing rather than throwing.
  IMAGES.set(src, img);
  img.addEventListener('load', () => {{
    if (typeof onImageLoad === 'function') onImageLoad(src);
  }});
  // A source that will not load is remembered as having failed, so the frame
  // loop is not woken once per frame forever by something that is never
  // coming.
  img.addEventListener('error', () => {{
    IMAGES.set(src, null);
  }});
  img.src = src;
  return img;
}}

/// Break a run into lines at measured width. Wrapping for a canvas: nothing
///
/// A second implementation of a rule that already exists in Kite, which is
/// normally the thing to avoid — but the string this breaks is the *value of a
/// field*. That belongs to the model, it changes on every keystroke, and the
/// layout never saw it: the tree carries the box, not the text inside it. So
/// there is nothing here to reuse.
///
/// It is kept faithful rather than approximate — hard breaks first, then words,
/// then single wide characters, measured with the same measurer — because the
/// two renderers agreeing about where a line ends is the property the whole
/// split rests on. Where it is used is narrow: only a renderer that cannot make
/// a real element has to draw a text area itself.
export function wrapText(body, limit) {{
  const wideAt = measure('W') * 1.2;
  const out = [];
  for (const paragraph of body.split('\n')) {{
    const pieces = [];
    let current = '';
    let spaced = false;
    for (const c of paragraph) {{
      if (c === ' ') {{
        if (current.length > 0) {{
          pieces.push([current, spaced]);
          current = '';
        }}
        spaced = true;
      }} else if (measure(c) > wideAt) {{
        if (current.length > 0) {{
          pieces.push([current, spaced]);
          current = '';
          spaced = false;
        }}
        pieces.push([c, spaced]);
        spaced = false;
      }} else current += c;
    }}
    if (current.length > 0) pieces.push([current, spaced]);
    if (pieces.length === 0) {{
      out.push('');
      continue;
    }}
    let line = pieces[0][0];
    for (let i = 1; i < pieces.length; i += 1) {{
      const candidate = pieces[i][1] ? line + ' ' + pieces[i][0] : line + pieces[i][0];
      if (measure(candidate) <= limit) line = candidate;
      else {{
        out.push(line);
        line = pieces[i][0];
      }}
    }}
    out.push(line);
  }}
  return out;
}}

/// Measure with a canvas, in the font the renderers draw with. A canvas is
/// used even for the DOM renderer: `measureText` is the same shaping the
/// browser applies to a text node, and it costs no layout.
// How far below `y` a run of text has to be drawn on a canvas for it to land
// where the DOM puts it.
//
// The two renderers agree about every rectangle and disagreed about text by a
// couple of pixels, which read as the whole layout shifting up when you
// switched to the canvas. The cause is that they anchor a line differently:
// `textBaseline = 'top'` is the top of the font's *em square*, while a DOM
// text node's first line starts at the top of its **content box**, and those
// are not the same place — the em square usually sits higher.
//
// Rather than guess at the difference, it is measured, once per font: where the
// DOM puts the alphabetic baseline in a line box of `lineHeight()`, minus where
// the canvas puts it under `'top'`. `measureText` reports the second directly.
// A host with no `document` — Node — has no canvas either, so the offset is
// zero and nothing that runs there is affected.
const BASELINES = new Map();

export function baselineOffset() {{
  const key = fontCss();
  const cached = BASELINES.get(key);
  if (cached !== undefined) return cached;
  if (typeof document === 'undefined') return 0;
  const ctx = document.createElement('canvas').getContext('2d');
  ctx.font = key;
  ctx.textBaseline = 'top';
  // Distance from the `'top'` anchor *down* to the alphabetic baseline. The
  // metric is positive going up, so under `'top'` — which is above the
  // baseline — it is reported negative, and the distance downwards is its
  // negation. Taking it at face value adds an em instead of a pixel or two,
  // which drops every label clean out of the bottom of its box.
  const fromTop = -(ctx.measureText('Mg').alphabeticBaseline ?? 0);
  const probe = document.createElement('div');
  probe.style.cssText =
    'position:absolute;visibility:hidden;white-space:pre;font:' + key +
    ';line-height:' + lineHeight() + 'px';
  probe.textContent = 'Mg';
  const marker = document.createElement('span');
  marker.style.cssText = 'display:inline-block;width:0;height:0;vertical-align:baseline';
  probe.appendChild(marker);
  document.body.appendChild(probe);
  const domFromTop = marker.getBoundingClientRect().top - probe.getBoundingClientRect().top;
  probe.remove();
  const offset = Number.isFinite(domFromTop - fromTop) ? domFromTop - fromTop : 0;
  BASELINES.set(key, offset);
  return offset;
}}

export function fontMeasure() {{
  const ctx = document.createElement('canvas').getContext('2d');
  return (body) => {{
    ctx.font = fontCss();
    return ctx.measureText(body).width;
  }};
}}

/// The font's own line height, from the metrics a canvas reports. Falling back
/// to the nominal value matters: `fontBoundingBox*` is not universal, and a
/// layout that produced `NaN` would place everything at zero.
export function fontLineHeight() {{
  const ctx = document.createElement('canvas').getContext('2d');
  return () => {{
    ctx.font = fontCss();
    const m = ctx.measureText('Mg');
    const ascent = m.fontBoundingBoxAscent ?? m.actualBoundingBoxAscent;
    const descent = m.fontBoundingBoxDescent ?? m.actualBoundingBoxDescent;
    const height = (ascent ?? 0) + (descent ?? 0);
    return Number.isFinite(height) && height > 0 ? height : fontSize;
  }};
}}

// The default: describe each call. Useful under Node, and the same text the
// bytecode VM writes, so the two backends can be compared without a browser.
// Coordinates print the way Kite prints a float, so this and the bytecode VM
// produce the same text and the differential suite can compare drawing.
export const textRenderer = {{
  rect: (x, y, w, h, colour) =>
    write(
      'rect ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + colour,
    ),
  rrect: (x, y, w, h, r, colour) =>
    write(
      'rrect ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + showFloat(r) + ' ' + colour,
    ),
  drrect: (x, y, w, h, r, width, colour) =>
    write(
      'drrect ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + showFloat(r) + ' ' +
      showFloat(width) + ' ' + colour,
    ),
  alpha: (a) => write('alpha ' + showFloat(a)),
  text: (x, y, body, colour) =>
    write('text ' + showFloat(x) + ' ' + showFloat(y) + ' ' + body + ' ' + colour),
  // A transcript has no regions, so it writes what the field says. The same
  // degradation the canvas makes: only a DOM has a real element to become.
  field: (x, y, w, h, value, hint, colour, id, multiline) =>
    write(
      'field ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + showLine(value) + ' ' +
      showLine(hint) + ' ' + colour + ' ' + showLine(id) + ' ' + multiline,
    ),
  image: (x, y, w, h, src) =>
    write(
      'image ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + showLine(src),
    ),
  // A transcript is the one backend where the parallel tree and the picture
  // are the same kind of thing, which is what makes it the thing an audit
  // could read. Nothing in the toolchain reads it today: the auditor that did
  // was removed with `std/ui`, and a canvas program that wants to declare its
  // own semantics is what the call is left for.
  // Label last: it is the only field that may contain a space, so everything
  // before it is fixed and the rest of the line is the label.
  semantics: (x, y, w, h, role, label, flags, id) =>
    write(
      'semantics ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + role + ' ' + flags + ' ' +
      showLine(id) + ' ' + showLine(label),
    ),
  // Selecting a font writes nothing: it changes no pixel by itself, and its
  // whole effect is on where the runs after it land. Measurement selects a
  // font too, so a transcript that recorded it would be a transcript of the
  // layout rather than of the picture.
  font: (size, weight) => {{}},
  clip: (x, y, w, h) =>
    write(
      'clip ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h),
    ),
  unclip: () => write('unclip'),
}};

export let renderer = textRenderer;

export function setRenderer(r) {{
  renderer = r;
}}

/// Where output goes.
export let write = (line) => console.log(line);

/// Redirect it. A module's exported binding is read-only to whoever imports
/// it, so capturing output needs a function on this side rather than an
/// assignment on the other.
export function setWriter(fn) {{
  write = fn;
}}

function imports() {{
  return {{
{host_spread}    kite: {{
      scratch: SCRATCH,
      text_len: textLength,
      text_fill: textFill,
      text_push: textPush,
      print_int: (v) => write(showInt(v)),
      print_float: (v) => write(showFloat(v)),
      print_bool: (v) => write(showBool(v)),
      print_str: (body) => write(body),
      draw_rect: (x, y, w, h, colour) => renderer.rect(x, y, w, h, Number(colour)),
      draw_rrect: (x, y, w, h, r, colour) => renderer.rrect(x, y, w, h, r, Number(colour)),
      draw_drrect: (x, y, w, h, r, width, colour) =>
        renderer.drrect(x, y, w, h, r, width, Number(colour)),
      draw_alpha: (a) => {{
        setAlpha(a);
        if (renderer.alpha) renderer.alpha(a);
      }},
      draw_text: (x, y, body, colour) => renderer.text(x, y, body, Number(colour)),
      draw_field: (x, y, w, h, v, hint, colour, id, multiline) =>
        renderer.field(
          x, y, w, h, v, hint, Number(colour), id, multiline !== 0,
        ),
      draw_image: (x, y, w, h, src) => renderer.image(x, y, w, h, src),
      draw_semantics: (x, y, w, h, role, label, flags, id) => {{
        if (renderer.semantics) {{
          renderer.semantics(
            x, y, w, h, Number(role), label, Number(flags), id,
          );
        }}
      }},
      draw_clip: (x, y, w, h) => renderer.clip(x, y, w, h),
      draw_unclip: () => renderer.unclip(),
      measure_text: (body) => measure(body),
      line_height: () => lineHeight(),
      draw_font: (size, weight) => {{
        setFont(size, Number(weight));
        if (renderer.font) renderer.font(size, Number(weight));
      }},
      // Interpolation shares its formatting with printing, so a value cannot
      // look one way in `io.print(x)` and another in `"\(x)"`.
      str_of_int: (v) => hostText(showInt(v)),
      // A page has no standard input and no standard error. `console.error` is
      // the nearest honest equivalent of the second; there is no equivalent of
      // the first, so a read answers with the empty string — which is what a
      // program reading until it runs out already tests for.
      read_line: () => hostText(""),
      error_str: (body) => {{
        if (typeof console !== "undefined") console.error(body);
      }},
      str_of_float: (v) => hostText(showFloat(v)),
      str_of_bool: (v) => hostText(showBool(v)),
      // ---- the scheduler ---------------------------------------------------
      //
      // A queue of live tasks is mutable state, and Kite has none, so the
      // scheduler lives here. What crosses the boundary is a resume closure
      // the host cannot look inside: it is handed back through the module's
      // own `kite_poll` export, which is the only thing that can enter it.
      task_spawn: (poll) => {{
        TASKS.push({{ poll, wakeAt: null, parked: false, waitingOnHost: false }});
        // A task started from inside an event handler has nothing else to
        // notice it. Does nothing in the batch driver, which is already looping.
        wake();
      }},
      task_wake_at: (ms) => {{
        wakeRequest = Number(ms);
      }},
      task_park: () => {{
        parkRequest = true;
      }},
      task_wait_host: () => {{
        hostWaitRequest = true;
      }},
      time_now: () => BigInt(now()),
      // A Kite closure, wrapped in something JavaScript can call.
      //
      // The reference is held by the wrapper and by nothing else here — no
      // registry, no numbered handle, no table. That is the whole lifetime
      // story: the closure lives as long as the wrapper, the wrapper lives as
      // long as whatever the page attached it to, and the one collector traces
      // the chain. A registry would pin every listener a page ever installed.
      js_func: (handler, shape) => {{
        // Synchronous, because a listener that wanted to call
        // `preventDefault` could not if this were deferred — and because a
        // comparator handed to `sort` is asked for its answer now.
        const run = (...args) => {{
          const result = invoke(
            shape,
            handler,
            args.map((a) => (a === undefined ? null : a)),
          );
          // The handler may have made a sleeping task runnable, or started
          // one. This happens after the call so that the value is already in
          // hand: a pump that ran first could re-enter and the answer would be
          // whatever the re-entry left behind.
          wake();
          return result;
        }};
        // A wrapper of the handler's own arity, so anything that inspects
        // `fn.length` — and the platform does, for `Array.prototype.sort` and
        // for a `Proxy` trap — sees what the Kite side actually declared.
        switch (handlerArity(shape)) {{
          case 0:
            return () => run();
          case 1:
            return (a) => run(a);
          case 2:
            return (a, b) => run(a, b);
          case 3:
            return (a, b, c) => run(a, b, c);
          default:
            return (a, b, c, d) => run(a, b, c, d);
        }}
      }},
    }},
  }};
}}

// ---- the scheduler --------------------------------------------------------
//
// Round-robin, in spawn order — the same order the bytecode VM polls in, which
// is what lets the two backends be compared at all.
//
// **There are two clocks, and which one is running is the difference between
// the two modes below.**
//
// Virtual: when every task is waiting on a deadline, the clock jumps to the
// earliest one rather than waiting for it. A program that sleeps costs no real
// time, and two backends running the same program produce the same
// interleaving — which is the only reason three backends can be compared at
// all. This is the mode a test runs in, and `drive` is its driver.
//
// Real: `performance.now()`, and a sleeping task waits for a real timer. A
// program living in a page needs this, and the original note here — that a UI
// event loop does not need a real clock because events arrive from the host —
// was true of the old design and is not true of a resident program. A `sleep`
// that returns immediately is hidden control flow by this language's own
// definition.
const TASKS = [];
let clock = 0;
let wakeRequest = null;
let parkRequest = false;
let hostWaitRequest = false;

let realClock = false;
const millis = () =>
  typeof performance !== "undefined" ? performance.now() : Date.now();
const now = () => (realClock ? Math.round(millis()) : clock);

export async function drive(exports) {{
  useExports(exports);
  while (TASKS.length > 0) {{
    let polled = false;
    let completed = false;
    for (let i = 0; i < TASKS.length; ) {{
      const task = TASKS[i];
      if (task.parked || task.waitingOnHost || (task.wakeAt !== null && task.wakeAt > clock)) {{
        i += 1;
        continue;
      }}
      polled = true;
      task.wakeAt = null;
      wakeRequest = null;
      parkRequest = false;
      hostWaitRequest = false;
      const done = exports.kite_poll(task.poll) !== 0;
      if (TASKS[i] === task) {{
        task.wakeAt = wakeRequest;
        task.parked = parkRequest;
        task.waitingOnHost = hostWaitRequest;
      }}
      if (done) {{
        TASKS.splice(i, 1);
        completed = true;
      }} else {{
        i += 1;
      }}
    }}
    // A task finishing is what a parked task is waiting for, and may be what a
    // sleeping one wanted too — so a completion wakes everything and lets each
    // decide for itself.
    if (completed) {{
      for (const t of TASKS) {{
        t.parked = false;
        t.wakeAt = null;
      }}
    }}
    if (!polled) {{
      // Everything is waiting. A task waiting on the host — a fetch, a timer
      // the host owns — needs the event loop to run, which is what yielding to
      // a macrotask does. Only then does the virtual clock move.
      if (TASKS.some((t) => t.waitingOnHost)) {{
        await new Promise((resolve) => setTimeout(resolve, 0));
        for (const t of TASKS) t.waitingOnHost = false;
        continue;
      }}
      const next = TASKS.reduce(
        (best, t) => (t.wakeAt !== null && (best === null || t.wakeAt < best) ? t.wakeAt : best),
        null,
      );
      if (next === null || next <= clock) {{
        throw new Error(TASKS.length + ' task(s) can never make progress');
      }}
      clock = next;
    }}
  }}
}}

// ---- living in a page -----------------------------------------------------
//
// `drive` above runs a program to completion: it returns when the last task
// finishes, and while anything waits on the host it spins on a zero-delay
// timeout. Both are right for a program that starts, does its work and ends,
// and both are fatal to one that sits in a page.
//
// An island is idle almost all of the time. After its last task finishes there
// must still be something to run the next click, and while nothing is happening
// it must cost nothing at all. So this driver does not loop: it makes one pass,
// works out when it could next have something to do, and sleeps until then or
// until something wakes it.

let residentExports = null;
let pumping = false;
let pumpAgain = false;
let timer = null;

/// One pass over the task list.
///
/// Shares its shape with `drive`'s inner loop deliberately: the two must poll
/// in the same order, or a program would interleave differently depending on
/// which driver ran it and the test mode would stop predicting the real one.
function step(exports) {{
  const at = now();
  let completed = false;
  for (let i = 0; i < TASKS.length; ) {{
    const task = TASKS[i];
    if (task.parked || task.waitingOnHost || (task.wakeAt !== null && task.wakeAt > at)) {{
      i += 1;
      continue;
    }}
    task.wakeAt = null;
    wakeRequest = null;
    parkRequest = false;
    hostWaitRequest = false;
    const done = exports.kite_poll(task.poll) !== 0;
    // A handler firing during the poll can splice this task out from under us,
    // which is why the slot is checked before it is written back.
    if (TASKS[i] === task) {{
      task.wakeAt = wakeRequest;
      task.parked = parkRequest;
      task.waitingOnHost = hostWaitRequest;
    }}
    if (done) {{
      TASKS.splice(i, 1);
      completed = true;
    }} else {{
      i += 1;
    }}
  }}
  // A task finishing is what a parked task is waiting for, and may be what a
  // sleeping one wanted too.
  if (completed) {{
    for (const t of TASKS) {{
      t.parked = false;
      t.wakeAt = null;
    }}
  }}
}}

/// When to come back, and nothing sooner.
///
/// A task with no deadline is runnable now. A sleeping task sets the timer to
/// its own deadline. If every task is waiting on the host there is no timer at
/// all — a callback will call `wake`, and until it does this program costs
/// exactly nothing.
function schedule() {{
  if (timer !== null) {{
    clearTimeout(timer);
    timer = null;
  }}
  let soonest = null;
  for (const t of TASKS) {{
    if (t.parked || t.waitingOnHost) continue;
    if (t.wakeAt === null) {{
      soonest = 0;
      break;
    }}
    if (soonest === null || t.wakeAt < soonest) soonest = t.wakeAt;
  }}
  if (soonest === null) return;
  const delay = soonest === 0 ? 0 : Math.max(0, soonest - now());
  timer = setTimeout(() => {{
    timer = null;
    pump();
  }}, delay);
}}

/// Run the tasks, then decide when to run them again.
///
/// **Guarded against re-entry.** A host call can fire a Kite handler
/// synchronously in the middle of a poll — `focus()` is the ordinary example —
/// and that handler may spawn a task, which calls `wake`. Draining recursively
/// would mutate the task list underneath the loop that is walking it, so a
/// wake-up during a pump sets a flag and the pump goes round again.
function pump() {{
  if (residentExports === null || pumping) {{
    if (pumping) pumpAgain = true;
    return;
  }}
  pumping = true;
  try {{
    do {{
      pumpAgain = false;
      step(residentExports);
    }} while (pumpAgain);
  }} finally {{
    pumping = false;
  }}
  schedule();
}}

/// Something changed: run the tasks soon.
///
/// Anything that can make a waiting task runnable calls this — an event
/// listener, a settled promise, a host callback. Cheap and idempotent, so a
/// handler need not know whether the pump is already awake.
export function wake() {{
  if (residentExports === null) return;
  // Something happened on the host, which is precisely what a task waiting on
  // the host asked to be told about. This is `drive`'s "yield to the event
  // loop, then clear the flag", written for a driver that does not own the
  // loop and so cannot yield to it — the callback that woke us *is* the yield.
  //
  // Without this line a `wait_host` task is not merely slow, it is finished:
  // `step` skips a task carrying the flag and `schedule` gives it no deadline,
  // so nothing ever polls it again. The request completes, the response is
  // sitting in `REQUESTS`, and the program never looks.
  for (const t of TASKS) t.waitingOnHost = false;
  if (pumping) {{
    pumpAgain = true;
    return;
  }}
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => {{
    timer = null;
    pump();
  }}, 0);
}}

/// Hand a module to the resident driver, and switch to the real clock.
///
/// After this the program is alive: it has no more work to do until something
/// happens, and when something does, `wake` brings it back.
/// Call a Kite closure the host is holding.
///
/// `kite_invoke` is the module's own export and the only thing that can enter a
/// closure. It is looked up on whichever module was handed to a driver, which
/// is why a program using `js.func` has to be run rather than merely
/// instantiated.
let invokeExports = null;

/// Enter a Kite closure the host is holding.
///
/// One trampoline per handler shape, because `call_ref` needs the closure's
/// exact type and a two-argument handler is not the same type as a
/// one-argument one. `shape` is the index the module sent over with the
/// closure.
///
/// Every trampoline returns a reference — null when the Kite side answers with
/// nothing — so there is one calling convention here rather than one per
/// shape, and a comparator and a listener are wrapped by the same code.
function invoke(shape, handler, args) {{
  const trampoline = trampolineFor(shape);
  return trampoline(handler, ...args);
}}

function trampolineFor(shape) {{
  const trampoline =
    invokeExports === null ? null : invokeExports["kite_invoke_" + shape];
  if (typeof trampoline !== "function") {{
    throw new Error(
      "a handler fired before the module was given to a driver: call `run` or `resident`",
    );
  }}
  return trampoline;
}}

/// How many host values this shape's handler takes.
///
/// Read from the export's own arity rather than from a table generated beside
/// it: an exported Wasm function is a JavaScript function whose `length` is
/// the number of parameters it declares, and the first of those is the closure
/// itself. A table would be a second description of the same fact, free to
/// disagree with the module it describes.
function handlerArity(shape) {{
  return Math.max(0, trampolineFor(shape).length - 1);
}}

/// Make a module's exports reachable to handlers. Both drivers call it.
export function useExports(exports) {{
  invokeExports = exports;
}}

export function resident(exports) {{
  useExports(exports);
  realClock = true;
  residentExports = exports;
  pump();
}}

export async function instantiate(source = {wasm}) {{
  const bytes =
    source instanceof Uint8Array
      ? source
      : new Uint8Array(await (await fetch(source)).arrayBuffer());
{compile_step}
  // Before anything in the module can run. A driver sets this too, and used to
  // be the only thing that did — which was enough while a handler could only
  // be *entered* after one had started. It is not enough now: `js.func` reads
  // the trampoline's arity when it wraps the closure, and `main` may wrap one
  // long before a driver exists.
  useExports(instance.exports);
  return instance.exports;
}}

export async function run(source) {{
  const exports = await instantiate(source);
  if (typeof exports.main !== "function") {{
    throw new Error("this module has no `main`");
  }}
  const result = exports.main();
  // `main` returning is not the program ending: a task it started is still
  // the program's work, and dropping it would make `async` silently lossy.
  if (typeof exports.kite_poll === "function") {{
    await drive(exports);
  }}
  return result;
}}

"#,
        strings_section = strings_section,
        compile_step = compile_step,
        hosts = host_section,
        host_spread = if hosts.is_empty() {
            ""
        } else {
            "    ...HOSTS,\n"
        },
        wasm = json_string(wasm_path),
    ))
}

/// The `dom` group, which `std/dom` declares.
///
/// Supplied here for the same reason `net` is: a standard module whose one
/// boundary had to be wired up by hand would not be much of a standard module.
///
/// Elements cross as handles into a table, because a `str`, an `int` and a
/// `float` are what cross and a DOM node is none of those. Index 0 is
/// permanently empty, so a handle of 0 is "nothing found" and every function
/// tolerates it — a chain of lookups on a missing element does nothing rather
/// than throwing halfway along.
/// The `js` group: the whole of `std/js`, and the last JavaScript in the
/// project that anybody has to write.
///
/// **This block does not grow.** That is its point. One `extern` per browser
/// feature is faster and better typed, and the browser has thousands of them —
/// so the first API the standard library missed would send a user off to write
/// a host object of their own, in the language Kite exists to replace. What is
/// here is general enough that `std/dom`, and anything else a program needs,
/// is ordinary Kite written on top of it.
///
/// **Nothing throws across the boundary.** A JavaScript exception unwinding
/// through the Wasm frames aborts the scheduler and takes every running task
/// with it, so one mistyped method name would stop the program rather than fail
/// one call. Every entry that can throw is wrapped, and returns a marker object
/// the Kite side turns into an ordinary error.
///
/// The marker is made fresh each time rather than kept in a slot. A slot would
/// be an `errno`, and an `errno` is wrong here for a specific reason: a host
/// call can fire a Kite event handler synchronously in the middle of itself,
/// and that handler's own failures would overwrite the one being reported.
const JS_HOST: &str = r#"
if (HOSTS.js) {
  // What a throw becomes. `THREW` is a symbol so that no object a program
  // legitimately holds can be mistaken for one.
  const THREW = Symbol("kite.threw");
  const failure = (e) => ({
    [THREW]: e instanceof Error ? e.message : String(e),
  });
  const guard = (f) => {
    try {
      return f();
    } catch (e) {
      return failure(e);
    }
  };
  // `null` and `undefined` cannot be asked for a property, and `in` refuses a
  // primitive, so both are ruled out before the marker is looked for.
  const threw = (v) =>
    v !== null && typeof v === "object" && THREW in v;
  const root = () => (typeof globalThis === "undefined" ? {} : globalThis);
  // A constructor named on the global object. Kept to one place because
  // `js_new*` and `js_instance_of` must agree about what a name means.
  const ctor = (name) => {
    const c = root()[textOf(name)];
    if (typeof c !== "function") {
      throw new TypeError("`" + textOf(name) + "` is not a constructor");
    }
    return c;
  };

  HOSTS.js = {
    js_global: (name) => root()[textOf(name)],
    js_nothing: () => null,
    js_get: (target, name) => guard(() => target[textOf(name)]),
    // The value is returned so that a write has something to carry a failure
    // in. A strict-mode assignment to a read-only property throws, and so does
    // a setter of the page's own.
    js_set: (target, name, value) =>
      guard(() => {
        target[textOf(name)] = value;
        return null;
      }),
    js_at: (target, index) => guard(() => target[Number(index)]),

    js_call0: (t, name) => guard(() => t[textOf(name)]()),
    js_call1: (t, name, a) => guard(() => t[textOf(name)](a)),
    js_call2: (t, name, a, b) => guard(() => t[textOf(name)](a, b)),
    js_call3: (t, name, a, b, c) => guard(() => t[textOf(name)](a, b, c)),
    js_call4: (t, name, a, b, c, d) => guard(() => t[textOf(name)](a, b, c, d)),

    js_new0: (name) => guard(() => new (ctor(name))()),
    js_new1: (name, a) => guard(() => new (ctor(name))(a)),
    js_new2: (name, a, b) => guard(() => new (ctor(name))(a, b)),
    js_new3: (name, a, b, c) => guard(() => new (ctor(name))(a, b, c)),

    js_same: (a, b) => (a === b ? 1 : 0),
    // One question, not two. Which of `null` and `undefined` a property is
    // absent as is a JavaScript distinction with no Kite meaning.
    js_is_nothing: (v) => (v === null || v === undefined ? 1 : 0),
    js_instance_of: (v, name) => guard(() => v instanceof ctor(name)),
    // `typeof null` is `"object"`, which is a forty-year-old mistake and not
    // one to pass on: a caller asking what this is wants "nothing".
    js_kind_of: (v) => hostText(v === null ? "null" : typeof v),

    js_of_str: (value) => textOf(value),
    js_of_num: (value) => value,
    js_of_bool: (value) => value !== 0,
    // Guarded by `js_kind_of` on the Kite side, so these are only reached when
    // the value is already known to be of the right kind.
    js_as_str: (v) => hostText(v),
    js_as_num: (v) => v,
    js_as_bool: (v) => (v ? 1 : 0),
    // An `int` is an i64 and reaches here as a BigInt; a JavaScript number is
    // an f64. The conversion happens on this side because doing it in Kite
    // means a lossy cast, and the loss is the thing `of_int` refuses.
    // `js_is_safe_int` is asked first, so `BigInt` is only reached for a value
    // it can hold.
    js_of_int: (value) => Number(value),
    js_as_int: (v) => BigInt(v),
    js_is_safe_int: (v) =>
      typeof v === "number" && Number.isSafeInteger(v) ? 1 : 0,

    js_threw: (v) => (threw(v) ? 1 : 0),
    js_detail: (v) => hostText(threw(v) ? v[THREW] : ""),
  };
}
"#;

const DOM_HOST: &str = r#"
if (HOSTS.dom) {
  // Index 0 is the empty slot that means "not found", so the table starts with
  // a hole in it rather than reserving one by convention.
  const NODES = [null];
  const at = (id) => NODES[Number(id)] ?? null;
  const handle = (el) => (el ? BigInt(NODES.push(el) - 1) : 0n);
  HOSTS.dom = {
    query: (selector) => handle(document.querySelector(textOf(selector))),
    query_all: (selector) =>
      hostText(
        [...document.querySelectorAll(textOf(selector))].map((el) => handle(el)).join(","),
      ),
    create: (tag) => handle(document.createElement(textOf(tag))),
    append: (parent, child) => {
      const p = at(parent);
      const c = at(child);
      if (p && c) p.appendChild(c);
    },
    detach: (node) => {
      const el = at(node);
      if (el) el.remove();
    },
    get_text: (node) => hostText(at(node)?.textContent ?? ""),
    set_text: (node, body) => {
      const el = at(node);
      // `textContent`, never `innerHTML`. `std/dom` deliberately offers no way
      // to set markup, and the host must not quietly provide one.
      if (el) el.textContent = textOf(body);
    },
    get_attr: (node, name) => hostText(at(node)?.getAttribute(textOf(name)) ?? ""),
    set_attr: (node, name, value) => {
      const el = at(node);
      if (el) el.setAttribute(textOf(name), textOf(value));
    },
    drop_attr: (node, name) => {
      const el = at(node);
      if (el) el.removeAttribute(textOf(name));
    },
    set_style: (node, property, value) => {
      const el = at(node);
      // `setProperty` takes the CSS spelling — `background-color` — which is
      // what a caller writes in a stylesheet and what `std/dom` documents.
      if (el) el.style.setProperty(textOf(property), textOf(value));
    },
    set_class: (node, name, on) => {
      const el = at(node);
      if (el) el.classList.toggle(textOf(name), on !== 0);
    },
    has_class: (node, name) => (at(node)?.classList.contains(textOf(name)) ? 1 : 0),
    get_title: () => hostText(document.title),
    set_title: (body) => {
      document.title = textOf(body);
    },
  };
}
"#;

/// The `net` group, which `std/http` and `std/socket` declare.
///
/// Supplied here rather than left to the page because every environment Kite
/// runs in has `fetch`, and a standard library whose one boundary had to be
/// wired up by hand would not be much of a standard library. A program that
/// wants a different one calls `provide("net", …)` and replaces it.
///
/// A request is a handle rather than a promise: `str` and `int` are what cross
/// the boundary, so the module starts a request, is told to wait for the host,
/// and asks again. Streams — server-sent events and sockets — are the same
/// handle with a queue behind it, which is why they share one table: what
/// changes between them is which browser object fills the queue, not how a
/// program reads it.
const NET_HOST: &str = r#"
if (HOSTS.net) {
  const REQUESTS = [];
  const STREAMS = [];
  const parseHeaders = (text) => {
    const out = {};
    for (const line of String(text).split("\n")) {
      const at = line.indexOf(":");
      if (at > 0) out[line.slice(0, at).trim()] = line.slice(at + 1).trim();
    }
    return out;
  };
  HOSTS.net = {
    fetch_start: (method, url, body, headers, credentials, redirect) => {
      const request = { state: 0, status: 0, body: "", error: "", headers: null };
      const id = REQUESTS.push(request) - 1;
      const init = { method: textOf(method), headers: parseHeaders(textOf(headers)) };
      // `std/http` builds these from enums, so they are always one of the
      // words `fetch` knows. They are still only applied when non-empty, so a
      // host supplied by hand — `provide("net", …)` — that calls this with
      // four arguments behaves exactly as it did.
      const creds = credentials === undefined ? "" : textOf(credentials);
      if (creds !== "") init.credentials = creds;
      const follow = redirect === undefined ? "" : textOf(redirect);
      if (follow !== "") init.redirect = follow;
      if (textOf(body) !== "") init.body = textOf(body);
      // Every one of these settles a state a Kite task is waiting on, so every
      // one of them ends in `wake`. A host that changes what a program is
      // blocked on and does not say so leaves the program blocked on it
      // forever — the scheduler has no other way to find out.
      fetch(textOf(url), init)
        .then(async (response) => {
          request.status = response.status;
          request.headers = response.headers;
          request.body = await response.text();
          request.state = 1;
          wake();
        })
        .catch((e) => {
          request.error = String(e && e.message ? e.message : e);
          request.state = 2;
          wake();
        });
      return BigInt(id);
    },
    fetch_state: (id) => BigInt(REQUESTS[Number(id)].state),
    fetch_status: (id) => BigInt(REQUESTS[Number(id)].status),
    fetch_body: (id) => hostText(REQUESTS[Number(id)].body),
    fetch_header: (id, name) => {
      const headers = REQUESTS[Number(id)].headers;
      return hostText(headers ? headers.get(textOf(name)) ?? "" : "");
    },
    fetch_error: (id) => hostText(REQUESTS[Number(id)].error),

    // ---- server-sent events, which are EventSource ----
    //
    // `onerror` fires on every dropped connection, and EventSource reconnects
    // by itself — so only a source it has given up on (readyState 2) is a
    // failure the program is told about.
    sse_open: (url, names) => {
      const s = { state: 0, queue: [], taken: { name: "", id: "" }, source: null };
      const id = STREAMS.push(s) - 1;
      if (typeof EventSource === "undefined") {
        s.state = 2;
        return BigInt(id);
      }
      const source = new EventSource(textOf(url));
      s.source = source;
      s.take = (e) => {
        s.queue.push({ name: e.type || "message", id: e.lastEventId || "", data: e.data ?? "" });
        wake();
      };
      source.onopen = () => { s.state = 1; wake(); };
      source.onmessage = s.take;
      // Registered before anything can arrive, which is the whole reason the
      // names are given at open.
      for (const name of textOf(names).split("\n")) {
        if (name !== "") source.addEventListener(name, s.take);
      }
      source.onerror = () => { if (source.readyState === 2) { s.state = 2; wake(); } };
      return BigInt(id);
    },
    sse_state: (id) => BigInt(STREAMS[Number(id)].state),
    sse_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    sse_next: (id) => {
      const s = STREAMS[Number(id)];
      const e = s.queue.shift();
      if (e === undefined) {
        s.taken = { name: "", id: "" };
        return hostText("");
      }
      s.taken = { name: e.name, id: e.id };
      return hostText(e.data);
    },
    sse_event_name: (id) => hostText(STREAMS[Number(id)].taken.name),
    sse_event_id: (id) => hostText(STREAMS[Number(id)].taken.id),
    sse_listen: (id, name) => {
      const s = STREAMS[Number(id)];
      if (s.source) s.source.addEventListener(textOf(name), s.take);
      return 1n;
    },
    sse_close: (id) => {
      const s = STREAMS[Number(id)];
      if (s.source) s.source.close();
      s.state = 3;
      return 1n;
    },

    // ---- sockets, which are WebSocket ----
    //
    // Text frames only: a `str` is what crosses this boundary, and handing a
    // program an empty string where a binary frame arrived would be inventing
    // a message that was never sent.
    socket_open: (url) => {
      const s = { state: 0, queue: [], error: "", socket: null };
      const id = STREAMS.push(s) - 1;
      if (typeof WebSocket === "undefined") {
        s.state = 2;
        s.error = "this host has no WebSocket";
        return BigInt(id);
      }
      let socket;
      try {
        socket = new WebSocket(textOf(url));
      } catch (e) {
        s.state = 2;
        s.error = String(e && e.message ? e.message : e);
        return BigInt(id);
      }
      s.socket = socket;
      socket.onopen = () => { s.state = 1; wake(); };
      socket.onmessage = (e) => {
        if (typeof e.data === "string") s.queue.push(e.data);
        wake();
      };
      socket.onerror = () => {
        if (s.state !== 3) {
          s.state = 2;
          if (s.error === "") s.error = "the connection failed";
        }
        wake();
      };
      socket.onclose = () => {
        if (s.state !== 2) s.state = 3;
        wake();
      };
      return BigInt(id);
    },
    socket_state: (id) => BigInt(STREAMS[Number(id)].state),
    socket_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    socket_next: (id) => {
      const s = STREAMS[Number(id)];
      const message = s.queue.shift();
      return hostText(message === undefined ? "" : message);
    },
    socket_send: (id, message) => {
      const s = STREAMS[Number(id)];
      if (s.state !== 1 || !s.socket) return 0n;
      s.socket.send(textOf(message));
      return 1n;
    },
    socket_error: (id) => hostText(STREAMS[Number(id)].error),
    socket_close: (id) => {
      const s = STREAMS[Number(id)];
      if (s.socket) s.socket.close();
      s.state = 3;
      return 1n;
    },
  };
}
"#;

/// The `crypto` group, which `std/crypto` declares.
///
/// Bindings, not implementations: every one of these is the host's own
/// primitive. WebCrypto is asynchronous, so a digest starts and is polled —
/// what crosses the boundary is a handle and hex text, because that is what
/// crosses it at all.
/// The `audio` group.
///
/// Supplied here for the same reason `net` is: every browser has WebAudio, and
/// a program that had to be handed an oscillator by its page would not be able
/// to make a sound on its own. A program that wants a different one calls
/// `provide("audio", …)`.
///
/// The boundary is three calls and carries nothing but numbers. A *sample
/// buffer* would be the obvious thing to send and is exactly what cannot
/// cross — only `str`, `int` and `float` do — so what crosses is a **note**:
/// a frequency, when to start it, how long to hold it, how loud. The music is
/// the program's; the oscillator is the host's. That division is also why this
/// stays small enough to be honest about: scheduling is WebAudio's own, sample
/// accurate and running on the audio thread, so a frame that arrives late does
/// not make the music stutter.
const AUDIO_HOST: &str = r#"
if (HOSTS.audio) {
  let context = null;
  // The one recorded track in play, if a program asked for one.
  let element = null;
  let master = null;
  let voices = [];
  const ready = () => {
    if (context === null) {
      const Ctor = globalThis.AudioContext || globalThis.webkitAudioContext;
      if (!Ctor) return null;
      context = new Ctor();
      master = context.createGain();
      master.gain.value = 0.9;
      master.connect(context.destination);
    }
    // A browser starts the context suspended until a gesture; every call
    // nudges it, and the one that follows a click is the one that succeeds.
    if (context.state === "suspended") context.resume();
    return context;
  };
  HOSTS.audio = {
    note: (frequency, delay, seconds, gain) => {
      const ctx = ready();
      if (ctx === null) return;
      const at = ctx.currentTime + Math.max(0, delay);
      const osc = ctx.createOscillator();
      const env = ctx.createGain();
      osc.type = "triangle";
      osc.frequency.value = frequency;
      // A note with hard edges clicks. The envelope is the shortest one that
      // does not: a few milliseconds up, and an exponential decay down, which
      // is what a plucked string does and what an ear expects.
      env.gain.setValueAtTime(0.0001, at);
      env.gain.exponentialRampToValueAtTime(Math.max(0.0001, gain), at + 0.02);
      env.gain.exponentialRampToValueAtTime(0.0001, at + Math.max(0.05, seconds));
      osc.connect(env);
      env.connect(master);
      osc.start(at);
      osc.stop(at + Math.max(0.05, seconds) + 0.05);
      voices.push(osc);
      // Voices that have already finished are dropped when the list grows,
      // rather than on a timer nobody would cancel.
      if (voices.length > 256) voices = voices.slice(-128);
    },
    silence: () => {
      for (const osc of voices) {
        try {
          osc.stop();
        } catch (e) {
          // Already stopped, which is not an error worth reporting.
        }
      }
      voices = [];
    },
    // Whether a sound can currently be made. A page that has had no gesture
    // yet answers false, so a program can say so rather than appearing broken.
    awake: () => {
      const ctx = ready();
      return ctx !== null && ctx.state === "running";
    },

    // ---- recorded audio ----
    //
    // A second way to make a sound, for programs that have a file rather than
    // a tune. It is deliberately not the same mechanism: an oscillator is fed
    // notes and a file is played, and pretending one is the other would mean
    // decoding audio in Kite to hand back samples the boundary cannot carry.
    //
    // The element is the clock. A program that asked the host to start a file
    // and then counted frames itself would drift — the audio runs on its own
    // clock, and `at()` is how a program reads that clock rather than guessing
    // at it.
    load: (url) => {
      const next = textOf(url);
      if (element !== null && element.dataset.src === next) return;
      if (element !== null) element.pause();
      const el = new Audio();
      el.dataset.src = next;
      element = el;
      // Streaming first, and the whole file only if it turns out to be needed.
      //
      // A media element seeks by asking the server for the bytes at the new
      // position, over a `Range` request. A host that answers `200` with the
      // whole body instead leaves the element unable to seek *at all*: its
      // `seekable` range stays empty and every `currentTime` assignment
      // reverts to zero, however much has been buffered. Buffering is not
      // seekability, which is what made this look fixed once when it was not.
      //
      // The site this deploys to serves ranges now, so the ordinary path is
      // the native one: play the URL, stream progressively, seek against the
      // server. Nothing is downloaded twice and nothing waits.
      //
      // Where a host does refuse — a plain static server, someone serving the
      // directory locally — the element says so by reporting no seekable range
      // once its metadata is in. Only then are the bytes fetched and handed
      // over as a blob, which the browser can range over itself. The element
      // is put back exactly where it was, still playing if it was playing.
      el.src = next;
      el.preload = "auto";
      el.addEventListener("loadedmetadata", () => {
        if (element !== el || el.seekable.length > 0) return;
        fetch(next)
          .then((r) => r.blob())
          .then((b) => {
            // A different track may have been chosen while the bytes came.
            if (element !== el) return;
            const at = el.currentTime;
            const playing = !el.paused;
            el.src = URL.createObjectURL(b);
            el.addEventListener("loadedmetadata", () => {
              const want = Number(el.dataset.at || at || 0);
              if (want > 0) {
                try {
                  el.currentTime = Math.min(want, el.duration);
                } catch (e) {
                  // A position the element will not take. It does not move,
                  // which the program sees on its next frame.
                }
              }
              if (playing || el.dataset.wanted === "1") {
                const p = el.play();
                if (p && p.catch) p.catch(() => {});
              }
            }, { once: true });
          })
          .catch(() => {
            // No bytes: the element goes on streaming from the URL, where
            // forward playback works and seeking does not. Better than silence.
          });
      }, { once: true });
    },
    start: () => {
      if (element === null) return;
      // Rejected when no gesture has happened yet, which is not an error worth
      // trapping over — the next click will succeed.
      const played = element.play();
      if (played && played.catch) played.catch(() => {});
    },
    pause: () => {
      if (element !== null) element.pause();
    },
    seek: (seconds) => {
      if (element === null) return;
      // Before the metadata arrives the duration is NaN, and assigning a time
      // past it throws.
      const limit = Number.isFinite(element.duration) ? element.duration : seconds;
      const want = Math.max(0, Math.min(seconds, limit));
      try {
        element.currentTime = want;
      } catch (e) {
        // A seek the element is not ready for. The position simply does not
        // move, which the program sees on its next frame — better than a
        // trap the program has no way to handle.
      }
    },
    at: () => (element === null ? 0 : element.currentTime),
    // Zero until the metadata has loaded, which a program reads as "not known
    // yet" rather than as "an empty file".
    length: () => {
      if (element === null) return 0;
      return Number.isFinite(element.duration) ? element.duration : 0;
    },
    ended: () => element !== null && element.ended,
  };
}
"#;

const CRYPTO_HOST: &str = r#"
if (HOSTS.crypto) {
  const WORK = [];
  // Keys live here, on this side of the boundary. What the program holds is
  // an index into this array; the material itself never crosses. A key pair
  // is stored whole, and the private half is created non-extractable, so even
  // this file could not export it.
  const KEYS = [];
  const subtle = globalThis.crypto && globalThis.crypto.subtle;
  const hex = (buffer) =>
    [...new Uint8Array(buffer)].map((b) => b.toString(16).padStart(2, "0")).join("");
  const unhex = (text) =>
    new Uint8Array((String(text).match(/../g) ?? []).map((b) => parseInt(b, 16)));
  const start = (promise) => {
    const work = { state: 0, result: "", error: "" };
    const id = WORK.push(work) - 1;
    promise
      .then((value) => {
        work.result = value;
        work.state = 1;
        wake();
      })
      .catch((e) => {
        work.error = String(e && e.message ? e.message : e);
        work.state = 2;
        wake();
      });
    return BigInt(id);
  };
  const bytes = (text) => new TextEncoder().encode(text);
  const keep = (key) => String(KEYS.push(key) - 1);
  // WebCrypto reports a missing algorithm as NotSupportedError, whose message
  // says nothing. Saying which algorithm, and where it does exist, beats
  // that — and nothing weaker is substituted.
  const unsupported = (name) => (e) => {
    if (e && e.name === "NotSupportedError") {
      throw new Error(`this host's WebCrypto has no ${name} — Node 24 and current browsers do`);
    }
    throw e;
  };
  HOSTS.crypto = {
    random_hex: (count) => {
      const out = new Uint8Array(Number(count));
      globalThis.crypto.getRandomValues(out);
      return hostText(hex(out.buffer));
    },
    digest_start: (algorithm, text) =>
      start(subtle.digest(textOf(algorithm), bytes(textOf(text))).then(hex)),
    hmac_start: (algorithm, key, text) =>
      start(
        subtle
          .importKey(
            "raw",
            bytes(textOf(key)),
            { name: "HMAC", hash: textOf(algorithm) },
            false,
            [
            "sign",
            ],
          )
          .then((k) => subtle.sign("HMAC", k, bytes(textOf(text))))
          .then(hex),
      ),
    derive_start: (password, salt, iterations) =>
      start(
        subtle
          .importKey("raw", bytes(textOf(password)), "PBKDF2", false, ["deriveBits"])
          .then((k) =>
            subtle.deriveBits(
              {
                name: "PBKDF2",
                salt: unhex(textOf(salt)),
                iterations: Number(iterations),
                hash: "SHA-256",
              },
              k,
              256,
            ),
          )
          .then(hex),
      ),
    key_generate_start: (kind) => {
      const name = textOf(kind);
      if (name === "AES-GCM") {
        return start(
          subtle.generateKey({ name, length: 256 }, false, ["encrypt", "decrypt"]).then(keep),
        );
      }
      const usages = name === "Ed25519" ? ["sign", "verify"] : ["deriveBits"];
      return start(subtle.generateKey(name, false, usages).catch(unsupported(name)).then(keep));
    },
    key_import_start: (material) => {
      const text = textOf(material);
      if (!/^[0-9a-f]{64}$/i.test(text)) {
        return start(Promise.reject(new Error("a key is 32 bytes — 64 hex characters")));
      }
      return start(
        subtle.importKey("raw", unhex(text), "AES-GCM", false, ["encrypt", "decrypt"]).then(keep),
      );
    },
    key_public_start: (key) =>
      start(
        Promise.resolve(KEYS[Number(key)])
          .then((pair) => subtle.exportKey("raw", pair.publicKey))
          .then(hex),
      ),
    seal_start: (key, nonce, plaintext) =>
      start(
        subtle
          .encrypt(
            { name: "AES-GCM", iv: unhex(textOf(nonce)) },
            KEYS[Number(key)],
            bytes(textOf(plaintext)),
          )
          .then(hex),
      ),
    open_start: (key, nonce, cipher) =>
      start(
        subtle
          .decrypt(
            { name: "AES-GCM", iv: unhex(textOf(nonce)) },
            KEYS[Number(key)],
            unhex(textOf(cipher)),
          )
          .then((clear) => new TextDecoder().decode(clear))
          .catch(() => {
            // GCM authenticates before it decrypts, and reports nothing more
            // specific — which is right: an oracle that said *what* failed
            // would be worth attacking.
            throw new Error("the sealed text was altered, or sealed under a different key");
          }),
      ),
    sign_start: (key, text) =>
      start(
        subtle.sign("Ed25519", KEYS[Number(key)].privateKey, bytes(textOf(text))).then(hex),
      ),
    verify_start: (pub, text, signature) =>
      start(
        subtle
          .importKey("raw", unhex(textOf(pub)), "Ed25519", false, ["verify"])
          .catch(unsupported("Ed25519"))
          .then((k) =>
            subtle.verify(
              "Ed25519",
              k,
              unhex(textOf(signature)),
              bytes(textOf(text)),
            ),
          )
          .then((ok) => (ok ? "true" : "false")),
      ),
    // Agreement and derivation in one step, so the raw X25519 output exists
    // only inside this chain: it has structure an attacker can use, and HKDF
    // is what turns it into a key that does not.
    agree_start: (key, pub) =>
      start(
        subtle
          .importKey("raw", unhex(textOf(pub)), "X25519", false, [])
          .catch(unsupported("X25519"))
          .then((theirs) =>
            subtle.deriveBits({ name: "X25519", public: theirs }, KEYS[Number(key)].privateKey, 256),
          )
          .then((shared) => subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]))
          .then((k) =>
            subtle.deriveKey(
              { name: "HKDF", hash: "SHA-256", salt: new Uint8Array(0), info: bytes("kite crypto.agree v1") },
              k,
              { name: "AES-GCM", length: 256 },
              false,
              ["encrypt", "decrypt"],
            ),
          )
          .then(keep),
      ),
    work_state: (id) => BigInt(WORK[Number(id)].state),
    work_result: (id) => hostText(WORK[Number(id)].result),
    work_error: (id) => hostText(WORK[Number(id)].error),
    // Same time whichever way it goes: every byte is compared, and the length
    // is folded in rather than returned early on.
    constant_time_equal: (a, b) => {
      const x = textOf(a);
      const y = textOf(b);
      let diff = x.length ^ y.length;
      for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
        diff |= (x.charCodeAt(i % x.length) || 0) ^ (y.charCodeAt(i % y.length) || 0);
      }
      return diff === 0 ? 1 : 0;
    },
  };
}
"#;

/// A JavaScript object key, quoted only when it has to be.
fn json_ident(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if plain {
        name.to_string()
    } else {
        json_string(name)
    }
}

/// A page that runs the module, with a control for which renderer draws.
///
/// The same compiled module backs all three: the program calls `draw.rect` and
/// `draw.text` and cannot tell where they went. Being able to switch between
/// them in one page is the clearest evidence that neither renderer is deciding
/// anything about layout.
/// A page that loads the module and runs it.
///
/// Deliberately close to nothing. `kitec build` emits an ES module, and the
/// point of the web target is that the module goes into a page somebody else
/// wrote — served by anything, styled by any stylesheet, with Kite owning the
/// parts that need logic. A generated page that brought its own layout,
/// its own chrome and its own idea of what an application looks like would be
/// demonstrating the opposite.
///
/// So this is a loader and two elements: somewhere for printed output to go,
/// and a canvas for a program that draws. Everything else a real page has is
/// the real page's business.
pub fn generate_page(title: &str) -> String {
    strip_comments(&format!(
        r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  html, body {{ height: 100%; margin: 0; }}
  body {{ background: #0b0d10; color: #c9d1dc;
         font: 13px ui-monospace, SFMono-Regular, Menlo, monospace;
         display: flex; flex-direction: column; }}
  /* Sized by the page, not by the program. A canvas is an element here like
     any other, so it takes its box from the stylesheet — which is the whole
     difference from what this file used to generate. */
  canvas {{ flex: 1; min-height: 0; width: 100%; display: block; }}
  pre {{ margin: 0; padding: 12px; white-space: pre-wrap; flex: none;
         max-height: 40vh; overflow: auto; }}
</style>

<canvas id="stage"></canvas>
<pre id="out"></pre>

<script type="module">
  import {{ instantiate, setRenderer, setMeasure, setLineHeight, fontMeasure,
            fontLineHeight, canvasRenderer, resident }} from "./app.js";

  const out = document.getElementById("out");
  const stage = document.getElementById("stage");

  // Measure in the font that will be drawn, before anything draws.
  setMeasure(fontMeasure());
  setLineHeight(fontLineHeight());

  // A canvas at device resolution, described to CSS at layout resolution.
  const scale = window.devicePixelRatio || 1;
  const box = stage.getBoundingClientRect();
  stage.width = Math.max(1, Math.round(box.width * scale));
  stage.height = Math.max(1, Math.round(box.height * scale));
  const ctx = stage.getContext("2d");
  ctx.scale(scale, scale);
  setRenderer(canvasRenderer(ctx));

  const exports = await instantiate("./app.wasm");
  if (typeof exports.main !== "function") {{
    out.textContent = "this module has no `main`";
  }} else {{
    exports.main();
    // `main` returning is not the program ending, and in a page the program
    // does not end at all: `resident` keeps the tasks alive on a real clock,
    // costing nothing while there is nothing to do.
    if (typeof exports.kite_poll === "function") resident(exports);
  }}
</script>
"#,
        title = title
    ))
}

/// Encode a Rust string as a JavaScript string literal.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escape control characters and the line separators that are legal
            // in JSON but not in a JavaScript string literal.
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The typed door for JavaScript and TypeScript: `api.js` and `api.d.ts`.
///
/// **This is the phase's whole point, and it is small because most of it was
/// already there.** A `pub fn` has been a real Wasm export all along, so
/// JavaScript could always call one — it just had to know that an `int` arrives
/// as a `BigInt` and that a `str` crosses through the module's scalar-array
/// conversion functions. That is a calling convention, and a calling
/// convention nobody wrote down is a reason not to adopt something.
///
/// So the compiler writes the wrapper instead. A TypeScript project imports
/// `api.js`, type-checks against `api.d.ts`, and never learns any of it.
///
/// That matters more than it looks. **TypeScript won by being adoptable one
/// file at a time** — not by being better in the abstract, but by letting a
/// team rename one file and leave four hundred alone. This is that shape: swap
/// one hot module for Kite, keep everything else.
///
/// A function whose signature this cannot describe is **left out of both
/// files** rather than described wrongly. Structs, enums, slices and maps have
/// no representation JavaScript can hold yet, and a `.d.ts` that promised
/// otherwise would type-check code that cannot work.
pub fn generate_api(api: &[crate::Export], wasm_path: &str) -> (String, String) {
    // A module-qualified name is not a JavaScript identifier, and `export
    // function prelude.contains(…)` is a syntax error that takes the whole
    // file with it — so a page importing this got nothing, not a missing
    // function. Those names come from a `use`d module rather than from the
    // program, and a program's public interface is its own `pub fn`s: the
    // standard library is not part of the door this file opens.
    let own = |e: &&crate::Export| !e.name.contains('.');
    let describable: Vec<&crate::Export> = api
        .iter()
        .filter(|e| e.name != "main")
        .filter(own)
        .filter(|e| {
            e.params.iter().all(|(_, t)| ts_type(t).is_some())
                && e.ret
                    .as_deref()
                    .map(|r| ts_type(r).is_some())
                    .unwrap_or(true)
        })
        .collect();

    let mut js = String::from(
        "// Generated by kitec. Do not edit.\n\
         //\n\
         // The typed door into a Kite module. Every `pub fn` is here with its\n\
         // conversions already applied: an `int` is a BigInt on the wire, and a\n\
         // `str` crosses through the module's Unicode-scalar conversion helpers.\n\
         // A caller sees ordinary JavaScript values.\n\n\
         import { instantiate, str, text } from \"./app.js\";\n\n\
         let module = null;\n\n\
         /// Load the module. Call this once before anything else here.\n\
         export async function load(source = ",
    );
    js.push_str(&json_string(wasm_path));
    js.push_str(
        ") {\n  module = await instantiate(source);\n  return module;\n}\n\n\
         const ready = () => {\n\
         \x20 if (module === null) {\n\
         \x20   throw new Error(\"call `await load()` before calling into the module\");\n\
         \x20 }\n\
         \x20 return module;\n\
         };\n",
    );

    let mut dts = String::from(
        "// Generated by kitec. Do not edit.\n\
         //\n\
         // A Kite module, described for TypeScript. `int` is 64-bit, so it is a\n\
         // `bigint` here rather than a `number` that would silently lose the top\n\
         // eleven bits.\n\n\
         export declare function load(source?: string | Uint8Array): Promise<unknown>;\n",
    );

    for e in &describable {
        // A JavaScript reserved word would be a syntax error in the wrapper,
        // and a Kite parameter may legitimately be called `new` or `class`.
        let names: Vec<String> = e
            .params
            .iter()
            .enumerate()
            .map(|(i, (n, _))| {
                if reserved(n) {
                    format!("{}_{}", n, i)
                } else {
                    n.clone()
                }
            })
            .collect();
        // Two conversions, and both were found by a test rather than by
        // reasoning. A `str` crosses through the module's scalar-array
        // conversion helpers. A `bool` crosses as an i32, so a JavaScript
        // caller passing `true` would send `undefined` and a returned `false`
        // would arrive as `0` — which the `.d.ts` says is a `boolean`, and a
        // wrapper whose types are a lie is worse than no wrapper.
        let args: Vec<String> = e
            .params
            .iter()
            .zip(&names)
            .map(|((_, ty), n)| match ty.as_str() {
                "str" => format!("str({})", n),
                "bool" => format!("{} ? 1 : 0", n),
                _ => n.clone(),
            })
            .collect();
        let call = format!("ready().{}({})", e.name, args.join(", "));
        let body = match e.ret.as_deref() {
            Some("str") => format!("text({})", call),
            Some("bool") => format!("({}) !== 0", call),
            _ => call,
        };
        js.push_str(&format!(
            "\nexport function {}({}) {{\n  return {};\n}}\n",
            e.name,
            names.join(", "),
            body
        ));

        let sig: Vec<String> = e
            .params
            .iter()
            .zip(&names)
            .map(|((_, ty), n)| format!("{}: {}", n, ts_type(ty).unwrap_or("unknown")))
            .collect();
        dts.push_str(&format!(
            "export declare function {}({}): {};\n",
            e.name,
            sig.join(", "),
            e.ret.as_deref().and_then(ts_type).unwrap_or("void")
        ));
    }

    let skipped: Vec<&str> = api
        .iter()
        .filter(|e| e.name != "main")
        .filter(own)
        .filter(|e| !describable.iter().any(|d| d.name == e.name))
        .map(|e| e.name.as_str())
        .collect();
    if !skipped.is_empty() {
        let note = format!(
            "\n// Left out, because these take or answer with a type JavaScript has no\n\
             // representation for yet: {}.\n\
             // The module still exports them; describing them wrongly would be worse\n\
             // than not describing them.\n",
            skipped.join(", ")
        );
        js.push_str(&note);
        dts.push_str(&note);
    }

    (js, dts)
}

/// Words JavaScript will not accept as a parameter name.
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "new"
            | "class"
            | "function"
            | "var"
            | "let"
            | "const"
            | "return"
            | "typeof"
            | "await"
            | "yield"
            | "this"
            | "null"
            | "true"
            | "false"
            | "default"
            | "import"
            | "export"
            | "delete"
            | "void"
            | "in"
            | "of"
            | "with"
    )
}

/// How a Kite type is spelled in TypeScript, or `None` when it cannot be.
fn ts_type(kite: &str) -> Option<&'static str> {
    Some(match kite {
        // 64-bit, so a `bigint`. A `number` would hold it up to 2^53 and lose
        // the rest silently, which is the kind of edge that reaches production.
        "int" => "bigint",
        "float" => "number",
        "bool" => "boolean",
        "str" => "string",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped_for_javascript() {
        assert_eq!(json_string("a\"b"), r#""a\"b""#);
        assert_eq!(json_string("a\nb"), r#""a\nb""#);
        assert_eq!(json_string("a\\b"), r#""a\\b""#);
        assert_eq!(json_string("tab\there"), r#""tab\there""#);
    }

    /// U+2028 and U+2029 are valid JSON but terminate a JavaScript string
    /// literal, which is a classic way to generate broken glue.
    #[test]
    fn line_separators_are_escaped() {
        assert_eq!(json_string("a\u{2028}b"), "\"a\\u2028b\"");
        assert_eq!(json_string("a\u{2029}b"), "\"a\\u2029b\"");
    }

    #[test]
    fn the_glue_has_the_bounded_string_bridge() {
        let g = generate_glue("app.wasm");
        assert!(g.contains("new WebAssembly.Memory({ initial: 1, maximum: 1 })"));
        assert!(g.contains("text_len: textLength"));
        assert!(g.contains(r#"stringRuntime()["$kite.str.from_host"]"#));
        assert!(g.contains(r#""app.wasm""#));
        assert!(g.contains("export async function run"));
    }
}
