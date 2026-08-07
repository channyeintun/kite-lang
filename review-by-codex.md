# Codex review of the three security-fix commits

Reviewed:

- `b0c9fac` — the initial security fixes
- `38d750d` — the first F20 lifetime fix
- `c440bdd` — the adversarial follow-up

The review used `security-review.md` as the claim set and checked the final
combined state, including paths not covered by the added regression tests.

## Current verdict

The string-lifetime blocker found in this review is now resolved. Wasm has one
language-owned `str`: a traced Unicode-scalar GC array. The permanent host
registry, representation switches, hidden environment selector, engine
proposal flag, and duplicate import ABIs have been removed. Internal string
operations stay in Wasm, and a fixed one-page bridge handles JavaScript
boundaries. Executable regressions cover Unicode operations, large
multi-chunk exports, declared hosts, maps and aggregate equality.

The following findings were review suggestions outside that string migration.
They remain useful follow-up work unless a later commit addresses them.

## Findings

### High — the TOML overflow fix creates a `min_int` exponent trap

The follow-up correctly changed `int_of` to accept
`-9223372036854775808` (`std/toml.kite:1158-1211`). That same value is also
accepted for a float exponent. `power_of_ten` then computes:

```kite
var steps = exp
if steps < 0 {
    steps = 0 - steps
}
```

at `std/toml.kite:1138-1141`. Negating `min_int` cannot be represented, so a
document containing `x = 1e-9223372036854775808` traps in a checked build
instead of returning a parse error. In a release build it wraps and produces a
wrong scale.

Clamp by sign before taking an absolute value—for example, handle
`exp < -400` directly—and add this exact edge beside the integer-boundary
tests.

### High — lock verification happens after unverified bytes are installed

`pkg::run` does not read and compare `kite.lock` until after
`lock_dependencies` returns (`bin/kitec/src/pkg.rs:49-79`). During that call,
`Vendor::build_dir` removes `.kite/vendor/<name>` and copies the newly resolved
candidate into its place (`:345-372`). If the digest disagrees with the lock,
`pkg` exits unsuccessfully, but the rejected bytes remain in the directory
used by later builds.

Resolve, copy, and hash into a staging directory first. Compare the prospective
lock, and only after it matches—or `--update` accepts it—atomically replace
the build directories. Builds should also verify the installed tree against
`kite.lock`, or refuse to claim locked operation.

### Medium — vendoring and hashing follow attacker-controlled symlinks

`copy_dir` uses `Path::is_dir` and `std::fs::copy`
(`bin/kitec/src/pkg.rs:588-606`), both of which follow symlinks. A Git
dependency can contain a directory symlink forming a cycle or leaving its
checkout, causing recursion, local-file copying, or disk exhaustion.
`collect_kite_files` follows directory symlinks similarly while computing the
trusted digest (`crates/kite-driver/src/manifest.rs:513-522`).

Use `symlink_metadata`, reject symlinks in fetched dependency trees, and ensure
every traversed canonical path remains below the checkout root. Use the same
walker for copying and hashing.

### Medium — the frame-pointer guard reads the first flag, not the effective flag

`crates/kite-rt/build.rs:76-99` returns on the first
`force-frame-pointers` option. Rust codegen options are order-sensitive; the
last occurrence is effective. Therefore `yes, no` can pass while compiling
without the required frame chain, and `no, yes` can be rejected despite being
safe.

Retain the last recognized value and decide after every flag has been read.
Test joined and split spellings, both duplicate orders, and explicit negative
values.

### Low — `SourceMap` still has an unchecked span-to-string path

`snippet`, `span_text`, and `text_before` clamp invalid offsets, but
`SourceMap::line_col` forwards the raw `span.start`
(`crates/kite-span/src/lib.rs:178-180`). `SourceFile::line_col` then slices at
that offset (`:93-97`), so an out-of-range or mid-code-point start can still
panic in diagnostic rendering.

Normalize the offset once and use it for both line selection and column
counting.

### Low — the server body ceiling is not measured in bytes

The generated adapter calls `MAX_BODY` a byte limit, but concatenates each
`Buffer` into a JavaScript string and checks `body.length`
(`crates/kite-codegen-wasm/src/serve.rs:67-74`, `:123-136`). That is a UTF-16
code-unit count, and decoding chunks independently can replace a multibyte
UTF-8 character split across chunk boundaries.

Track `chunk.length`, retain bounded buffers, and decode once with
`Buffer.concat` at end-of-stream.

## Additional suggestions

- The JSON and TOML exponent loops still do up to 400 operations per number.
  Reject or underflow out-of-range powers directly, or use exponentiation by
  squaring.
- Normalize dependency-relative path components to `/` before hashing so
  identical nested trees produce identical lock hashes on Windows and Unix.

## Areas that held up

No new correctness issue was found in the scoped alias map, E0403/E0404
collision diagnostics, Vite cache-directory confinement, no-host
accessibility execution, JSON infinity refusal, diagnostic control/bidi
escaping, or the SHA-256 compression implementation.
