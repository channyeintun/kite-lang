// The AST, drawn as a tree.
//
// `--emit ast` prints Rust's `{:#?}`: every field of every node, one to a
// line, four hundred lines for a program of fifteen. It is the truth, and it
// is unreadable — nothing folds away, and the part you came to look at is a
// hundred lines below the fold with its brackets somewhere off the bottom.
//
// The dump is regular, though. Four spaces a level, one node a line, and only
// the shapes `derive(Debug)` emits. So it is read back here and drawn as a
// tree that folds, rather than asking the compiler for a second serialisation
// of the AST that would then have to be kept in step with the first.
//
// Two things are moved rather than hidden, and both are named where they land:
// a `span` is drawn beside the node it describes instead of below it, and a
// wrapper holding exactly one node — `Some(x)`, `Fn(FnDecl { … })` — is drawn
// as one row carrying both names. Everything else is on the tree somewhere.

/// One line of `{:#?}`. A field name is an identifier and a colon; the rest is
/// the value, and a trailing comma is punctuation rather than content. The
/// value is taken lazily so that a comma *inside* a string stays in it.
const LINE = /^(?:([A-Za-z_][A-Za-z0-9_]*): )?(.*?),?$/;

/// `file:start..end`, as `kite_span` writes a span. The file is dropped: the
/// dump is produced before any other file is parsed, so every span in it is an
/// offset into the source in the editor.
const SPAN = /^\d+:(\d+)\.\.(\d+)$/;

/// How deep the tree opens on arrival. Three reaches a declaration's fields
/// without unrolling its body.
const OPEN_TO = 3;

/// The longest run of source drawn beside a node. A literal or a name is worth
/// showing; a whole function body is the tree itself, spelled differently.
const SNIPPET = 48;

const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

/// Whether a stage's output is an AST dump at all. A program that does not
/// compile answers with diagnostics instead, and those are text.
export function isAstDump(text) {
  return /^SourceFile \{/.test(text);
}

/// The dump, as nodes: `{ field, name, value, valueField, span, kind,
/// children }`, where `kind` is one of `struct`, `tuple`, `list` or `leaf`.
export function parse(dump) {
  const root = { children: [] };
  const stack = [root];

  for (const raw of dump.split("\n")) {
    const line = raw.trim();
    if (line === "") continue;

    const match = LINE.exec(line);
    if (!match) continue;
    const field = match[1] ?? null;
    const body = match[2];

    if (body === "}" || body === ")" || body === "]") {
      if (stack.length > 1) stack.pop();
      continue;
    }

    const node = {
      field,
      name: "",
      value: null,
      valueField: null,
      span: null,
      kind: "leaf",
      children: [],
    };
    if (body.endsWith("{")) {
      node.kind = "struct";
      node.name = body.slice(0, -1).trim();
    } else if (body.endsWith("(")) {
      node.kind = "tuple";
      node.name = body.slice(0, -1).trim();
    } else if (body === "[") {
      node.kind = "list";
    } else {
      node.value = body;
    }

    stack[stack.length - 1].children.push(node);
    if (node.kind !== "leaf") stack.push(node);
  }

  return root.children.map(tidy);
}

/// Fold the three shapes that are Rust's rather than the language's. Depth
/// first, so a node is tidied against children that are already tidy.
function tidy(node) {
  node.children = node.children.map(tidy);

  // A span is a range, not a string. Every one becomes a range on the node
  // that carries it, which is what lets the row show the source it covers and
  // lets a click go there.
  if (node.kind === "leaf" && SPAN.test(node.value ?? "")) {
    const [, start, end] = SPAN.exec(node.value);
    node.span = { start: Number(start), end: Number(end) };
    node.value = null;
  }

  // A node's own span belongs beside its name rather than below it: the
  // `span:` field nearly every node carries, and the lone span inside a
  // wrapper like `Int(1:56..57)`, are both facts about the node above them. A
  // second span with a name of its own — `sig_span` — stays the row it was.
  if (node.kind !== "list") {
    const at = node.children.findIndex(
      (c) =>
        c.kind === "leaf" &&
        c.span !== null &&
        c.value === null &&
        (c.field === "span" || c.field === null),
    );
    if (at >= 0) {
      node.span = node.children[at].span;
      node.children.splice(at, 1);
    }
  }

  // A wrapper around a single node is one node wearing two names. The names
  // are joined rather than dropped — `Fn FnDecl`, `Name Ident` — so the row
  // still says everything the two rows said. Lists are left alone: a list of
  // one is not the thing it holds.
  if (node.kind === "tuple" && node.children.length === 1 && node.children[0].kind !== "leaf") {
    const only = node.children[0];
    if (only.name && only.name !== node.name) node.name = `${node.name} ${only.name}`;
    node.value = only.value;
    node.valueField = only.valueField;
    node.span = node.span ?? only.span;
    node.kind = only.kind;
    node.children = only.children;
  }

  // A node whose one remaining field is a value *is* that value. `Ident` with
  // a single `name: "divide"` beneath it is a row, not a row and a row.
  if (
    node.kind !== "list" &&
    node.value === null &&
    node.children.length === 1 &&
    node.children[0].kind === "leaf" &&
    node.children[0].value !== null
  ) {
    const only = node.children[0];
    node.value = only.value;
    node.valueField = only.field;
    node.children = [];
  }

  return node;
}

/// Byte offsets to the indices a `<textarea>` counts in. A span is a range of
/// UTF-8 bytes and JavaScript counts UTF-16 units, which agree until the first
/// character above U+007F — the `£` in one of the samples on the page.
function byteIndex(source) {
  const at = [];
  let i = 0;
  for (const ch of source) {
    const cp = ch.codePointAt(0);
    const bytes = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    for (let k = 0; k < bytes; k += 1) at.push(i);
    i += ch.length;
  }
  at.push(i);
  return at;
}

/// Which colour a value takes, on the same scheme the highlighter uses for the
/// source it came from.
function valueClass(value) {
  if (value.startsWith('"')) return "s";
  if (value === "None" || value === "[]" || value === "{}") return "dim";
  if (value === "true" || value === "false") return "n";
  if (/^[0-9]/.test(value)) return "n";
  if (/^[A-Z]/.test(value)) return "t";
  return "";
}

/// The one row a node draws: its field, its name, its value, and the run of
/// source it covers when that is short enough to be a help rather than a copy
/// of the tree.
function row(node, ctx) {
  const line = el("span", "row");
  line.append(el("span", "twisty"));
  if (node.field) line.append(el("span", "f", `${node.field}: `));
  if (node.name) line.append(el("span", "t", node.name));
  if (node.kind === "list") {
    line.append(el("span", "count", `${node.name ? " " : ""}[${node.children.length}]`));
  }

  if (node.value !== null) {
    if (node.name) line.append(document.createTextNode(" "));
    // The inner field name earns its place unless the row has already said it:
    // `name: Ident "divide"` rather than `name: Ident name: "divide"`.
    if (node.valueField && node.valueField !== node.field) {
      line.append(el("span", "f", `${node.valueField}: `));
    }
    line.append(el("span", valueClass(node.value), node.value));
  } else if (node.span) {
    // The separator is a character rather than a `::before`, so that the name
    // read out for the row is the one on it: `Param · a: int`, not `Parama:
    // int`.
    const text = ctx.snippet(node.span);
    // A field name has already left a space behind it; a node name has not.
    if (text) line.append(el("span", "snip", `${node.name ? " " : ""}· ${text}`));
    // A span of its own with nothing beside it — `sig_span` over a whole
    // signature — still has to say something, and what it has is the range.
    else if (!node.name) line.append(el("span", "dim", `${node.span.start}..${node.span.end}`));
  }

  if (node.span) {
    line.title = `bytes ${node.span.start}..${node.span.end}`;
  }
  return line;
}

function build(node, depth, ctx) {
  const item = el("li");
  item.setAttribute("role", "treeitem");
  item.tabIndex = -1;

  const line = row(node, ctx);
  item.append(line);
  // Named from its own row: without this the name a screen reader announces is
  // the row plus every row underneath it.
  item.setAttribute("aria-label", line.textContent.trim() || "node");

  if (node.span) {
    item.dataset.start = String(node.span.start);
    item.dataset.end = String(node.span.end);
  }

  if (node.children.length > 0) {
    item.setAttribute("aria-expanded", String(depth < OPEN_TO));
    const group = el("ul");
    group.setAttribute("role", "group");
    for (const child of node.children) group.append(build(child, depth + 1, ctx));
    item.append(group);
  }
  return item;
}

/// The tree, as an element to put wherever the output goes.
///
/// `source` is the program the dump was made from, which is what lets a node
/// show the text it covers; `onSelect(from, to)` is handed the range of that
/// source the chosen node covers — already in the units JavaScript counts, so
/// it goes straight to `setSelectionRange` — and is how the page highlights a
/// node back in the editor.
export function astTree(dump, { source = "", onSelect = null } = {}) {
  const index = byteIndex(source);
  const ctx = {
    snippet({ start, end }) {
      if (end - start > SNIPPET || end <= start) return "";
      const text = source.slice(index[start] ?? 0, index[end] ?? source.length);
      return text.trim() === "" ? "" : text.replace(/\s+/g, " ");
    },
  };

  const wrap = el("div", "tree-wrap");

  const tree = el("ul", "tree");
  tree.setAttribute("role", "tree");
  tree.setAttribute("aria-label", "The abstract syntax tree");
  for (const node of parse(dump)) tree.append(build(node, 0, ctx));

  const raw = el("pre", "tree-raw", dump);
  raw.hidden = true;

  const items = () => [...tree.querySelectorAll('li[role="treeitem"]')];
  const shown = () => items().filter((item) => item.offsetParent !== null);

  /// One tab stop for the whole tree, on whichever node was last reached: the
  /// tree is one control, and tabbing through two hundred nodes is not
  /// navigation.
  const tabStop = (next) => {
    if (!next) return;
    for (const item of items()) item.tabIndex = -1;
    next.tabIndex = 0;
  };

  const focus = (next) => {
    if (!next) return;
    tabStop(next);
    next.focus();
  };

  const toggle = (item, open) => {
    const state = item.getAttribute("aria-expanded");
    if (state === null) return;
    item.setAttribute("aria-expanded", String(open ?? state === "false"));
  };

  const select = (item) => {
    for (const other of items()) other.removeAttribute("aria-selected");
    item.setAttribute("aria-selected", "true");
    if (onSelect && item.dataset.start !== undefined) {
      const start = Number(item.dataset.start);
      const end = Number(item.dataset.end);
      onSelect(index[start] ?? 0, index[end] ?? source.length);
    }
  };

  const setAll = (open) => {
    for (const item of tree.querySelectorAll("li[aria-expanded]")) {
      item.setAttribute("aria-expanded", String(open));
    }
    // Whatever had the tab stop may now be inside something closed.
    const first = shown()[0];
    if (first) {
      for (const item of items()) item.tabIndex = -1;
      first.tabIndex = 0;
    }
  };

  tree.addEventListener("click", (event) => {
    const item = event.target.closest('li[role="treeitem"]');
    if (!item) return;
    // The twisty opens; the row selects. Anything else and a reader cannot
    // look at a node without also rearranging the tree under it.
    if (event.target.classList.contains("twisty")) {
      toggle(item);
      return;
    }
    // Choosing a node hands the focus to the editor, where its range is now
    // selected — the tab stop stays here, so shift-tab comes back to it.
    tabStop(item);
    select(item);
  });

  tree.addEventListener("dblclick", (event) => {
    const item = event.target.closest('li[role="treeitem"]');
    if (item) toggle(item);
  });

  tree.addEventListener("keydown", (event) => {
    const item = event.target.closest('li[role="treeitem"]');
    if (!item) return;
    const visible = shown();
    const at = visible.indexOf(item);
    const expanded = item.getAttribute("aria-expanded");

    switch (event.key) {
      case "ArrowDown":
        focus(visible[at + 1]);
        break;
      case "ArrowUp":
        focus(visible[at - 1]);
        break;
      case "ArrowRight":
        if (expanded === "false") toggle(item, true);
        else if (expanded === "true") focus(item.querySelector('li[role="treeitem"]'));
        break;
      case "ArrowLeft":
        if (expanded === "true") toggle(item, false);
        else focus(item.parentElement.closest('li[role="treeitem"]'));
        break;
      case "Home":
        focus(visible[0]);
        break;
      case "End":
        focus(visible[visible.length - 1]);
        break;
      // One key, one thing: space opens a node where it is, enter leaves for
      // the source it covers.
      case " ":
        toggle(item);
        break;
      case "Enter":
        select(item);
        break;
      default:
        return;
    }
    event.preventDefault();
  });

  const tools = el("div", "tree-tools");
  const button = (label, onClick) => {
    const b = el("button", null, label);
    b.type = "button";
    b.onclick = onClick;
    return b;
  };
  const expand = button("Expand all", () => setAll(true));
  const collapse = button("Collapse all", () => setAll(false));
  const rawly = button("Raw", () => {
    const showing = raw.hidden;
    raw.hidden = !showing;
    tree.hidden = showing;
    expand.disabled = showing;
    collapse.disabled = showing;
    rawly.setAttribute("aria-pressed", String(showing));
  });
  rawly.setAttribute("aria-pressed", "false");
  rawly.title = "The dump exactly as the compiler printed it";
  tools.append(expand, collapse, rawly);
  tools.append(el("span", "hint", "select a node to find it in the source"));

  const first = tree.querySelector('li[role="treeitem"]');
  if (first) first.tabIndex = 0;

  wrap.append(tools, tree, raw);
  return wrap;
}
