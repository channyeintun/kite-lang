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
- declared hosts receiving and returning JavaScript strings; and
- an exported 5,000-emoji value crossing both directions in multiple chunks.
