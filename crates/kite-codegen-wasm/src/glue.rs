//! JavaScript glue generation.
//!
//! There is no standardised way for Wasm to call a Web API without JavaScript
//! glue — no Web IDL bindings proposal has landed, and none is imminent. Kite
//! therefore declares the host boundary explicitly and *generates* the glue
//! from that declaration, so the two cannot drift apart.
//!
//! The generated module is deliberately tiny and has no dependencies.

use crate::Strings;

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

pub fn generate_glue(strings: &[String], wasm_path: &str) -> String {
    generate_glue_with_hosts(strings, wasm_path, &[])
}

/// The glue, including the host groups a program declared.
///
/// Every `@host("group") extern fn` becomes an entry the host must supply.
/// The generated module declares each group as an object a page can fill in
/// with `provide("group", { … })` before running — and a call to something
/// nobody supplied fails saying which declaration it was, rather than doing
/// nothing.
pub fn generate_glue_with_hosts(
    strings: &[String],
    wasm_path: &str,
    hosts: &[crate::HostImport],
) -> String {
    generate_glue_for(strings, wasm_path, hosts, Strings::Table)
}

/// The glue for a module compiled with a chosen string representation.
///
/// The two differ in one place and nowhere else: what a `str` value *is*.
/// Every read of one goes through `S`, and every JavaScript string that
/// becomes one goes through `intern` — so the thirty-odd host functions below
/// are written once and do not know which module they are answering.
pub fn generate_glue_for(
    strings: &[String],
    wasm_path: &str,
    hosts: &[crate::HostImport],
    mode: Strings,
) -> String {
    let table = strings
        .iter()
        .map(|s| format!("  {},", json_string(s)))
        .collect::<Vec<_>>()
        .join("\n");
    let strings_section = match mode {
        Strings::Table => format!(
            r#"// String constants. A `str` is an index into this table, which is why the
// module needs no linear memory. Concatenation appends, so the table grows.
const STRINGS = [
{table}
];

function intern(s) {{
  const existing = STRINGS.indexOf(s);
  if (existing !== -1) return existing;
  return STRINGS.push(s) - 1;
}}

// The string a `str` value stands for.
const S = (i) => STRINGS[i];

/// A `str` for an exported function to take.
///
/// A `str` is an index into the table above, not a pointer and not a JavaScript
/// string. Passing a JavaScript string to an export does not fail — the Wasm
/// JS API runs `ToNumber` on it, which gives `NaN`, which becomes index 0 —
/// so the program reads whichever string happens to be first. Anything calling
/// an export with text has to come through here.
export function str(s) {{
  return intern(String(s));
}}

/// The text a `str` stands for, for a value an export returned.
export function text(i) {{
  return S(i);
}}
"#
        ),
        Strings::Builtins => String::from(
            r#"// A `str` *is* a JavaScript string here.
//
// The module holds an `externref`; its constants are imported globals the
// engine synthesised from the literals themselves; `+` and `==` on strings are
// the JS String Builtins, compiled to intrinsics rather than to calls into
// this file. There is no table, nothing to intern, and nothing to look up when
// a string crosses to a DOM API — the value already is the string.
//
// What is *not* a builtin is deliberate. `length`, `charCodeAt` and
// `substring` index by UTF-16 code unit and Kite counts characters, so using
// them would make this backend disagree with the bytecode VM the moment a
// program held an astral character. Those stay host calls below, where they
// can count code points.
const intern = (s) => s;
const S = (s) => s;

/// A `str` for an exported function to take. Here that is the string itself,
/// and this exists so a caller need not know which representation it got.
export function str(s) {
  return String(s);
}

/// The text a `str` stands for, for a value an export returned.
export function text(s) {
  return s;
}
"#,
        ),
    };
    // The builtins are a *compile* option, not an instantiate one, so the two
    // steps have to be taken separately here. An engine that does not know the
    // option ignores it and then fails to find `wasm:js-string`, which is a
    // link error nobody could place — so it is checked for and named.
    let compile_step = match mode {
        Strings::Table => String::from(
            "  const { instance } = await WebAssembly.instantiate(bytes, imports());",
        ),
        Strings::Builtins => String::from(
            "  // An engine without the builtins ignores the options and then cannot\n\
            \x20 // find `wasm:js-string`, which is a link error nobody could place. The\n\
            \x20 // failure is caught and named rather than left as one.\n\
            \x20 let instance;\n\
            \x20 try {\n\
            \x20   const module = await WebAssembly.compile(bytes, {\n\
            \x20     builtins: ['js-string'],\n\
            \x20     importedStringConstants: 'kite:strings',\n\
            \x20   });\n\
            \x20   // Given a Module rather than bytes, this answers with the Instance\n\
            \x20   // itself — there is no `{ instance, module }` pair to take apart.\n\
            \x20   instance = await WebAssembly.instantiate(module, imports());\n\
            \x20 } catch (e) {\n\
            \x20   throw new Error(\n\
            \x20     'this module was built with --js-strings and needs the JS String ' +\n\
            \x20       'Builtins, which this engine does not have: ' + e.message + '. ' +\n\
            \x20       'Use a current browser or Node 23+, or build without the flag.',\n\
            \x20   );\n\
            \x20 }",
        ),
    };
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
  // reads. `kite check --a11y` parses these lines.
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
  // Writing out a frame is the whole point of this renderer, so it has no
  // damage path: a partial transcript would not be a transcript. It says so
  // by declaring only `rebuild`, which is what the frame loop falls back to.
  rebuild: (calls) => replay(calls, textRenderer),
}};

// Absolutely-positioned elements under one container. The layout has already
// decided every position, so the container needs no layout of its own.
//
// The elements are **retained**: one per drawing call, in call order, kept
// between frames. A frame that differs from the last in one label therefore
// costs one `textContent` write rather than a rebuilt subtree — and the
// program that produced it still wrote an ordinary `view` that describes the
// whole picture.
export function domRenderer(container) {{
  container.style.position = 'relative';
  container.replaceChildren();

  // A clip becomes a nested element with `overflow: hidden`, and everything
  // drawn until the matching `unclip` goes inside it. Its children are
  // positioned relative to it, so the origin has to be subtracted — which is
  // the whole difference between this and the canvas renderer, where a clip is
  // a path and coordinates never move.
  let host = container;
  let originX = 0;
  let originY = 0;
  let stack = [];
  // One node per call, in call order. This is the retained scene graph.
  let nodes = [];
  let index = 0;

  const opaque = (el) => {{
    el.style.opacity = alpha === 1 ? '' : String(alpha);
    return el;
  }};

  const place = (el, x, y) => {{
    el.style.position = 'absolute';
    el.style.left = x - originX + 'px';
    el.style.top = y - originY + 'px';
    // Every call goes through here, so this is the one place the alpha in
    // force has to be applied.
    return opaque(el);
  }};
  // `div` unless a call needs a different element. The tag is a parameter
  // rather than always a div because an `<img>` cannot be faked with one: a
  // background image would lose the browser's decoding, its cache and any
  // description a screen reader could read.
  const take = (tag = 'div') => {{
    const el = document.createElement(tag);
    nodes[index] = el;
    index += 1;
    host.appendChild(el);
    return el;
  }};

  const renderer = {{
    rect: (x, y, w, h, colour) => {{
      const el = place(take(), x, y);
      // A filled rectangle is decoration until something says otherwise, and
      // a screen reader announcing "group" for every box is worse than
      // silence.
      el.setAttribute('aria-hidden', 'true');
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.background = hex(colour);
    }},
    rrect: (x, y, w, h, r, colour) => {{
      const el = place(take(), x, y);
      el.setAttribute('aria-hidden', 'true');
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.background = hex(colour);
      // The one thing a rounded rectangle needs that a square one does not.
      el.style.borderRadius = r + 'px';
    }},
    // The ring between two rounded rectangles, which is what a CSS border is:
    // `box-sizing: border-box` makes the border eat into the given size rather
    // than adding to it, so the outer edge is exactly the rectangle asked for.
    drrect: (x, y, w, h, r, width, colour) => {{
      const el = place(take(), x, y);
      el.setAttribute('aria-hidden', 'true');
      el.style.boxSizing = 'border-box';
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.background = 'transparent';
      el.style.border = width + 'px solid ' + hex(colour);
      el.style.borderRadius = r + 'px';
    }},
    // A real `<input>`: the browser's caret, selection, IME, autofill and the
    // phone's keyboard — none of which can be drawn, and all of which a field
    // made of glyphs has to fake or do without.
    field: (x, y, w, h, value, hint, colour, id, multiline) => {{
      // Claim a node slot, even though the element this draws is not in it.
      //
      // `patch` pairs `nodes[i]` with `calls[i]` by index — that is the whole
      // basis of the retained scene graph — so a call that claimed no slot
      // hands every element after it the wrong call. The symptom is not
      // subtle: chips drew at the transport's coordinates and the list drew
      // over the app bar. The real input lives outside the container, so what
      // is claimed here is an empty slot that `patch` skips.
      nodes[index] = null;
      index += 1;
      const el = fieldEl(id, multiline);
      fieldsSeen.add(id);
      fieldBoxes.set(id, [x - originX, y - originY, w, h]);
      const box = container.getBoundingClientRect();
      el.style.left = box.left + (x - originX) + 'px';
      el.style.top = box.top + (y - originY) + 'px';
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.color = hex(colour);
      el.style.font = fontCss();
      el.style.lineHeight = lineHeight() + 'px';
      el.style.opacity = alpha === 1 ? '' : String(alpha);
      el.placeholder = hint;
      el.style.display = '';
      // Assigning `value` moves the caret to the end, so the element being
      // typed into is left alone: while it has focus it *is* the truth, and
      // the model is downstream of it.
      if (document.activeElement !== el && el.value !== value) el.value = value;
    }},
    // What this region is. A transparent element over the box, carrying the
    // role and the label — which is the parallel tree §7 specifies, built here
    // rather than only in the canvas renderer because the DOM renderer has the
    // same problem: every element it draws is `aria-hidden`, and a picture made
    // of hidden boxes is not reachable however real its elements are.
    //
    // A field is the exception, and it is the interesting one. The pooled
    // `<input>` is already a real control with real semantics, so this labels
    // *it* instead of putting a second announcement over the top — which is
    // what fixes "edit, blank", the specific Flutter failure §7 names.
    semantics: (x, y, w, h, role, label, flags, id) => {{
      const field = fieldElements.get(id);
      if (field !== undefined && field.style.display !== 'none') {{
        if (label.length > 0) field.setAttribute('aria-label', label);
        else field.removeAttribute('aria-label');
        if (flags & SEMANTIC_DISABLED) field.setAttribute('aria-disabled', 'true');
        else field.removeAttribute('aria-disabled');
        // The element is real, so it needs no node of its own — but the call
        // still claims its slot, or every element after it takes the wrong one.
        nodes[index] = null;
        index += 1;
        return;
      }}
      const el = place(take(), x, y);
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      // It describes; it does not draw and it does not catch the pointer.
      // Hit-testing is the program's, against the tree it laid out, and an
      // element over the top would take presses away from it.
      el.style.pointerEvents = 'none';
      el.style.opacity = '';
      describe(el, role, label, flags, id);
    }},
    // A real `<img>`, which is what gets the browser's own decoding, its cache
    // and its lazy loading. The box is the layout's; the picture is fitted into
    // it with `object-fit: contain`, so an image whose proportions differ from
    // its box is letterboxed rather than stretched.
    image: (x, y, w, h, src) => {{
      const el = place(take('img'), x, y);
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.objectFit = 'contain';
      // Decorative until a semantics call says otherwise — the same default
      // every other drawn thing here has. A picture that carries meaning gets
      // its description from the tree, not from the drawing call.
      el.setAttribute('alt', '');
      if (el.getAttribute('src') !== src) el.setAttribute('src', src);
    }},
    text: (x, y, body, colour) => {{
      const el = place(take(), x, y);
      el.style.color = hex(colour);
      el.style.font = fontCss();
      // The line box, pinned to the same height the layout measured.
      //
      // Without this the two renderers disagree about where a glyph sits
      // inside its line even though they agree exactly about where the line
      // is: CSS defaults `line-height` to `normal`, which is taller than the
      // font's ascent plus descent, and centres the text in what is left —
      // half-leading — while the canvas draws from the em box's top. Same y,
      // glyphs a couple of pixels apart, and switching renderers looked like
      // the layout had shifted. Setting the line box to `lineHeight()` —
      // which *is* ascent plus descent, and is what `text.height` answered
      // when the box was measured — makes the half-leading zero and puts the
      // baseline in both renderers at the same place.
      el.style.lineHeight = lineHeight() + 'px';
      el.style.whiteSpace = 'pre';
      // A drawing call carries no direction, so a single-direction run
      // re-derives its own from its first strong character — rule P2 again.
      // Without this the browser resolves the run against a left-to-right
      // base and hangs its leading and trailing neutrals on the wrong side.
      el.style.direction = firstStrongRtl(body) ? 'rtl' : 'ltr';
      el.textContent = body;
    }},
    clip: (x, y, w, h) => {{
      const el = place(take(), x, y);
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.overflow = 'hidden';
      // A clip is *structure*, not paint, so it is always fully opaque —
      // whatever alpha happens to be in force when it is opened.
      //
      // `place` applies the current alpha to everything it positions, which is
      // right for the things that draw and catastrophic here: an element with
      // `opacity` set forms a stacking context and dims **everything inside
      // it**. A clip opened while a translucent state layer was in force
      // therefore took its whole contents down with it — a design system drew
      // one eight-per-cent hover layer and the entire scrolling list behind it
      // rendered at eight per cent.
      //
      // Nothing had noticed because nothing had ever set alpha and then
      // clipped: the two features were independently correct and had never met.
      el.style.opacity = '';
      stack.push([host, originX, originY]);
      host = el;
      originX = x;
      originY = y;
    }},
    unclip: () => {{
      // An `unclip` paints nothing, but it still takes a slot: the scene graph
      // is indexed by call, and a call that consumed no node would put every
      // node after it one place out.
      nodes[index] = null;
      index += 1;
      const popped = stack.pop();
      if (popped) {{
        host = popped[0];
        originX = popped[1];
        originY = popped[2];
      }}
    }},

    /// Draw a frame from nothing, discarding whatever was there.
    rebuild: (calls) => {{
      container.replaceChildren();
      host = container;
      originX = 0;
      originY = 0;
      stack = [];
      nodes = [];
      index = 0;
      // The fields are not among the children that were just discarded, so a
      // frame that no longer has one has to say so — otherwise it stays on
      // screen over whatever replaced it.
      fieldsSeen = new Set();
      replay(calls, renderer);
      hideUnseenFields();
    }},

    /// Update the nodes whose calls changed, and leave the rest alone.
    ///
    /// Only reached when the diff says the two frames have the same shape, so
    /// node `i` and call `i` still describe the same thing. Anything else goes
    /// through `rebuild`, because a renderer that guessed at how nodes moved
    /// would be a second layout engine.
    patch: (previous, next, diff) => {{
      for (let i = diff.from; i < diff.newEnd; i += 1) {{
        const el = nodes[i];
        const call = next[i];
        const was = previous[i];
        if (!el) continue;
        // The origin a clip established is not re-derived here: it did not
        // change, or the frame would not have been patchable.
        const ox = Number(el.style.left.replace('px', '')) - was[1];
        const oy = Number(el.style.top.replace('px', '')) - was[2];
        el.style.left = call[1] + ox + 'px';
        el.style.top = call[2] + oy + 'px';
        // Opacity is a property of the *call*, and a patched node keeps
        // whatever it was created with unless it is written here. Leaving it
        // out meant a node reused across frames wore a stale translucency:
        // a state layer that had faded went on being drawn at the opacity it
        // had when its node was made, and one that had arrived stayed
        // invisible. Every branch below needs it, so it is done once, above.
        const want = call[call.length - 1];
        const opacity = typeof want === 'number' && want < 1 ? String(want) : '';
        if (call[0] !== 'c' && el.style.opacity !== opacity) el.style.opacity = opacity;
        if (call[0] === 'r') {{
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          el.style.background = hex(call[5]);
        }} else if (call[0] === 'R') {{
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          el.style.borderRadius = call[5] + 'px';
          el.style.background = hex(call[6]);
        }} else if (call[0] === 'D') {{
          // The border ring, which was missing entirely: a patched frame moved
          // it and never resized or recoloured it. So an outlined control that
          // changed width — a filter chip gaining its tick — kept the outline
          // of its old size around its new fill, which is the mismatched box
          // that looked like the control had grown a second edge. A control
          // that changed only its border, as an outlined field does when it
          // takes focus, did not appear to change at all.
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          el.style.borderRadius = call[5] + 'px';
          el.style.border = call[6] + 'px solid ' + hex(call[7]);
        }} else if (call[0] === 't') {{
          el.style.color = hex(call[4]);
          el.style.direction = firstStrongRtl(call[3]) ? 'rtl' : 'ltr';
          if (el.textContent !== call[3]) el.textContent = call[3];
        }} else if (call[0] === 'i') {{
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          // Only on a change: reassigning the same `src` makes the browser
          // drop the decoded picture and fetch it again, which flickers.
          if (el.getAttribute('src') !== call[5]) el.setAttribute('src', call[5]);
        }} else if (call[0] === 's') {{
          // A declaration that landed on the pooled field element has no node
          // of its own, and `nodes[i]` is null there — so this branch is only
          // reached for the ones that made a describing element.
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          describe(el, call[5], call[6], call[7], call[8]);
        }} else if (call[0] === 'c') {{
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
        }}
      }}
    }},
  }};
  return renderer;
}}

// The same drawing onto a 2D context. Text is drawn from its top-left, which
// is where the layout put it, so the baseline is set rather than assumed.
/// Where a canvas renderer's text is announced.
///
/// A canvas is a picture: everything a program draws into one is invisible to
/// a screen reader, which is the accessibility cost of the canvas path and the
/// reason the DOM renderer is the one to ship first. What closes the gap is a
/// **parallel tree**: the same runs of text, in the same order, in hidden DOM
/// next to the canvas. It is not a full accessibility tree — there are no
/// roles, no focus and no live regions yet — and calling it one would be a
/// claim this does not earn.
let announcer = null;

/// Where a real field's edits go.
///
/// The element is the browser's and the model is Kite's, so something has to
/// carry a value across. This is that: the page sets it, and the DOM renderer
/// calls it whenever the element the person is typing in changes.
let onFieldEdit = null;

/// The elements that must outlive a frame, keyed by the control's id.
///
/// `rebuild` calls `replaceChildren`, and a repaint happens on every keystroke
/// — so an element pooled with the divs would be destroyed and remade between
/// one letter and the next, losing focus, the caret and any selection every
/// time. They therefore live on `document.body`, positioned `fixed` against the
/// container's own rectangle, where nothing a renderer does can reach them.
///
/// Module-level rather than inside the DOM renderer's closure, because
/// something outside it has to be able to put them away: a canvas renderer
/// draws the field itself, and an element left showing over the top would say
/// everything twice.
///
/// **Keyed, not single.** One element was enough only while one field could be
/// on screen at a time. A text area makes two the ordinary case — a title
/// beside a body is the shape every compose form has — and a shared element
/// would have drawn the second field over the first and handed both the same
/// value. The tag is part of the identity as well: an `<input>` cannot be
/// turned into a `<textarea>`, so a field that changes kind is given a new
/// element rather than a mutated one.
const fieldElements = new Map();
/// Where each field last drew, in the container's own coordinates.
const fieldBoxes = new Map();
/// The ids that drew in the frame being built, so the ones that did not can be
/// put away before it is shown.
let fieldsSeen = new Set();
let onFieldFocus = null;
/// A key that types nothing, from inside a field. See `fieldEl`.
let onFieldKey = null;

function fieldEl(id, multiline) {{
  const kind = multiline ? 'textarea' : 'input';
  const existing = fieldElements.get(id);
  if (existing !== undefined) {{
    if (existing.tagName.toLowerCase() === kind) return existing;
    // A field that changed kind cannot keep its element. Nothing is preserved
    // across the swap because there is nothing to preserve: the value lives in
    // the model, which is where it came from.
    existing.remove();
  }}
  const el = document.createElement(kind);
  if (multiline) {{
    // A text area scrolls; it does not grow. The box came from the layout, and
    // an element that resized itself to its content would be a second layout
    // engine quietly disagreeing with the first.
    el.rows = 1;
    el.style.resize = 'none';
  }} else {{
    el.type = 'text';
  }}
  // The container is Kite's — the box, the border and the state layer are
  // all drawn. Only the editing is the browser's, so the element brings its
  // behaviour and none of its appearance.
  el.style.cssText =
    'position:fixed;background:transparent;border:none;outline:none;' +
    'padding:0;margin:0;box-sizing:border-box;resize:none;overflow:auto';
  document.body.appendChild(el);
  el.addEventListener('input', () => {{
    if (typeof onFieldEdit === 'function') onFieldEdit(el.value, id);
  }});
  // Clicking the element is how a person focuses this field, and the element
  // is *over* the stage — so the press never reaches the hit-test and the
  // model never learns focus moved. It finds out here instead.
  //
  // Reported as a press at the middle of the field rather than through a
  // channel of its own: the application already turns a press into focus,
  // and a control that is editable carries no message, so there is nothing
  // else for the press to set off. Letting the element keep the pointer is
  // what preserves clicking *into* the text to place the caret.
  el.addEventListener('focus', () => {{
    const box = fieldBoxes.get(id);
    if (typeof onFieldFocus === 'function' && box !== undefined) onFieldFocus(box, id);
  }});
  // A key that types nothing has to be forwarded from here, and this is the
  // half of the keyboard a real element had been swallowing.
  //
  // The document's own handler ignores any key whose target is not the body or
  // the canvas renderer's hidden input — which is every key pressed inside this
  // element. `input` reports what was *typed*, and Enter, Escape, Tab and the
  // arrows type nothing, so they were reaching nothing at all: a focused field
  // could be typed into, and Enter would not submit what was typed.
  //
  // The exception is Enter in a text area, which types a newline and is
  // therefore the element's own. Forwarding it would submit a form that the
  // person is still in the middle of writing.
  el.addEventListener('keydown', (e) => {{
    if ([...e.key].length === 1) return;
    if (multiline && e.key === 'Enter') return;
    if (typeof onFieldKey === 'function') onFieldKey(e);
  }});
  fieldElements.set(id, el);
  return el;
}}

/// Put every field away. A renderer that cannot make one calls this, and so
/// does the page when it switches to one.
export function hideFields() {{
  for (const el of fieldElements.values()) el.style.display = 'none';
}}

/// Put away the fields that did not draw in the frame just built.
///
/// A field that has left the tree has to stop being on screen, and it cannot
/// notice that by itself — nothing calls it to say it was not drawn. So the
/// renderer records what it saw and this hides the rest.
function hideUnseenFields() {{
  for (const [id, el] of fieldElements) {{
    if (!fieldsSeen.has(id)) el.style.display = 'none';
  }}
}}

export function setFieldEdit(fn) {{
  onFieldEdit = fn;
}}

export function setFieldFocus(fn) {{
  onFieldFocus = fn;
}}

export function setFieldKey(fn) {{
  onFieldKey = fn;
}}

export function setAnnouncer(element) {{
  announcer = element;
  if (element) element.replaceChildren();
}}

// Off while a damaged region is repainted: the parallel tree is rebuilt from
// the whole frame once, and a run of text that happens to sit in two damage
// rectangles must not be read twice.
let announcing = true;

function announce(body) {{
  if (!announcer || !announcing || body === '') return;
  const line = document.createElement('div');
  line.textContent = body;
  announcer.appendChild(line);
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
    // A canvas cannot hold a caret, so a field here is the text it shows —
    // which is exactly what it was before this call existed. The page's hidden
    // `#typing` input is what brings a keyboard and an IME up over it.
    field: (x, y, w, h, value, hint, colour, id, multiline) => {{
      const body = value.length > 0 ? value : hint;
      const line = lineHeight();
      if (!multiline) {{
        // The call hands over the *box*, because that is what a real input
        // would occupy. Drawing one line into it means centring the line,
        // which the element would have done with its own line box.
        const top = h > line ? y + (h - line) / 2 : y;
        renderer.text(x, top, body, colour);
        return;
      }}
      // A text area starts at the top and runs down: there is no single line to
      // centre, and centring the block would put the first line somewhere the
      // caret in the DOM's own `<textarea>` never goes.
      //
      // Broken here rather than by the layout, because a `\n` in a value is not
      // a `\n` in the tree — the value is the model's, and it changes on every
      // keystroke without the tree being rebuilt around it.
      let top = y;
      for (const l of wrapText(body, w)) {{
        if (top + line > y + h) break;
        renderer.text(x, top, l, colour);
        top += line;
      }}
    }},
    // The parallel tree, which is the whole of §7's answer for a canvas. It is
    // built from the *same* declarations the DOM renderer reads, so the two
    // cannot drift: there is one description of a control and two things that
    // consume it.
    semantics: (x, y, w, h, role, label, flags, id) => {{
      const el = semanticNode();
      if (el === null) return;
      el.style.left = x + 'px';
      el.style.top = y + 'px';
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      describe(el, role, label, flags, id);
    }},
    // Fitted into its box the way `object-fit: contain` fits an `<img>`, so
    // the two renderers letterbox a mismatched picture identically instead of
    // one of them stretching it.
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
      announce(body);
      // The atlas caches tiles keyed by glyph, so it is only safe while the
      // font it was built for is the one in force.
      // Drawn where the DOM would put it, which is a measured offset below the
      // em box's top — see `baselineOffset`.
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

    rebuild: (calls) => {{
      ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
      setAnnouncer(announcer);
      beginSemantics();
      replay(calls, renderer);
      endSemantics();
    }},

    /// Repaint only the rectangles that changed.
    ///
    /// A canvas has no nodes to patch — it is one surface — so what a retained
    /// scene graph buys here is different from what it buys the DOM: the frame
    /// is replayed in full, but clipped to the damage, so the pixels outside it
    /// are never touched and the calls outside it are rejected by the clip
    /// rather than drawn. The text announced to a screen reader is rebuilt
    /// whole either way, because a partial reading is worse than a repeated
    /// one.
    damage: (calls, rects) => {{
      setAnnouncer(announcer);
      for (const call of calls) {{
        if (call[0] === 't') announce(call[3]);
      }}
      announcing = false;
      // The tree is rebuilt whole, like the announced text and for the same
      // reason: a control whose label changed without its pixels moving would
      // otherwise keep the old one, and half a description is worse than a
      // repeated one.
      beginSemantics();
      for (const call of calls) {{
        if (call[0] === 's') {{
          renderer.semantics(
            call[1], call[2], call[3], call[4], call[5], call[6], call[7], call[8],
          );
        }}
      }}
      endSemantics();
      for (const rect of rects) {{
        ctx.save();
        ctx.beginPath();
        ctx.rect(rect[0], rect[1], rect[2], rect[3]);
        ctx.clip();
        ctx.clearRect(rect[0], rect[1], rect[2], rect[3]);
        for (const call of calls) {{
          // The tree was rebuilt whole above, and it paints nothing — a second
          // pass per damage rectangle would describe the same control once for
          // each rectangle it happened to touch.
          if (call[0] === 's') continue;
          const bounds = callBounds(call);
          if (bounds === null || rectsOverlap(bounds, rect)) {{
            replay([call], renderer);
          }}
        }}
        ctx.restore();
      }}
      announcing = true;
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

// ---- the parallel tree ------------------------------------------------------
//
// §7's answer for a renderer with no elements, and — as it turns out — the
// right answer for the one that has them too. A control declares what it is;
// this turns each declaration into something a screen reader can reach.
//
// It is **always on**. That is the first of Flutter's failures the design
// document names, and it is a failure of defaults rather than of capability:
// accessibility behind a switch is accessibility nobody switches on. The cost
// is one element per control per frame, reused between frames like everything
// else here.

/// ARIA's name for each `ui.Role`, by the number the role crosses as.
///
/// `null` means the role has no ARIA equivalent worth claiming — a group is a
/// `div`, and dressing it up as `role="group"` would add noise to the tree
/// without adding meaning.
const ROLES = [
  null, 'button', 'link', 'checkbox', 'radio', 'switch', 'slider',
  'textbox', 'img', 'heading', 'tab', 'menuitem', 'dialog', 'progressbar',
];

const SEMANTIC_DISABLED = 1;
const SEMANTIC_CHECKED = 2;
const SEMANTIC_FOCUSED = 4;
const SEMANTIC_CHECKABLE = 8;

/// Put the state a declaration carries onto a real element.
///
/// Shared by the DOM renderer and the canvas renderer's parallel tree, because
/// the two differ in *what element* they are describing and not at all in what
/// they have to say about it.
function describe(el, role, label, flags, id) {{
  const name = ROLES[role] ?? null;
  if (name === null) el.removeAttribute('role');
  else el.setAttribute('role', name);
  // A label is required by the tree, so an empty one is a program that has not
  // said anything yet rather than one that means "no name". Setting an empty
  // `aria-label` would actively silence the element.
  if (label.length > 0) el.setAttribute('aria-label', label);
  else el.removeAttribute('aria-label');
  if (flags & SEMANTIC_DISABLED) el.setAttribute('aria-disabled', 'true');
  else el.removeAttribute('aria-disabled');
  if (flags & SEMANTIC_CHECKABLE) {{
    el.setAttribute('aria-checked', (flags & SEMANTIC_CHECKED) ? 'true' : 'false');
  }} else el.removeAttribute('aria-checked');
  // Reachable by keyboard, in the order declared — which is the order the
  // controls were laid out, and the order `ui.focus_order` walks. A disabled
  // control leaves the tab order, exactly as it leaves focus order.
  el.tabIndex = (flags & SEMANTIC_DISABLED) ? -1 : 0;
}}

/// Where a renderer with no real elements puts its parallel tree.
///
/// The canvas renderer draws pixels and cannot be read; this is the element it
/// describes itself into, positioned over the canvas. Pooled between frames the
/// same way the DOM renderer's nodes are, because rebuilding it wholesale would
/// move focus off whatever held it every time anything moved.
let semanticsLayer = null;
let semanticsNodes = [];
let semanticsIndex = 0;

export function setSemanticsLayer(el) {{
  semanticsLayer = el;
  semanticsNodes = [];
  semanticsIndex = 0;
  if (el) el.replaceChildren();
}}

function beginSemantics() {{
  semanticsIndex = 0;
}}

/// Drop the nodes left over from the last frame — they describe controls that
/// are no longer there, and a reader would still find them.
function endSemantics() {{
  for (let i = semanticsIndex; i < semanticsNodes.length; i += 1) {{
    semanticsNodes[i].remove();
  }}
  semanticsNodes.length = semanticsIndex;
}}

function semanticNode() {{
  if (semanticsLayer === null) return null;
  const existing = semanticsNodes[semanticsIndex];
  if (existing !== undefined) {{
    semanticsIndex += 1;
    return existing;
  }}
  const el = document.createElement('div');
  el.style.position = 'absolute';
  semanticsLayer.appendChild(el);
  semanticsNodes.push(el);
  semanticsIndex += 1;
  return el;
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

/// Break a run into lines the way `std/ui.wrap` does.
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

export let renderer = textRenderer;

export function setRenderer(r) {{
  renderer = r;
}}

/// Where output goes. Replace to capture it.
export let write = (line) => console.log(line);

export function setWriter(fn) {{
  write = fn;
}}

function imports() {{
  return {{
{host_spread}    kite: {{
      print_int: (v) => write(showInt(v)),
      print_float: (v) => write(showFloat(v)),
      print_bool: (v) => write(showBool(v)),
      print_str: (i) => write(S(i)),
      str_concat: (a, b) => intern(S(a) + S(b)),
      str_eq: (a, b) => (S(a) === S(b) ? 1 : 0),
      // By code point, which is what `<` on JavaScript strings compares and
      // what Rust's `str` ordering compares — so the two backends sort the
      // same way. It is *not* alphabetical order in every language; collation
      // is a table and a locale, and neither belongs in an operator.
      str_compare: (a, b) =>
        S(a) < S(b) ? -1n : S(a) > S(b) ? 1n : 0n,
      draw_rect: (x, y, w, h, colour) => renderer.rect(x, y, w, h, Number(colour)),
      draw_rrect: (x, y, w, h, r, colour) => renderer.rrect(x, y, w, h, r, Number(colour)),
      draw_drrect: (x, y, w, h, r, width, colour) =>
        renderer.drrect(x, y, w, h, r, width, Number(colour)),
      draw_alpha: (a) => {{
        setAlpha(a);
        if (renderer.alpha) renderer.alpha(a);
      }},
      draw_text: (x, y, i, colour) => renderer.text(x, y, S(i), Number(colour)),
      draw_field: (x, y, w, h, v, hint, colour, id, multiline) =>
        renderer.field(
          x, y, w, h, S(v), S(hint), Number(colour), S(id), multiline !== 0,
        ),
      draw_image: (x, y, w, h, src) => renderer.image(x, y, w, h, S(src)),
      draw_semantics: (x, y, w, h, role, label, flags, id) => {{
        if (renderer.semantics) {{
          renderer.semantics(
            x, y, w, h, Number(role), S(label), Number(flags), S(id),
          );
        }}
      }},
      draw_clip: (x, y, w, h) => renderer.clip(x, y, w, h),
      draw_unclip: () => renderer.unclip(),
      measure_text: (i) => measure(S(i)),
      line_height: () => lineHeight(),
      draw_font: (size, weight) => {{
        setFont(size, Number(weight));
        if (renderer.font) renderer.font(size, Number(weight));
      }},
      // Kite counts characters, JavaScript counts UTF-16 code units, so each
      // of these goes through `[...s]` rather than indexing the string.
      str_slice: (i, from, to) => {{
        const cs = [...S(i)];
        const a = Math.min(Math.max(Number(from), 0), cs.length);
        const b = Math.min(Math.max(Number(to), a), cs.length);
        return intern(cs.slice(a, b).join(''));
      }},
      str_index_of: (i, n) => {{
        const at = S(i).indexOf(S(n));
        return at < 0 ? -1n : BigInt([...S(i).slice(0, at)].length);
      }},
      str_trim: (i) => intern(S(i).trim()),
      // A code point, not a UTF-16 code unit — `codePointAt` on a surrogate
      // pair would answer with half of one, and the bytecode VM answers with
      // the whole character.
      str_code_at: (i, at) => {{
        const c = [...S(i)][Number(at)];
        return c === undefined ? -1n : BigInt(c.codePointAt(0));
      }},
      // Interpolation shares its formatting with printing, so a value cannot
      // look one way in `io.print(x)` and another in `"\(x)"`.
      str_of_int: (v) => intern(showInt(v)),
      // A page has no standard input and no standard error. `console.error` is
      // the nearest honest equivalent of the second; there is no equivalent of
      // the first, so a read answers with the empty string — which is what a
      // program reading until it runs out already tests for.
      read_line: () => intern(""),
      error_str: (i) => {{
        if (typeof console !== "undefined") console.error(S(i));
      }},
      // A code point that is not a character — a surrogate, or past the end of
      // Unicode — is the empty string. `String.fromCodePoint` throws on those,
      // so the range is tested rather than the exception caught.
      str_from_code: (code) => {{
        const n = Number(code);
        const real =
          Number.isInteger(n) && n >= 0 && n <= 0x10ffff && !(n >= 0xd800 && n <= 0xdfff);
        return intern(real ? String.fromCodePoint(n) : "");
      }},
      str_of_float: (v) => intern(showFloat(v)),
      str_of_bool: (v) => intern(showBool(v)),
      // Characters, not UTF-16 code units: `[...s]` iterates code points, so
      // an emoji counts once rather than twice.
      str_len: (i) => BigInt([...S(i)].length),
      // ---- the scheduler ---------------------------------------------------
      //
      // A queue of live tasks is mutable state, and Kite has none, so the
      // scheduler lives here. What crosses the boundary is a resume closure
      // the host cannot look inside: it is handed back through the module's
      // own `kite_poll` export, which is the only thing that can enter it.
      task_spawn: (poll) => {{
        TASKS.push({{ poll, wakeAt: null, parked: false, waitingOnHost: false }});
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
      time_now: () => BigInt(clock),
    }},
  }};
}}

/// A renderer that records what it was asked to draw.
///
/// `view` is written as though it painted the whole tree every frame, because
/// that is the only shape a function from a model to a picture can have. What
/// makes that affordable is that the picture is *recorded* rather than
/// painted, and the recording is compared with the last one — so the tree the
/// program describes and the work the host does are two different things.
export function recordingRenderer() {{
  const calls = [];
  // The font is host *state*, and a damage repaint replays only the calls
  // inside the dirty rectangle. A recorded `font` call would therefore be
  // skipped as often as not, and the run after it drawn in whatever font the
  // last full frame happened to leave behind. Stamping every run of text with
  // the font in force when it was recorded is what makes any subset of the
  // recording replayable on its own — which is the property the whole scene
  // graph rests on.
  let size = NOMINAL_SIZE;
  let weight = 400;
  // Alpha is stamped onto each call for the same reason the font is: a damage
  // repaint replays only the calls inside the dirty rectangle, and a recorded
  // `alpha` call would be skipped as often as not.
  let opacity = 1;
  return {{
    calls,
    rect: (x, y, w, h, colour) => calls.push(['r', x, y, w, h, colour, opacity]),
    rrect: (x, y, w, h, r, colour) => calls.push(['R', x, y, w, h, r, colour, opacity]),
    drrect: (x, y, w, h, r, width, colour) =>
      calls.push(['D', x, y, w, h, r, width, colour, opacity]),
    alpha: (a) => {{
      opacity = a;
    }},
    font: (s, w) => {{
      size = s;
      weight = w;
    }},
    text: (x, y, body, colour) => calls.push(['t', x, y, body, colour, size, weight, opacity]),
    field: (x, y, w, h, value, hint, colour, id, multiline) =>
      calls.push(['f', x, y, w, h, value, hint, colour, id, multiline, size, weight, opacity]),
    image: (x, y, w, h, src) => calls.push(['i', x, y, w, h, src, opacity]),
    semantics: (x, y, w, h, role, label, flags, id) =>
      calls.push(['s', x, y, w, h, role, label, flags, id]),
    clip: (x, y, w, h) => calls.push(['c', x, y, w, h]),
    unclip: () => calls.push(['u']),
  }};
}}

/// Replay recorded calls into a real renderer.
export function replay(calls, renderer) {{
  // Only on a change, so a replay makes the same sequence of font calls the
  // program made rather than one per run of text.
  //
  // Started from what the host is *actually* in — not from the default. A
  // damage repaint calls this once per call, and each of those replays would
  // otherwise begin by assuming the font is 16dp: a run stamped 16dp would
  // match the assumption, no font would be selected, and it would be drawn in
  // whatever the previous frame left behind. That is the bug where the search
  // field's placeholder came out at the app bar's 22dp while the list was
  // being scrolled, and snapped back on the next full frame.
  //
  // Reading the state rather than assuming it also keeps the property that
  // made stamping worthwhile: any subset of a recording replays correctly on
  // its own.
  let size = fontSize;
  let weight = fontWeight;
  let opacity = alpha;
  // The alpha a call was recorded under, re-selected only when it changes, so
  // a replay makes the same sequence of alpha calls the program made.
  const wantAlpha = (a) => {{
    const next = a ?? 1;
    if (next !== opacity) {{
      opacity = next;
      setAlpha(next);
      if (renderer.alpha) renderer.alpha(next);
    }}
  }};
  for (const call of calls) {{
    if (call[0] === 'r') {{
      wantAlpha(call[6]);
      renderer.rect(call[1], call[2], call[3], call[4], call[5]);
    }} else if (call[0] === 'R') {{
      wantAlpha(call[7]);
      renderer.rrect(call[1], call[2], call[3], call[4], call[5], call[6]);
    }} else if (call[0] === 'D') {{
      wantAlpha(call[8]);
      renderer.drrect(call[1], call[2], call[3], call[4], call[5], call[6], call[7]);
    }} else if (call[0] === 't') {{
      wantAlpha(call[7]);
      const want = call[5] ?? NOMINAL_SIZE;
      const wantWeight = call[6] ?? 400;
      if (want !== size || wantWeight !== weight) {{
        size = want;
        weight = wantWeight;
        setFont(size, weight);
        if (renderer.font) renderer.font(size, weight);
      }}
      renderer.text(call[1], call[2], call[3], call[4]);
    }} else if (call[0] === 'f') {{
      wantAlpha(call[12]);
      const want = call[10] ?? NOMINAL_SIZE;
      const wantWeight = call[11] ?? 400;
      if (want !== size || wantWeight !== weight) {{
        size = want;
        weight = wantWeight;
        setFont(size, weight);
        if (renderer.font) renderer.font(size, weight);
      }}
      renderer.field(
        call[1], call[2], call[3], call[4], call[5], call[6], call[7], call[8], call[9],
      );
    }} else if (call[0] === 'i') {{
      wantAlpha(call[6]);
      renderer.image(call[1], call[2], call[3], call[4], call[5]);
    }} else if (call[0] === 's') {{
      // Paints nothing, so it takes no alpha and no font. A renderer with no
      // parallel tree to build simply does not offer the method.
      if (renderer.semantics) {{
        renderer.semantics(
          call[1], call[2], call[3], call[4], call[5], call[6], call[7], call[8],
        );
      }}
    }} else if (call[0] === 'c') renderer.clip(call[1], call[2], call[3], call[4]);
    else renderer.unclip();
  }}
}}

/// Whether two calls are the same call.
export function sameCall(a, b) {{
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {{
    if (a[i] !== b[i]) return false;
  }}
  return true;
}}

/// Whether two recordings are the same picture.
export function sameFrame(a, b) {{
  if (a === null || b === null || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {{
    if (!sameCall(a[i], b[i])) return false;
  }}
  return true;
}}

// ---- the retained scene graph ---------------------------------------------
//
// The recording *is* the scene graph. It survives between frames, and a new
// frame is compared with it rather than replacing it — which is what turns
// "repaint everything" into "repaint what moved".
//
// The diff is a common prefix and a common suffix, not a general edit script.
// That is a deliberate limit and it is the right one for this shape of data: a
// `view` is a walk over a tree in a fixed order, so a change to a model
// changes a contiguous run of calls, and an inserted row shifts a suffix that
// the suffix scan finds unmoved. A general LCS would find fewer differences on
// pathological input, and cost more on every frame that is not pathological.

/// The rectangle a call covers, or null for one that paints nothing.
///
/// Text is measured with the same measurer the layout used, so the damage
/// rectangle for a run of text is the rectangle the run was laid out into and
/// not a guess about it.
export function callBounds(call) {{
  if (call[0] === 'r' || call[0] === 'R') return [call[1], call[2], call[3], call[4]];
  // A field and an image both carry their own box, so their damage needs no
  // measuring.
  if (call[0] === 'f' || call[0] === 'i') return [call[1], call[2], call[3], call[4]];
  // A declaration damages nothing: it changes what a control is *called*, not
  // a single pixel. It still has to reach the tree when it changes, which the
  // renderers handle by rebuilding the tree rather than by painting.
  if (call[0] === 's') return null;
  if (call[0] === 'D') return [call[1], call[2], call[3], call[4]];
  if (call[0] === 't') {{
    // Measured in the font the run was recorded in, not in whatever font is
    // current: a damaged rectangle computed against the wrong size would be
    // the wrong rectangle, and the repaint would leave a strip behind.
    const size = fontSize;
    const weight = fontWeight;
    // A call with no font on it is one from before there was a font to put
    // there, and it means the default — never `undefined`, which would make
    // every measurement `NaN` and every damage rectangle empty.
    setFont(call[5] ?? NOMINAL_SIZE, call[6] ?? 400);
    const box = [call[1], call[2], measure(call[3]), lineHeight()];
    setFont(size, weight);
    return box;
  }}
  return null;
}}

/// What changed between two frames.
///
/// `from` is the first index that differs; `oldEnd` and `newEnd` are one past
/// the last index that differs in each frame. When nothing differs, `from`
/// equals both and `same` is true.
export function diffFrames(previous, next) {{
  const old = previous ?? [];
  let from = 0;
  while (from < old.length && from < next.length && sameCall(old[from], next[from])) {{
    from += 1;
  }}
  let oldEnd = old.length;
  let newEnd = next.length;
  while (oldEnd > from && newEnd > from && sameCall(old[oldEnd - 1], next[newEnd - 1])) {{
    oldEnd -= 1;
    newEnd -= 1;
  }}
  return {{
    same: previous !== null && from === oldEnd && from === newEnd,
    from,
    oldEnd,
    newEnd,
    // Whether the two frames have the same *shape*: same length, and every
    // call of the same kind. A renderer that holds one node per call can patch
    // those in place; anything else it has to rebuild, because the nodes and
    // the calls would no longer line up.
    patchable:
      previous !== null &&
      old.length === next.length &&
      old.every((call, i) => call[0] === next[i][0]),
  }};
}}

/// The rectangles a frame has to repaint, given what changed.
///
/// Both frames contribute: a rectangle that *left* has to be painted over just
/// as much as one that arrived. Overlapping rectangles are merged until none
/// overlap, and past a limit the whole lot collapses to one bounding box —
/// clearing a slightly larger area is always correct, and a damage list that
/// grew without bound would cost more to walk than the painting it saved.
export function damageOf(previous, next, diff, limit) {{
  const cap = limit ?? 16;
  const old = previous ?? [];
  let rects = [];
  for (let i = diff.from; i < diff.oldEnd; i += 1) {{
    const r = callBounds(old[i]);
    if (r) rects.push(r);
  }}
  for (let i = diff.from; i < diff.newEnd; i += 1) {{
    const r = callBounds(next[i]);
    if (r) rects.push(r);
  }}
  // A clip or an unclip among the changes moves everything drawn inside it, and
  // this diff does not track which calls those were. Repainting everything is
  // the honest answer rather than a wrong one.
  const structural = (call) => call[0] === 'c' || call[0] === 'u';
  for (let i = diff.from; i < diff.oldEnd; i += 1) {{
    if (structural(old[i])) return null;
  }}
  for (let i = diff.from; i < diff.newEnd; i += 1) {{
    if (structural(next[i])) return null;
  }}
  if (rects.length === 0) return [];
  rects = mergeRects(rects);
  if (rects.length > cap) return [boundingBox(rects)];
  return rects;
}}

export function rectsOverlap(a, b) {{
  return (
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
  );
}}

function boundingBox(rects) {{
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const r of rects) {{
    x0 = Math.min(x0, r[0]);
    y0 = Math.min(y0, r[1]);
    x1 = Math.max(x1, r[0] + r[2]);
    y1 = Math.max(y1, r[1] + r[3]);
  }}
  return [x0, y0, x1 - x0, y1 - y0];
}}

function mergeRects(rects) {{
  const out = [];
  for (const rect of rects) {{
    let merged = rect;
    let again = true;
    while (again) {{
      again = false;
      for (let i = out.length - 1; i >= 0; i -= 1) {{
        if (rectsOverlap(out[i], merged)) {{
          merged = boundingBox([out[i], merged]);
          out.splice(i, 1);
          again = true;
        }}
      }}
    }}
    out.push(merged);
  }}
  return out;
}}

// ---- the scheduler --------------------------------------------------------
//
// Round-robin, in spawn order — the same order the bytecode VM polls in, which
// is what lets the two backends be compared at all.
//
// The clock is **virtual**. When every task is waiting on a deadline it jumps
// to the earliest one rather than waiting for it, so a program that sleeps
// costs no real time and two backends running the same program produce the
// same interleaving. A scheduler that raced real timers could not be
// differentially tested, and a UI event loop does not need one: events arrive
// from the host, not from the clock.
const TASKS = [];
let clock = 0;
let wakeRequest = null;
let parkRequest = false;
let hostWaitRequest = false;

export async function drive(exports) {{
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

export async function instantiate(source = {wasm}) {{
  const bytes =
    source instanceof Uint8Array
      ? source
      : new Uint8Array(await (await fetch(source)).arrayBuffer());
{compile_step}
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

// An application exports `init`, `view` and `update` instead of `main`.
//
// The model never crosses the boundary as data — it is a Wasm reference the
// host holds and hands back, opaque to JavaScript. That is what lets a model
// be any Kite type at all without needing a representation both sides agree
// on, and it is why `update` returns a new model rather than mutating one:
// Kite has no mutable global state to mutate.
//
// `update(model, event, x, y, key)` takes every event through one door. A
// click fills `x` and `y` and leaves `key` empty; a key press fills `key` and
// leaves the position at zero. One signature rather than one per event means a
// new kind of event is a new constant rather than a new export, and a program
// that ignores a kind simply never matches on it.
export const EVENT_CLICK = 0n;
export const EVENT_KEY = 1n;
/// A wheel: `y` carries the distance, `x` the horizontal one.
export const EVENT_WHEEL = 2n;
/// The pointer moved, went down, or came up. One door, as with the rest: a
/// new kind of event is a new constant, and a program that ignores a kind
/// simply never matches on it.
export const EVENT_MOVE = 3n;
export const EVENT_DOWN = 4n;
export const EVENT_UP = 5n;
/// The window was resized: `x` and `y` carry the new size.
export const EVENT_RESIZE = 6n;
/// A frame is about to be painted: `x` carries the milliseconds since the last
/// one, so a simulation steps by *time* rather than by frame — the same
/// program then runs at the same speed on a machine that manages 30 frames a
/// second and one that manages 144.
///
/// The loop runs **while the model keeps changing**. A program that returns
/// the model it was given has nothing to animate, so the ticking stops until
/// real input arrives; one that returns a new model each frame keeps it going.
/// That rule needs no new export and no way to ask for frames: a static
/// application pays one comparison at startup and nothing after, and an
/// animating one never has to say so. A paused simulation that wants to keep
/// the loop warm returns a model that differs — a frame counter is enough —
/// which is the explicit way to say "still going".
export const EVENT_FRAME = 7n;
/// A real `<input>`'s value changed: `key` carries the whole new value.
///
/// The value, not the keystroke. A DOM input edits itself — a paste, a drag, an
/// IME commit and an autofill are all edits and none of them is a key — so what
/// comes back is what the field now says. It carries no id: only one control
/// holds focus, and focus is in the model, so the program already knows which
/// field this is. That is what lets an edit cross a door with one string on it.
export const EVENT_EDIT = 8n;
/// The page was hidden or shown: `x` is 1 when it came back and 0 when it went
/// away.
///
/// A hidden tab gets no frames — `requestAnimationFrame` is suspended there —
/// so a program that measures anything against the frame clock stops measuring
/// while the user is elsewhere, and the world it was tracking does not stop
/// with it. Sound already handed to the host keeps sounding, a download keeps
/// arriving, a countdown keeps being due. What the program cannot do is
/// *notice*, because the only clock it had has stopped.
///
/// So the absence is told rather than inferred. A program that has nothing
/// outside its own model to reconcile ignores this event and loses nothing;
/// one that does — anything scheduled ahead, on a clock that is not the frame
/// clock — gets the two moments where it can put itself right.
export const EVENT_VISIBLE = 9n;

export function isApplication(exports) {{
  return ["init", "view", "update"].every((n) => typeof exports[n] === "function");
}}
"#,
        strings_section = strings_section,
        compile_step = compile_step,
        hosts = host_section,
        host_spread = if hosts.is_empty() { "" } else { "    ...HOSTS,\n" },
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
const DOM_HOST: &str = r#"
if (HOSTS.dom) {
  // Index 0 is the empty slot that means "not found", so the table starts with
  // a hole in it rather than reserving one by convention.
  const NODES = [null];
  const at = (id) => NODES[Number(id)] ?? null;
  const handle = (el) => (el ? BigInt(NODES.push(el) - 1) : 0n);
  HOSTS.dom = {
    query: (selector) => handle(document.querySelector(S(selector))),
    query_all: (selector) =>
      str([...document.querySelectorAll(S(selector))].map((el) => handle(el)).join(",")),
    create: (tag) => handle(document.createElement(S(tag))),
    append: (parent, child) => {
      const p = at(parent);
      const c = at(child);
      if (p && c) p.appendChild(c);
    },
    detach: (node) => {
      const el = at(node);
      if (el) el.remove();
    },
    get_text: (node) => str(at(node)?.textContent ?? ""),
    set_text: (node, body) => {
      const el = at(node);
      // `textContent`, never `innerHTML`. `std/dom` deliberately offers no way
      // to set markup, and the host must not quietly provide one.
      if (el) el.textContent = S(body);
    },
    get_attr: (node, name) => str(at(node)?.getAttribute(S(name)) ?? ""),
    set_attr: (node, name, value) => {
      const el = at(node);
      if (el) el.setAttribute(S(name), S(value));
    },
    drop_attr: (node, name) => {
      const el = at(node);
      if (el) el.removeAttribute(S(name));
    },
    set_style: (node, property, value) => {
      const el = at(node);
      // `setProperty` takes the CSS spelling — `background-color` — which is
      // what a caller writes in a stylesheet and what `std/dom` documents.
      if (el) el.style.setProperty(S(property), S(value));
    },
    set_class: (node, name, on) => {
      const el = at(node);
      if (el) el.classList.toggle(S(name), on !== 0);
    },
    has_class: (node, name) => (at(node)?.classList.contains(S(name)) ? 1 : 0),
    get_title: () => str(document.title),
    set_title: (body) => {
      document.title = S(body);
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
    fetch_start: (method, url, body, headers) => {
      const request = { state: 0, status: 0, body: "", error: "", headers: null };
      const id = REQUESTS.push(request) - 1;
      const init = { method: S(method), headers: parseHeaders(S(headers)) };
      if (S(body) !== "") init.body = S(body);
      fetch(S(url), init)
        .then(async (response) => {
          request.status = response.status;
          request.headers = response.headers;
          request.body = await response.text();
          request.state = 1;
        })
        .catch((e) => {
          request.error = String(e && e.message ? e.message : e);
          request.state = 2;
        });
      return BigInt(id);
    },
    fetch_state: (id) => BigInt(REQUESTS[Number(id)].state),
    fetch_status: (id) => BigInt(REQUESTS[Number(id)].status),
    fetch_body: (id) => intern(REQUESTS[Number(id)].body),
    fetch_header: (id, name) => {
      const headers = REQUESTS[Number(id)].headers;
      return intern(headers ? headers.get(S(name)) ?? "" : "");
    },
    fetch_error: (id) => intern(REQUESTS[Number(id)].error),

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
      const source = new EventSource(S(url));
      s.source = source;
      s.take = (e) => {
        s.queue.push({ name: e.type || "message", id: e.lastEventId || "", data: e.data ?? "" });
      };
      source.onopen = () => { s.state = 1; };
      source.onmessage = s.take;
      // Registered before anything can arrive, which is the whole reason the
      // names are given at open.
      for (const name of S(names).split("\n")) {
        if (name !== "") source.addEventListener(name, s.take);
      }
      source.onerror = () => { if (source.readyState === 2) s.state = 2; };
      return BigInt(id);
    },
    sse_state: (id) => BigInt(STREAMS[Number(id)].state),
    sse_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    sse_next: (id) => {
      const s = STREAMS[Number(id)];
      const e = s.queue.shift();
      if (e === undefined) {
        s.taken = { name: "", id: "" };
        return intern("");
      }
      s.taken = { name: e.name, id: e.id };
      return intern(e.data);
    },
    sse_event_name: (id) => intern(STREAMS[Number(id)].taken.name),
    sse_event_id: (id) => intern(STREAMS[Number(id)].taken.id),
    sse_listen: (id, name) => {
      const s = STREAMS[Number(id)];
      if (s.source) s.source.addEventListener(S(name), s.take);
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
        socket = new WebSocket(S(url));
      } catch (e) {
        s.state = 2;
        s.error = String(e && e.message ? e.message : e);
        return BigInt(id);
      }
      s.socket = socket;
      socket.onopen = () => { s.state = 1; };
      socket.onmessage = (e) => { if (typeof e.data === "string") s.queue.push(e.data); };
      socket.onerror = () => {
        if (s.state !== 3) {
          s.state = 2;
          if (s.error === "") s.error = "the connection failed";
        }
      };
      socket.onclose = () => { if (s.state !== 2) s.state = 3; };
      return BigInt(id);
    },
    socket_state: (id) => BigInt(STREAMS[Number(id)].state),
    socket_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    socket_next: (id) => {
      const s = STREAMS[Number(id)];
      const message = s.queue.shift();
      return intern(message === undefined ? "" : message);
    },
    socket_send: (id, message) => {
      const s = STREAMS[Number(id)];
      if (s.state !== 1 || !s.socket) return 0n;
      s.socket.send(S(message));
      return 1n;
    },
    socket_error: (id) => intern(STREAMS[Number(id)].error),
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
      const next = S(url);
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
      })
      .catch((e) => {
        work.error = String(e && e.message ? e.message : e);
        work.state = 2;
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
      return intern(hex(out.buffer));
    },
    digest_start: (algorithm, text) =>
      start(subtle.digest(S(algorithm), bytes(S(text))).then(hex)),
    hmac_start: (algorithm, key, text) =>
      start(
        subtle
          .importKey("raw", bytes(S(key)), { name: "HMAC", hash: S(algorithm) }, false, [
            "sign",
          ])
          .then((k) => subtle.sign("HMAC", k, bytes(S(text))))
          .then(hex),
      ),
    derive_start: (password, salt, iterations) =>
      start(
        subtle
          .importKey("raw", bytes(S(password)), "PBKDF2", false, ["deriveBits"])
          .then((k) =>
            subtle.deriveBits(
              {
                name: "PBKDF2",
                salt: unhex(S(salt)),
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
      const name = S(kind);
      if (name === "AES-GCM") {
        return start(
          subtle.generateKey({ name, length: 256 }, false, ["encrypt", "decrypt"]).then(keep),
        );
      }
      const usages = name === "Ed25519" ? ["sign", "verify"] : ["deriveBits"];
      return start(subtle.generateKey(name, false, usages).catch(unsupported(name)).then(keep));
    },
    key_import_start: (material) => {
      const text = S(material);
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
            { name: "AES-GCM", iv: unhex(S(nonce)) },
            KEYS[Number(key)],
            bytes(S(plaintext)),
          )
          .then(hex),
      ),
    open_start: (key, nonce, cipher) =>
      start(
        subtle
          .decrypt(
            { name: "AES-GCM", iv: unhex(S(nonce)) },
            KEYS[Number(key)],
            unhex(S(cipher)),
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
      start(subtle.sign("Ed25519", KEYS[Number(key)].privateKey, bytes(S(text))).then(hex)),
    verify_start: (pub, text, signature) =>
      start(
        subtle
          .importKey("raw", unhex(S(pub)), "Ed25519", false, ["verify"])
          .catch(unsupported("Ed25519"))
          .then((k) => subtle.verify("Ed25519", k, unhex(S(signature)), bytes(S(text))))
          .then((ok) => (ok ? "true" : "false")),
      ),
    // Agreement and derivation in one step, so the raw X25519 output exists
    // only inside this chain: it has structure an attacker can use, and HKDF
    // is what turns it into a key that does not.
    agree_start: (key, pub) =>
      start(
        subtle
          .importKey("raw", unhex(S(pub)), "X25519", false, [])
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
    work_result: (id) => intern(WORK[Number(id)].result),
    work_error: (id) => intern(WORK[Number(id)].error),
    // Same time whichever way it goes: every byte is compared, and the length
    // is folded in rather than returned early on.
    constant_time_equal: (a, b) => {
      const x = S(a);
      const y = S(b);
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
pub fn generate_page(title: &str) -> String {
    strip_comments(&format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>{title}</title>
<style>
  /* The page around the program, kept deliberately plain: everything with a
     colour in it is drawn by the module, and chrome that competed with it
     would be chrome pretending to be the demonstration. */
  html, body {{ height: 100%; }}
  body {{ margin: 0; background: #0b0d10; color: #8b97a8; display: flex;
         flex-direction: column;
         font: 13px Roboto, "Helvetica Neue", "Segoe UI", system-ui, sans-serif; }}
  header {{ display: flex; gap: 8px; align-items: center; padding: 8px 12px;
            border-bottom: 1px solid #1f232b; flex: none; }}
  header strong {{ font-weight: 500; letter-spacing: .01em; margin-right: 4px; }}
  button {{ font: inherit; color: inherit; background: transparent; cursor: pointer;
            border: 1px solid #2a2f3a; border-radius: 999px; padding: 4px 12px; }}
  button[aria-pressed="true"] {{ background: #c9d1dc; border-color: #c9d1dc; color: #0b0d10; }}
  main {{ flex: 1; min-height: 0; display: flex; }}
  /* The program is given the whole area and told how big it is. A fixed box
     in the corner of a large window is not what an application looks like. */
  /* The stage is an application surface, not a document.
     `user-select` matters more than it looks. Under the DOM renderer every
     label is a real text node, so without this a click begins a *selection*:
     drag a few pixels and the browser highlights half the screen, a click that
     ends on a different node than it began on may not be delivered at all, and
     the next click goes to clearing the selection rather than to the program.
     What that feels like is a program that has to be clicked several times.
     `touch-action` is the same problem on a phone: without it a drag is a pan
     and never reaches the application. */
  #stage {{ flex: 1; min-height: 0; position: relative; overflow: hidden;
            user-select: none; -webkit-user-select: none; touch-action: none; }}
  pre {{ margin: 0; white-space: pre-wrap; padding: 12px;
         font: 12px ui-monospace, SFMono-Regular, Menlo, monospace; }}
  /* The parallel tree for the canvas renderer: read by a screen reader,
     invisible to everyone else. `display: none` would hide it from both. */
  #announcer {{ position: absolute; width: 1px; height: 1px; overflow: hidden;
                clip-path: inset(50%); white-space: nowrap; }}
  /* Where typing goes when the canvas is drawing. A canvas cannot hold a
     caret, so the text lands in a real input positioned under the pointer and
     kept invisible — the same trick every canvas editor uses. */
  #typing {{ position: absolute; opacity: 0; pointer-events: none; width: 1px; }}
  /* Over the stage, describing it. Invisible and untouchable: the boxes it
     names are hit-tested by the program against the tree it laid out, so an
     element here that caught a press would be taking it from the program. */
  #semantics {{ position: absolute; inset: 0; pointer-events: none; }}
  #semantics div {{ pointer-events: none; }}
</style>

<header>
  <strong>{title}</strong>
  <button id="dom" aria-pressed="true">DOM</button>
  <button id="canvas" aria-pressed="false">canvas</button>
  <button id="text" aria-pressed="false">text</button>
</header>

<main>
  <div id="stage"></div>
  <div id="announcer" role="region" aria-live="polite" aria-label="drawing"></div>
  <!-- The parallel tree, for the renderers that draw pixels instead of
       elements. It sits over the stage and catches nothing: every node inside
       it is describing a box the program already hit-tests for itself. -->
  <div id="semantics"></div>
  <input id="typing" aria-hidden="true" tabindex="-1">
</main>

<script type="module">
  import {{ instantiate, setRenderer, setWriter, isApplication, setMeasure,
            setLineHeight, fontMeasure, fontLineHeight, FONT, setAnnouncer,
            EVENT_CLICK, EVENT_KEY, EVENT_WHEEL, EVENT_MOVE, EVENT_DOWN,
            EVENT_UP, EVENT_RESIZE, EVENT_FRAME, EVENT_EDIT, EVENT_VISIBLE,
            setFieldEdit, setFieldFocus,
            setFieldKey, hideFields, setImageLoad, setSemanticsLayer, watchFonts, str,
            recordingRenderer, replay, diffFrames,
            damageOf, domRenderer, canvasRenderer, textRenderer }} from "./app.js";

  // Measure in the font that will be drawn, before anything is laid out.
  // Measurement and line height both follow whatever `draw.font` last chose.
  setMeasure(fontMeasure());
  setLineHeight(fontLineHeight());

  const stage = document.getElementById("stage");
  const announcer = document.getElementById("announcer");
  const semantics = document.getElementById("semantics");
  const typing = document.getElementById("typing");
  let currentRenderer = textRenderer;
  const buttons = {{
    dom: document.getElementById("dom"),
    canvas: document.getElementById("canvas"),
    text: document.getElementById("text"),
  }};

  // One instance for the life of the page. A program that exports `init`,
  // `view` and `update` keeps its model inside the module — the host holds a
  // reference and hands it back, opaque to JavaScript — so instantiating again
  // would throw the model away.
  const exports = await instantiate("./app.wasm");
  const interactive = isApplication(exports);
  let model = interactive ? exports.init() : null;
  let mode = "dom";


  function mount(which) {{
    stage.replaceChildren();
    // Only the DOM renderer can make a real input. The others draw the field
    // themselves, so one left showing over the top would say it all twice.
    if (which !== "dom") hideFields();
    setAnnouncer(which === "canvas" ? announcer : null);
    // Only a renderer with no real elements needs the parallel tree built
    // beside the picture. The DOM renderer puts the roles on the elements it
    // was already making, which is strictly better: they are the real ones.
    setSemanticsLayer(which === "canvas" ? semantics : null);
    if (which === "text") {{
      // The text renderer writes rather than draws, so it needs somewhere to
      // write to before the frame is replayed.
    }}
    if (which === "canvas") {{
      const canvas = document.createElement("canvas");
      const scale = window.devicePixelRatio || 1;
      const box = stage.getBoundingClientRect();
      canvas.width = Math.max(1, Math.round(box.width * scale));
      canvas.height = Math.max(1, Math.round(box.height * scale));
      canvas.style.width = box.width + "px";
      canvas.style.height = box.height + "px";
      stage.appendChild(canvas);
      const ctx = canvas.getContext("2d");
      ctx.scale(scale, scale);
      currentRenderer = canvasRenderer(ctx);
    }} else if (which === "text") {{
      const pre = document.createElement("pre");
      stage.appendChild(pre);
      setWriter((line) => {{ pre.textContent += line + "\n"; }});
      // Writing out a frame is this renderer's whole job, so it has no damage
      // path — and its `rebuild` clears first, because a transcript that
      // appended every frame would stop being a picture of one.
      currentRenderer = {{
        ...textRenderer,
        rebuild: (calls) => {{
          pre.textContent = "";
          replay(calls, textRenderer);
        }},
      }};
    }} else {{
      currentRenderer = domRenderer(stage);
    }}
  }}

  // The last frame, kept between frames. It is the retained scene graph: a new
  // frame is compared with it rather than replacing it, so an identical frame
  // costs one comparison and a frame that changed in one label costs one
  // element's worth of work rather than a rebuilt tree.
  let lastFrame = null;

  function draw(force) {{
    const recorder = recordingRenderer();
    setRenderer(recorder);
    if (interactive) {{
      exports.view(model);
    }} else {{
      exports.main();
    }}
    const next = recorder.calls;
    const diff = diffFrames(force ? null : lastFrame, next);
    if (diff.same) return;

    if (!force && diff.patchable && currentRenderer.patch) {{
      currentRenderer.patch(lastFrame, next, diff);
    }} else if (!force && lastFrame !== null && currentRenderer.damage) {{
      const rects = damageOf(lastFrame, next, diff);
      if (rects === null) {{
        currentRenderer.rebuild(next);
      }} else {{
        currentRenderer.damage(next, rects);
      }}
    }} else {{
      // A renderer that was just switched to, or one with no damage path, gets
      // the whole picture.
      currentRenderer.rebuild(next);
    }}
    lastFrame = next;
  }}

  function show(which) {{
    mode = which;
    for (const [name, button] of Object.entries(buttons)) {{
      button.setAttribute("aria-pressed", String(name === which));
    }}
    // A new renderer has nothing retained, so the frame it gets is a whole one
    // however little the model changed.
    mount(which);
    draw(true);
  }}

  // An event becomes a new model, and the new model replaces the old. Nothing
  // else changes: the program has no way to reach the page, and the page has
  // no way to reach inside the model.
  function send(kind, x, y, key) {{
    if (!interactive) return;
    // `key` is interned rather than passed as a JavaScript string: a `str` is
    // an index into the module's table, and handing an export a string quietly
    // becomes index 0.
    model = exports.update(model, kind, x, y, str(key));
    draw(false);
    // Input may have started something moving.
    wake();
  }}

  // The frame loop. It runs while the model keeps changing and stops when it
  // does not, which is how an application asks for animation without there
  // being anything to ask: a model is a value, so `update` returning the one
  // it was given *is* the statement that nothing is moving.
  let ticking = false;
  let lastTick = 0;
  // The longest step that is still a measurement of a frame. Two frames at the
  // slowest rate a browser animates at are under this; everything above it is
  // the machine having been elsewhere.
  const MAX_STEP = 100;

  function tick(now) {{
    if (!interactive) {{
      ticking = false;
      return;
    }}
    // The first frame of a run has no previous one to measure from, and
    // handing a program a huge first step would make every simulation jump.
    //
    // A frame can also arrive long after the one before it without the run
    // having ended: `requestAnimationFrame` is suspended in a hidden tab and
    // the callback already queued simply waits, so the step measured across a
    // trip to another tab is the length of the trip. A debugger, a long task
    // and a sleeping machine all do the same thing. None of them is a frame,
    // and a program handed one as a frame advances by however long it was
    // away — which is how a music player that was paused mid-track comes back
    // believing the track finished while nobody was listening.
    //
    // Past a tenth of a second it is a gap rather than a measurement, and the
    // honest step across a gap is zero: no time passed that this program was
    // in a position to account for. Programs that need to know the gap
    // happened get `EVENT_VISIBLE`, which says so directly.
    const step = lastTick === 0 ? 0 : now - lastTick;
    const elapsed = step > MAX_STEP ? 0 : step;
    lastTick = now;
    const next = exports.update(model, EVENT_FRAME, elapsed, 0, str(""));
    if (next === model) {{
      ticking = false;
      lastTick = 0;
      return;
    }}
    model = next;
    draw(false);
    requestAnimationFrame(tick);
  }}

  function wake() {{
    if (!interactive || ticking) return;
    ticking = true;
    lastTick = 0;
    requestAnimationFrame(tick);
  }}

  // A real input's edits reach the model the same way every other event does.
  setFieldEdit((value) => {{
    send(EVENT_EDIT, 0, 0, value);
  }});
  setFieldFocus((box) => {{
    send(EVENT_CLICK, box[0] + box[2] / 2, box[1] + box[3] / 2, "");
  }});
  // The font the stack resolves to has changed, which means every width the
  // last layout was computed from is now wrong.
  //
  // A repaint is not enough and a repaint is not what this asks for: the atlas
  // is keyed by the font it was built for, the measurements are the program's,
  // and only the program can lay out again. So the frame is discarded and
  // rebuilt from the model, in the font that is now actually there.
  watchFonts(() => {{
    measured();
    draw(true);
  }});
  // A picture the canvas renderer asked for has arrived, so the frame that
  // drew a gap where it goes is now wrong. Repaint rather than wake the model:
  // nothing about the model changed, and an animation loop started here would
  // never stop.
  setImageLoad(() => {{
    if (mode === "canvas") draw(true);
  }});
  // Enter, Escape, Tab and the arrows, from inside a real field.
  //
  // They type nothing, so they never arrive through `input`, and the document's
  // handler ignores them because their target is the element rather than the
  // body — which left a focused field able to take letters and unable to
  // submit them. A text area keeps its own Enter; `setFieldKey` is not called
  // for that one.
  setFieldKey((e) => {{
    const before = model;
    send(EVENT_KEY, 0, 0, e.key);
    if (model !== before) e.preventDefault();
  }});

  const at = (e) => {{
    const box = stage.getBoundingClientRect();
    return [e.clientX - box.left, e.clientY - box.top];
  }};

  // A click is *derived* from the press, rather than taken from the browser.
  //
  // `click` is a compatibility event and the browser is entitled to withhold
  // it: it is suppressed when the press turned into a text selection, when the
  // pointer moved far enough to be a drag, and when down and up resolve to
  // different targets. Every one of those happens constantly on a surface made
  // of absolutely-positioned text nodes, and the symptom is not subtle — the
  // program simply does not respond, and whoever is using it clicks again.
  //
  // A press that went down on the stage and came up on it is a click. That is
  // the whole rule, it is the one every canvas application uses, and it cannot
  // be taken away. `pressedOn` also keeps a release that began *outside* the
  // stage — finishing a drag that started on the page — from counting as one.
  let pressedOn = false;

  const activate = (x, y) => {{
    send(EVENT_CLICK, x, y, "");
    // Typing goes to a real input placed where the pointer is: a canvas
    // cannot hold a caret, and an invisible input is how every canvas editor
    // handles this. It also brings the on-screen keyboard up on a phone.
    if (mode === "canvas") {{
      typing.style.left = x + "px";
      typing.style.top = y + "px";
      typing.focus({{ preventScroll: true }});
    }}
  }};

  // Pointer events, all through the same door as everything else.
  stage.addEventListener("pointermove", (e) => {{
    const [x, y] = at(e);
    send(EVENT_MOVE, x, y, "");
  }});
  stage.addEventListener("pointerdown", (e) => {{
    const [x, y] = at(e);
    // Capture, so the *release* is reported here wherever it happens.
    //
    // Without it a press that ends outside the stage — dragged off a control
    // and let go, which is how anyone cancels — is never reported at all: the
    // program is left believing the button is still held, and its pressed
    // state and its ripple stay on the screen for good. Capture also makes a
    // drag across the stage one continuous gesture, which is what a slider
    // being scrubbed needs.
    try {{
      stage.setPointerCapture(e.pointerId);
    }} catch (err) {{
      // Not every pointer can be captured; the release still arrives when the
      // pointer is over the stage, which is the common case.
    }}
    pressedOn = true;
    send(EVENT_DOWN, x, y, "");
  }});
  stage.addEventListener("pointerup", (e) => {{
    const [x, y] = at(e);
    try {{
      stage.releasePointerCapture(e.pointerId);
    }} catch (err) {{
      // Already released, which is not a failure.
    }}
    send(EVENT_UP, x, y, "");
    if (pressedOn) {{
      pressedOn = false;
      activate(x, y);
    }}
  }});
  // A gesture the browser takes away — a system swipe, a context menu — ends
  // the press as surely as a release does, and leaves the same stuck state
  // behind if it is ignored.
  stage.addEventListener("pointercancel", (e) => {{
    const [x, y] = at(e);
    // A cancelled gesture releases the press but is *not* an activation.
    pressedOn = false;
    send(EVENT_UP, x, y, "");
  }});

  // The pointer leaving, which the browser does not otherwise report.
  //
  // `pointermove` fires while the pointer is over the stage and stops when it
  // is not — there is no final event for the pixel outside the edge. A program
  // that lights a control under the pointer therefore has no way to learn the
  // pointer has gone, and the last thing hovered stays lit until something else
  // is: move off the window and a row keeps its state layer forever.
  //
  // Sent as an ordinary move to a point outside every rectangle rather than as
  // a new kind of event. A move to nowhere *is* what happened, it needs no new
  // constant, and every program that already handles `EVENT_MOVE` is fixed by
  // it without being edited — which a new constant, ignored by default, would
  // not have done.
  stage.addEventListener("pointerleave", () => {{
    send(EVENT_MOVE, -1, -1, "");
  }});

  // Text typed into the hidden input arrives one character at a time, which
  // is the same shape a key press has. An IME's composition arrives here too,
  // once it is committed.
  typing.addEventListener("input", () => {{
    for (const character of typing.value) {{
      send(EVENT_KEY, 0, 0, character);
    }}
    typing.value = "";
  }});

  // A program is told how big it is before it first draws, not only when the
  // window changes: an application that only learned its size on a resize
  // would lay its first frame out against a guess.
  function measured() {{
    const box = stage.getBoundingClientRect();
    send(EVENT_RESIZE, box.width, box.height, "");
  }}

  window.addEventListener("resize", () => {{
    if (mode === "canvas") mount("canvas");
    measured();
  }});

  // Keys go to the document rather than the stage: a div is not focusable, and
  // making it so would put a focus ring around the whole application.
  document.addEventListener("keydown", (e) => {{
    if (!interactive || e.metaKey || e.ctrlKey || e.altKey) return;
    // Keys reach the document from two places: the page itself, and the hidden
    // input the canvas renderer focuses to bring up a keyboard and an IME.
    //
    // That input reports what was *typed*, through its own `input` event, and
    // a Backspace types nothing — neither do Enter, Tab, or the arrows. They
    // were therefore dropped twice over in canvas mode: the `input` event had
    // no character to report, and this handler ignored the event because its
    // target was the input rather than the body. A field could be typed into
    // and never corrected.
    //
    // So keys from the input are taken here, except the ones it will report
    // itself. A character must come through `input` and only `input`: that is
    // the path an IME commits along, and taking it here as well would put
    // every letter in twice.
    const typed = e.target === typing;
    if (e.target !== document.body && !typed) return;
    if (typed && [...e.key].length === 1) return;
    // A key the program acts on should not also scroll the page. Which keys
    // those are is the program's business, so this asks by comparing the model
    // it gives back — an application that ignores the key is left alone.
    const before = model;
    send(EVENT_KEY, 0, 0, e.key);
    if (model !== before) e.preventDefault();
  }});

  stage.addEventListener("wheel", (e) => {{
    if (!interactive) return;
    const before = model;
    send(EVENT_WHEEL, e.deltaX, e.deltaY, "");
    if (model !== before) e.preventDefault();
  }}, {{ passive: false }});

  // Leaving the tab and coming back. Both directions are sent, because they
  // are different questions: going away is the last moment a program can put
  // down anything that would go on without it, and coming back is the first
  // moment it can find out what happened while it was not being asked.
  document.addEventListener("visibilitychange", () => {{
    const shown = !document.hidden;
    // The frame clock restarts here rather than resuming. `wake()` only resets
    // it for a loop that had stopped, and a loop suspended by the tab going
    // away never stopped — its callback is still queued, holding a timestamp
    // from before the trip.
    if (shown) lastTick = 0;
    send(EVENT_VISIBLE, shown ? 1 : 0, 0, "");
  }});

  for (const [name, button] of Object.entries(buttons)) {{
    button.addEventListener("click", () => show(name));
  }}
  show("dom");
  // The size, once the stage has one. `show` mounted it, so the box is real.
  measured();
  // An application that animates from the first frame starts here; a static one
  // stops after a single comparison.
  wake();
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
    fn the_glue_embeds_every_string_constant() {
        let g = generate_glue(&["hello".into(), "world".into()], "app.wasm");
        assert!(g.contains(r#""hello""#));
        assert!(g.contains(r#""world""#));
        assert!(g.contains(r#""app.wasm""#));
        assert!(g.contains("export async function run"));
    }
}
