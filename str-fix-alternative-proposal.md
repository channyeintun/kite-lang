# Alternative proposal: collector-visible strings without a wrapper object

Status: design proposal only; no implementation is included.

## Summary

Make a Kite `str` a bare JavaScript string held in an `externref`, with the
generated glue providing ordinary host implementations of string operations.
Keep JS String Builtins as an optional acceleration, not as the requirement for
having collector-visible strings.

This closes the append-only-table leak without:

- a generic `Iterator` trait;
- a WasmGC wrapper allocation for every derived string;
- mode-specific wrapping and unwrapping at every host call; or
- breaking exported `pub fn` signatures that contain `str`.

Call the representation `Strings::HostRef` below. The name is illustrative.

## Why this is possible

`Strings::Builtins` already stores a `str` as `externref`. The proposal-specific
parts of that mode are narrower than the representation itself:

- `concat` and `equals` are imported from `wasm:js-string`; and
- string constants are synthesized through the imported-string-constants
  compile option.

An `externref` does not require either feature. It can hold an ordinary
JavaScript string today, and the Wasm engine traces the reference. When no
reachable Wasm value holds it, the string is collectible.

For a baseline mode:

- import `str_concat` and `str_eq` from the generated `kite` host object, as
  Table does;
- give them the `externref` signatures already present in
  `JS_STRING_IMPORTS`;
- import literals as ordinary `externref` globals supplied by the glue, using
  the mechanism already introduced for `Strings::Object`; and
- make `intern` and `S` identity functions.

No append-only `STRINGS` or `INDEX` structure is involved.

## Representation comparison

| Property | Table | Object | Proposed HostRef |
|---|---|---|---|
| Collector can see a live string | No | Yes | Yes |
| Reclaims derived strings | No | Yes | Yes |
| Extra WasmGC allocation per result | No | Yes | No |
| JavaScript export accepts/returns `str` directly | Via index helpers | Not currently | Yes |
| Requires JS String Builtins | No | No | No |
| Can attach a per-value cache | By table index | Yes | No |
| Boundary conversion sites | Table lookup | Wrap/unwrap | Identity |

Object's theoretical advantage is its cache field. The current implementation
does not use that field: it initializes it to `-1`, then the glue scans the
string again for every indexed operation. HostRef therefore gives up no cache
that currently works.

## Compiler shape

### 1. Add the representation

For `Strings::HostRef`:

- `val_type_with(TyId::STR)` returns nullable `externref`;
- `imports_for` uses the existing externref-oriented import signatures;
- `import_origin` leaves every string operation in the `kite` namespace rather
  than moving `concat` and `equals` to `wasm:js-string`;
- literals are imported as ordinary `externref` globals; and
- the normal `WebAssembly.instantiate(bytes, imports())` path is used.

Unlike Object, no `str_record`, `wrap_str`, `unwrap_str`, boundary value type,
or record-aware equality path is needed. Existing deep-equality functions
still call host equality for `str`, exactly as Builtins already does.

### 2. Use identity glue

The HostRef string section can use:

```js
const intern = (s) => String(s);
const S = (s) => s;

export function str(s) {
  return String(s);
}

export function text(s) {
  return s;
}
```

All existing host functions continue to read through `S` and return through
`intern`. `str_concat` and `str_eq` remain ordinary generated functions:

```js
str_concat: (a, b) => a + b,
str_eq: (a, b) => (a === b ? 1 : 0),
```

### 3. Switch all entry points together

Once differential and API tests pass, select HostRef in:

- `Strings::default` and `kite_codegen_wasm::compile`;
- the native `kitec` CLI when `--js-strings` is absent;
- `kite-playground::kite_build`;
- `@kite-lang/compiler-wasm`;
- the Vite plugin's default build; and
- direct glue-generation helpers.

`--js-strings` can continue selecting Builtins for intrinsic concat/equality.
`Strings::Table` should then be removed, rather than retained as a selectable
leaking compatibility mode.

## Linear traversal without a generic Iterator trait

Collector visibility and traversal complexity are separate problems. HostRef
fixes lifetime. It does not make arbitrary code-point indexing constant-time,
and neither the VM nor native UTF-8 strings promise that.

The problematic shape is repeated sequential indexing:

```kite
for i in 0..text.len() {
    let code = text.code_at(i)
}
```

That should not require the generic `Iterator<T>` design that is outside Kite's
roadmap. Kite already gives `for` closed, compiler-known meanings for ranges,
slices, and maps. A later, independent language rule can add:

```kite
for code in text {
    // code: int, one Unicode scalar value
}
```

This is a built-in `str` case, not trait dispatch. The compiler owns an
unobservable cursor:

- the VM/native backend advances a UTF-8 byte cursor and decodes one scalar;
- the Wasm HostRef backend advances a UTF-16 cursor in the JavaScript string;
  and
- each iteration exposes only the scalar `int`, preserving Kite's
  code-point semantics.

The cursor can be implemented by a dedicated internal host primitive returning
the next scalar and cursor (Wasm multi-value or one packed internal value).
It is not a Kite value, cannot be forged by a program, and does not commit the
language to a generic iterator protocol.

Start with one loop binding. Code needing an index can maintain an `int`
counter. That avoids assigning a second, map-like meaning to
`for (a, b) in value` before the language has a general enumeration design.

The standard JSON/TOML scanners and other character-at-a-time library code can
then move to this loop form. Random `code_at(n)` remains available with its
natural linear cost for non-narrow text.

String iteration is useful, but it is not a prerequisite for removing Table:
HostRef should close the memory leak first.

## Required tests before rollout

1. Run every `js_strings` differential program under VM, HostRef, Table during
   transition, Object during transition, and Builtins where supported.
2. Add `pub fn echo(value: str) -> str` and call it through generated
   `api.js`; this catches the Object boundary gap.
3. Exercise `str` through declared `@host` parameters and returns.
4. Cover direct and nested equality: map keys, slices, tuples, errors, and
   `Option<str>`.
5. Repeat the 200 × 200 KiB server test and assert bounded post-GC memory.
6. Parse a large astral-Unicode JSON string and check both result and scaling;
   ASCII-only stress does not exercise code-point/UTF-16 divergence.
7. Assert that default glue contains no append-only `STRINGS`, `INDEX`, or
   runtime interning table.
8. Run the exported-API case through the Vite/compiler-wasm path, not only the
   native CLI, so the selected default is proven at every public entry point.

## When Object would still be preferable

Keep Object only if profiles show that a per-string cache materially beats
HostRef plus linear traversal. In that case it should not become the default
until all of these are complete:

- the `narrow` field is actually read and updated;
- exported `str` functions have generated boundary wrappers;
- string literals are not re-wrapped on every use in hot code;
- every compiler surface can select it; and
- the Table path has been removed.

Without those pieces, HostRef is the smaller fix: it makes the collector see
strings while preserving the boundary the rest of the toolchain already
understands.
