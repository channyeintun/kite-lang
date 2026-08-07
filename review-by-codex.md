# Codex review of the three security-fix commits

Reviewed:

- `b0c9fac` — the initial security fixes
- `38d750d` — `Strings::Object`
- `c440bdd` — the adversarial follow-up

The review used `security-review.md` as the claim set and checked the final
combined state, including paths not covered by the new regression tests.

## Verdict

The changes materially improve the repository, but they are not ready to be
treated as a completed security fix. Two issues should block that claim:

1. the leaking `Strings::Table` representation is still selected by every
   ordinary build path; and
2. TOML can still turn a short untrusted number into an uncatchable integer
   overflow trap.

The lockfile work is correctly described as partial in the updated report, but
its current ordering has a further consequence: a rejected dependency has
already replaced the directory that later builds consume.

## Findings

### High — F20 remains reachable through the shipped default

`Strings` still derives `Default` with `Table`
(`crates/kite-codegen-wasm/src/lib.rs:62-70`), and the convenience compiler
still calls `compile_with(..., Strings::Table)` (`:962-966`). The CLI selects
`Object` only through the undocumented `KITE_STRINGS_OBJECT` environment
variable (`bin/kitec/src/main.rs:229-238`).

The compiler-wasm/playground boundary has only a boolean choice between
`Builtins` and `Table` (`crates/kite-playground/src/lib.rs:176-195`), so Vite
and `@kite-lang/compiler-wasm` cannot select `Object` at all. Consequently, the
default generated server remains vulnerable to the exact unbounded-memory
failure F20 describes.

`security-review.md` does record F20 as open. The practical suggestion is to
treat switching every public compilation entry point atomically—and then
removing `Table`—as part of the fix, not as a later optimization. A hidden
environment variable is useful for measurement but is not a mitigation for
users.

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
`exp < -400` directly—and add this exact edge beside the four integer-boundary
tests. This is the same uncatchable-parser failure class F12 and F13 were meant
to close.

### High — lock verification happens after unverified bytes are installed

`pkg::run` does not read and compare `kite.lock` until after
`lock_dependencies` returns (`bin/kitec/src/pkg.rs:49-79`). During that call,
`Vendor::build_dir` removes `.kite/vendor/<name>` and copies the newly resolved
candidate into its place (`:345-372`). If the new digest then disagrees with
the lock, `pkg` exits unsuccessfully, but the rejected bytes remain in the
directory used by `build`, `run`, and `test`.

Those commands do not consult the lock, as the follow-up now documents. Thus a
failed verification does not quarantine the candidate; a later build can
compile it without another warning.

Resolve, copy, and hash into a staging directory first. Compare the complete
prospective lock, and only after it matches (or `--update` explicitly accepts
it) atomically replace the build directories. Builds should also verify the
installed tree against `kite.lock`, or refuse to claim locked operation.

### Medium — `Strings::Object` cannot satisfy the generated JavaScript API

The generated glue explicitly acknowledges that an exported Kite function
taking or returning `str` is unreachable in Object mode
(`crates/kite-codegen-wasm/src/glue.rs:132-142`). The Wasm export expects the
internal GC record, while `api.js` still calls `str(value)` and `text(result)`
as though the wire value were a table index or bare `externref`
(`:2482-2567`).

This is not merely an optional API corner: the Vite integration presents
generated exports as its primary typed boundary. Switching Object to the
default before fixing it would turn valid `pub fn echo(s: str) -> str` APIs
into runtime failures.

Either generate Wasm boundary-wrapper exports that wrap and unwrap the record,
or use a representation whose internal and JavaScript boundary value is
already `externref`. Add an end-to-end API test with both a `str` parameter and
return value for every representation.

### Medium — the Object cache is never used, so sequential scans are quadratic

The Object record reserves a mutable `narrow` field and says the scan happens
once (`crates/kite-codegen-wasm/src/lib.rs:1212-1234`), but construction only
writes `-1` (`:3242-3250`, `:3747-3756`). No code reads or updates that field.

Instead, the Object glue runs `SURROGATE.test(s)` on the entire string on every
`slice` or `code_at` call (`crates/kite-codegen-wasm/src/glue.rs:120-130`,
`:1231-1260`). Builtins mode similarly spreads the whole string to answer the
same question on every call (`:239-248`). A loop that advances with
`code_at(i)` therefore remains O(n²) in both non-table modes. That includes the
new `json.parse_string` scanning direction, so F11 is fixed for the cached
Table path but not for all representations.

Before making Object the default, either wire the record field into the
indexed operations or introduce a linear, compiler-specialized string
traversal. This does not require a generic `Iterator` trait; the separate
proposal describes one option.

### Medium — vendoring and hashing follow attacker-controlled symlinks

This gap predates the three commits, but it is directly in the package and
lockfile trust boundary they modify.

`copy_dir` uses `Path::is_dir` and `std::fs::copy`
(`bin/kitec/src/pkg.rs:588-606`), both of which follow symlinks. A Git
dependency can therefore contain a directory symlink forming a cycle or
leaving its checkout, causing recursion, local-file copying, or disk
exhaustion during `kitec pkg`. `collect_kite_files` follows directory symlinks
in the same way while computing the trusted digest
(`crates/kite-driver/src/manifest.rs:513-522`).

Use `symlink_metadata`, reject symlinks in fetched dependency trees, and ensure
every traversed canonical path remains below the checkout root. Apply the same
walker to copying and hashing so they cannot disagree about the tree being
verified.

### Medium — the frame-pointer guard checks the first flag, not the effective flag

`crates/kite-rt/build.rs:76-99` returns immediately on the first
`force-frame-pointers` option. Rust codegen options are order-sensitive; the
last occurrence is effective. Therefore:

- `-C force-frame-pointers=yes -C force-frame-pointers=no` passes the guard
  while compiling without the required frame chain; and
- the reverse ordering is rejected even though the effective value is safe.

Because this guard is meant to turn a GC memory-safety convention into a build
invariant, it should retain the last recognized value and decide only after
all flags have been read. Unit-test joined and split spellings, both duplicate
orders, and explicit negative values.

### Low — `SourceMap` still has an unchecked span-to-string path

`snippet`, `span_text`, and `text_before` now clamp invalid offsets, but
`SourceMap::line_col` forwards the raw `span.start`
(`crates/kite-span/src/lib.rs:178-180`). `SourceFile::line_col` then slices
directly at that offset (`:93-97`), so an out-of-range or mid-code-point start
still panics in diagnostic rendering.

The known interpolation defect produced a bad end and is fixed at its source,
but the stated defense—bad spans produce bad snippets rather than dead
processes—is incomplete. Normalize the offset once and use it for both line
selection and column counting.

### Low — the server body ceiling is not measured in bytes

The generated adapter says `MAX_BODY` is a byte limit, but concatenates each
`Buffer` into a JavaScript string and checks `body.length`
(`crates/kite-codegen-wasm/src/serve.rs:67-74`, `:123-136`). That is a UTF-16
code-unit count, and decoding each chunk independently can replace a multibyte
UTF-8 character split across chunk boundaries.

Track `chunk.length`, retain bounded Buffers, and decode once with
`Buffer.concat` at end-of-stream. This makes the documented 8 MiB limit exact
and preserves request text.

## Additional suggestions

- The exponent loops are now bounded but still do up to 400 operations per
  number (`std/json.kite:321-332` and `std/toml.kite:1142-1154`). An 8 MiB JSON
  document containing many exponent forms can amplify work substantially.
  Reject/underflow out-of-range powers directly or use exponentiation by
  squaring.
- Normalize dependency-relative path components to `/` before hashing. The
  current `PathBuf::to_string_lossy()` input makes the digest platform-shaped,
  so a nested dependency tree can produce a different lock hash on Windows
  and Unix despite identical files.
- The Builtins error classifier matches any failure message containing
  `js-string` (`crates/kite-codegen-wasm/src/glue.rs:273-304`). A tiny known-good
  capability probe, cached per process, would separate engine capability from
  application-module validation without relabeling either class heuristically.

## Areas that held up

I did not find a new correctness issue in the scoped alias table, E0403/E0404
collision diagnostics, the Vite cache-directory confinement, no-host a11y
execution, JSON infinity refusal, diagnostic control/bidi escaping, or the
SHA-256 compression implementation itself.

## Validation performed

- Read the complete three-commit diff and the final affected code.
- `git diff --check` passed.
- `node --check packages/vite-plugin-kite/index.js` passed under Node
  `v22.23.1`.
- Rust/Kite tests could not be rerun in this environment because `cargo`,
  `rustc`, and a built `kitec` are not installed. The findings above are based
  on static control-flow and representation analysis; the TOML and boundary
  cases should be added as executable regressions when the toolchain is
  available.
