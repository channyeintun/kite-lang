# Language-owned WebAssembly strings

Status: implemented.

## Decision

Kite's WebAssembly `str` is one WasmGC array of Unicode scalar values:

```wat
(array (mut i32))
```

One element is one Unicode scalar value. The mutability is private
construction machinery for `array.set` and `array.copy`; no Kite operation can
mutate an existing string.

This is the sole Wasm representation. It is not selected by a CLI flag,
environment variable, playground option, or bundler setting.

## Why scalar values

Kite specifies `len`, `slice`, `index_of`, and `code_at` in characters, not
UTF-8 bytes or UTF-16 code units. A scalar array makes that contract direct:

- `len` is `array.len`;
- `code_at(i)` is one bounds check and `array.get`;
- `slice` allocates the exact result and uses `array.copy`;
- sequential `for i in 0..body.len()` access is linear;
- equality and ordering compare scalar values in Wasm; and
- a future string iterator can walk the same array without adding a generic
  `Iterator` feature to the language.

UTF-8 would be more compact for mostly-ASCII text, but direct indexed access
would require rescanning or maintaining an additional index. Kite's existing
public semantics favor the scalar layout. A packed or rope representation can
be considered later behind the same language contract if profiles justify it.

## Lifetime

Every dynamic result is a GC array referenced by the Kite value that owns it.
When the last reference disappears, the WebAssembly engine can collect the
array. No integer handle, append-only registry, interning map, hidden cache, or
high-water linear-memory arena keeps it alive.

Literals are emitted directly with `array.new_fixed`; they never cross the host
and require no imported globals.

## Operations

The compiler emits a small internal runtime for:

- host conversion in both directions;
- concatenation;
- equality and scalar ordering;
- length, slice, search, trim, and indexed access; and
- construction from a scalar value.

Map keys and generated aggregate equality call the same internal equality
function. Primitive interpolation formatting remains a host concern, because
it shares formatting with host printing; its returned JavaScript string is
immediately converted into a Kite array.

## JavaScript boundary

JavaScript strings are an ABI format only. The generated glue imports one
`WebAssembly.Memory` with `initial: 1` and `maximum: 1`. Conversion uses the
first 16 KiB in chunks of 4,096 scalar values:

1. `text_len(value)` counts JavaScript code points and starts a short-lived
   iterator.
2. `text_fill(count)` writes the next chunk as little-endian `u32` values.
3. Wasm copies them into a newly allocated GC array and clears the iterator at
   the end.
4. In the other direction Wasm copies one chunk into the page and
   `text_push(count, first, last)` builds bounded JavaScript chunks.
5. The final call joins the chunks and clears the temporary list.

The page cannot grow, and no converted value remains rooted after the
synchronous call. Large values are not spread into an unbounded JavaScript
argument list.

Declared hosts receive and return ordinary JavaScript strings. Public Kite
exports keep their canonical Wasm signatures; two private conversion exports
power the generated `str(value)` and `text(value)` API helpers.

## Validation

Regression coverage includes:

- Wasm validation for every generated runtime body;
- Unicode literals, astral scalars, slicing, searching, trimming, comparison,
  and invalid scalar construction;
- dynamic concatenation and formatting;
- strings in maps, slices, optionals, structs, and generated deep equality;
- declared hosts receiving and returning JavaScript strings;
- an exported 5,000-emoji value crossing both directions in multiple chunks;
- a literal longer than one `array.new_fixed` chunk; and
- a lone surrogate arriving from JavaScript.

## What the review of the implementation found

The design held up. The bridge cannot interleave two conversions — every
emitted host call converts each argument to completion before invoking the
host and converts the return afterwards, so conversions nest rather than
overlap — and every `array.get`/`set`/`copy` the compiler emits is preceded
by an exact clamp. Equality, ordering and map keys compare contents
everywhere; no reference-identity comparison survived from the handle era.

Four things did not hold up, and all four are fixed.

**The generated server adapter was never migrated.** `serve.rs` still wrapped
its host returns in `str(...)` and its parameters in `text(...)`. Those are
the *public* API's helpers, for callers outside the module; a declared host
exchanges plain JavaScript strings, because the module converts around each
call. Every request died in `textLength` with "Cannot convert object to
primitive value" — so the commit shipped with the four tests in
`crates/kite-driver/tests/strings.rs`' sibling `serve.rs` red, and they pass
at the parent commit. The lesson is narrower than "test more": the adapter's
only coverage was those integration tests, and everything else asserting
about generated JavaScript asserts that a substring is present, which no
behavioural break can fail.

**A literal over 10,000 scalars produced a module no engine would take.**
`array.new_fixed` draws its elements from the operand stack and V8 caps it at
10,000 — and past the cap the module is refused at *instantiation*, not at
validation, so `wasmparser` called it well-formed and every validity
assertion passed. Long literals are now built a chunk at a time and joined
with the runtime's own `concat`, balanced rather than left-to-right.

**Lone surrogates could enter the array.** A JavaScript string is UTF-16 and
may hold half a pair; a Unicode scalar value cannot be one. `from_code`
already refused the range, so the host bridge was the only ingress — and it
copied the code unit straight through, giving a `str` the VM and native
backends cannot represent, which made the same program compare and hash
differently depending on where it ran. The bridge now substitutes U+FFFD.

**The size floor quadrupled and the budget test was left red.** A module that
prints went from 399 to 1,625 bytes; a program containing no strings at all
is 1,619, so the cost is the runtime's presence rather than any program's use
of it. All twelve runtime functions and the three bridge imports are emitted
unconditionally, because the module exports `str` and `text` for its
JavaScript API whether the program handles text or not. That is a real
consequence of the design, not a bug — but emitting only what a program can
reach would give most of it back, and has not been done. The budget is raised
to 2,048 with that written down beside it.

Left alone, and worth knowing: locally built artifacts still embed the old
ABI until they are rebuilt — `packages/kite-wasm/kite-compiler.wasm`, the
site's modules, and `examples/page/app.wasm`. None are tracked, so the
repository is clean, but an npm package or a page built from a stale copy
compiles with the representation this document replaced.
