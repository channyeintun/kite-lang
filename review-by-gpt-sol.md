# Kite implementation review against `SPECIFICATION.md`

Reviewed at commit `245b126` on 2026-08-03.

## Executive summary

The implementation does not currently conform to the specification. The most serious issues are in the error-taint analysis: control-flow joins are unsound, and merely passing an error as an ordinary argument counts as handling it. Together, those defects defeat the language's central guarantee that a value paired with an error cannot be used until the error has been proved absent or explicitly checked.

There are also observable semantic differences between the VM, native, and Wasm backends, including integer overflow, nil-error method calls, and Unicode string indexing. Trait implementation checking can admit methods with incompatible signatures, `defer` does not preserve the specified evaluation order, and `var self` is parsed but not enforced.

Several larger specification areas are still absent or intentionally implemented with different semantics: sized numeric types and fixed arrays, the full error trait/object model, by-reference closures, native/VM parallelism, structural equality for every type, and Wasm source maps.

## Findings

### 1. Critical: error-taint state is not restored or merged across loops and `match`

The type checker snapshots definite-initialization state around control flow, but does not do the same for error-taint state.

- `for_stmt` saves and restores only `entry_init` (`crates/kite-types/src/lib.rs:1509-1602`).
- `match_expr` likewise restores initialization before each arm and after the match, but never restores or merges taint (`crates/kite-types/src/lib.rs:4657-4746`).
- `check` calls `mark_checked`, which changes the paired value from `Tainted` to `Clean` (`crates/kite-types/src/lib.rs:4070`, `4121-4131`).

Consequently, a `check` in a loop body that never executes can make a value usable after the loop:

```kite
fn source() -> (int, error) {
    return _, errors.new("no value")
}

fn bad() -> (int, error) {
    let (value, err) = source()
    for false {
        check err
    }
    return value, nil
}
```

The same state leak occurs between `match` arms and after a `match`: checking an error in one arm cleans the paired value in every other arm.

This violates the error rules in §7.3, especially R2, R4, and R5. Taint must be part of the control-flow state. Each branch/body should begin from an entry snapshot, and outgoing states must be merged conservatively. A loop body that may execute zero times cannot make an entry-tainted value clean after the loop.

### 2. Critical: any read of an error is treated as handling it

When a path expression reads an unchecked error local, the checker immediately changes that local to `Clean` (`crates/kite-types/src/lib.rs:2490-2497`). `report_unchecked_errors` only diagnoses locals that remain `Unchecked` (`crates/kite-types/src/lib.rs:4138-4157`).

This means an error can be silently discarded by passing it to an unrelated function:

```kite
fn ignore(err: error) {
}

fn bad() {
    let (_, err) = source()
    ignore(err)
}
```

No proof has established that `err` is nil, and no `check` has propagated it, yet the implementation considers the error handled. This directly defeats §7.3 R3 and the specification's stated no-silently-discarded-errors guarantee.

Reading an error must not mutate its taint state. Only semantically recognized proof operations (`err == nil`, `err != nil`, appropriate pattern tests) or `check` should mark it handled.

### 3. High: the canonical `check errors.wrap(err, ...)` form does not clean the paired value

The specification and `std/errors.kite` show `check errors.wrap(err, "...")` as the standard context-wrapping pattern. However, `check_stmt` calls `mark_checked` on the whole checked expression (`crates/kite-types/src/lib.rs:4070`), while `mark_checked` recognizes only a direct path and immediately returns for a call expression (`crates/kite-types/src/lib.rs:4121-4124`).

Reading `err` inside `errors.wrap` happens to mark the error local clean because of finding 2, but its paired value remains tainted. A subsequent use of that value therefore raises E0301:

```kite
let (value, err) = source()
check errors.wrap(err, "while loading")
use(value) // incorrectly rejected as tainted
```

The checker should model error-preserving/wrapping expressions explicitly and associate the checked result with the originating error/value pair. It should not rely on incidental path reads.

### 4. High: `defer` evaluates at the wrong time and can read an uninitialized guard

The lowering stores a cloned call expression for later execution (`crates/kite-types/src/lib.rs:1204-1250`). It does not evaluate and save the receiver and arguments when the `defer` statement is reached, despite the nearby comment describing that behavior. Later changes to locals are therefore visible to the deferred call, contrary to §9.4.

Return lowering also inserts deferred statements before the original return statement (`crates/kite-types/src/lib.rs:1258-1284`). As a result, the return expression is evaluated after deferred calls, so a deferred mutation can change the returned value. The specification requires return values to be evaluated first.

There is a third problem for conditional registration. Each defer guard is initialized only inside the registration block. If that block is skipped, function-exit lowering still reads the guard:

```kite
fn f(run: bool) {
    if run {
        defer io.print("done")
    }
}

fn main() {
    f(false)
}
```

VM registers begin as `Unit`, so using the untouched guard as a boolean can trap; Wasm locals default to zero, so the same source can behave differently across backends.

Lowering should initialize every guard to false in the function entry, evaluate deferred-call operands into hidden temporaries at registration time, set the guard true after successful registration, and evaluate return operands before running the deferred stack.

### 5. High: trait implementations are accepted without type-compatible method signatures

`check_impls` validates whether the trait and implementation agree about a `self` receiver and parameter count, but does not compare parameter types, return types, or fallibility (`crates/kite-types/src/lib.rs:6503-6661`).

For example, an implementation shaped like this can pass the structural checks:

```kite
trait T {
    fn convert(self, value: int) -> int
}

struct S {
    n: int
}

impl T for S {
    fn convert(self, value: str) -> str {
        return value
    }
}
```

Virtual calls are typed using the trait declaration, while dispatch reaches the concrete implementation. The Wasm dispatcher is also built with the trait signature and directly calls the implementation. An incompatible implementation can therefore produce an invalid Wasm call signature or type-confused execution in another backend.

The compiler must substitute trait type parameters and `Self`, then require an exact compatible signature for every implementation method, including receiver form, generic arity, parameter types, return tuple, and error/fallibility shape.

### 6. High: `var self` is parsed but receiver mutability is not enforced

The parser records `SelfParam.is_var` (`crates/kite-parser/src/lib.rs:613-620`), but the resolver reduces the receiver to a `takes_self` boolean and creates the `self` local as immutable (`crates/kite-resolve/src/lib.rs:882-893`). Receiver mutability is not retained in the resolved method signature.

Field assignment checks whether the field is declared mutable, but not whether the current receiver is `var self` (`crates/kite-types/src/lib.rs:5823-5912`). Method calls similarly do not require a mutable caller binding. Existing differential-test source even calls a `var self` method through a `let` binding.

This allows both directions of the §8.2 contract to be violated:

- A method written with plain `self` can mutate a mutable field.
- A `var self` method can be called through an immutable binding.

Receiver mutability needs to survive parsing, resolution, HIR signatures, trait matching, and call checking. Assignment through `self` should require a `var self` receiver, and invoking such a method should require a mutable addressable base.

### 7. High: integer-overflow behavior differs by backend and ignores build mode

Section §3.1 requires integer overflow to trap in debug builds and wrap in release builds.

- Wasm emits plain `i64.add`, `i64.sub`, and `i64.mul`, which always wrap (`crates/kite-codegen-wasm/src/lib.rs:3100-3107`).
- The VM uses checked arithmetic and traps on overflow in all modes (`crates/kite-vm/src/lib.rs:621-649`, `713-716`).
- The native backend also emits overflow traps and documents the release-mode divergence in a comment (`crates/kite-codegen-clif/src/lib.rs:1405-1421`).
- The driver passes `release` to the checker but not to backend code generation (`crates/kite-driver/src/lib.rs:389`, `431-485`).

Thus debug Wasm silently wraps while debug VM/native trap, and release VM/native still trap instead of wrapping. Build mode must be carried into every backend, with equivalent checked or wrapping operations selected consistently.

### 8. High: calling an error method on nil has backend-dependent behavior

The checker permits `.message()` on any value of type `error` without requiring a non-nil proof (`crates/kite-types/src/lib.rs:3327-3346`):

```kite
let err: error = nil
io.print(err.message())
```

The VM returns an empty string for nil (`crates/kite-vm/src/lib.rs:598-600`), while Wasm performs a non-null reference cast and traps (`crates/kite-codegen-wasm/src/lib.rs:2294-2302`).

The language needs one specified behavior. The most consistent choice with §7 is to reject the method call until control flow proves the error non-nil. Whichever behavior is selected must be implemented identically in all backends.

### 9. Medium: Wasm `str.index_of` returns UTF-16-dependent indices

The JavaScript glue gets a UTF-16 code-unit offset from `String.indexOf`, then applies that offset as the element count of a code-point array (`crates/kite-codegen-wasm/src/glue.rs:1120-1123`).

For `"😀a".index_of("a")`, JavaScript returns offset 2. The current conversion also returns 2, but the Kite character index is 1. VM/native string traversal returns the character-based result.

The prefix must be sliced in UTF-16 space first and only then counted as code points, for example conceptually `[...source.slice(0, offset)].length`. Add differential tests with astral characters before both matching and non-matching substrings.

### 10. Medium: a Unicode block string can panic the lexer

Triple-quoted string scanning checks `self.src[self.pos..]` on every iteration but advances `self.pos` by one byte (`crates/kite-lexer/src/lib.rs:401-416`). After encountering a multibyte UTF-8 character, `self.pos` is not a character boundary, and slicing the Rust `str` at that offset panics.

A valid source file such as a block string containing `é`, Burmese text, or an emoji can therefore crash the compiler instead of producing a token or diagnostic. Advance by `char.len_utf8()`, or scan the delimiter using the underlying byte slice without constructing an invalid `str` slice.

### 11. Medium: identifiers are not NFC-normalized

Section §2.1 requires identifiers to be normalized to NFC. The parser copies the original identifier spelling directly into a `String` (`crates/kite-parser/src/lib.rs:284-295`), and resolution uses those raw strings as names. No Unicode-normalization implementation or dependency is present.

Consequently, canonically equivalent identifiers such as `café` written with U+00E9 and `café` written with `e` plus U+0301 are treated as different names. Normalize identifiers once at the lexer/parser boundary while retaining the original source span for diagnostics.

### 12. Medium: newline continuation excludes two operators contrary to §2.5

The lexer explicitly excludes `>` and `>>` from the set of operators after which a newline continues an expression (`crates/kite-lexer/src/kinds.rs:92-119`). Section §2.5 says a newline continues after any operator.

This makes otherwise equivalent formatting behave differently:

```kite
let a = left >=
    right // continues

let b = left >
    right // terminates unexpectedly
```

If the exception exists to disambiguate generic closing brackets, that decision needs parser context rather than a source-level semantic exception, or the specification must explicitly document the exception.

## Material specification areas not implemented

These are broader gaps rather than isolated defects:

| Specification area | Current implementation |
| --- | --- |
| Sized numeric types, `byte`, `char`, and fixed arrays (§3.1-§3.2) | The core type table exposes only unsized `int`/`float`; numeric suffixes are stripped and still produce those types, character expressions are explicitly unimplemented, and there is no `[N]T` fixed-array type (`crates/kite-hir/src/ty.rs:220-231`, `859-868`; `crates/kite-types/src/lib.rs:1853-1863`, `1981-1983`, `7037-7067`). |
| Full error model (§7.2, §7.6) | Errors are a built-in record centered on a message. Concrete user error types, the specified error trait-object behavior, `errors.is`, `errors.as`, and cause-chain semantics are absent. `std/errors.kite` describes `mentions` as a temporary stand-in. |
| By-reference closures (§4.4) | Captures are by value, and mutable captures are rejected instead of being promoted and captured by reference (`crates/kite-types/src/lib.rs:2157-2167`, `2388-2413`). |
| Trait `Self`, generic methods, and full object-safety rules (§10) | The implementation covers a smaller trait subset. Method generic parameters and the complete restrictions involving `Self` are not represented or validated. |
| Structural equality for every type (§5.2, §10.4) | The checker restricts equality to a subset and omits at least maps, functions, and dynamic trait values (`crates/kite-hir/src/ty.rs:691-710`). |
| Real task parallelism on native and bytecode targets (§12.3) | `std/task.kite` explicitly implements sequential execution/yielding and states that no target currently runs tasks in parallel. |
| Wasm source maps (§16) | The Wasm artifact contains module bytes, glue, HTML, and host metadata, but no source-map output; the CLI writes no `.map` file (`crates/kite-codegen-wasm/src/lib.rs:501-510`, `1347-1362`; `bin/kitec/src/main.rs:229-255`). |

## Specification ambiguities found during review

The specification itself contains a few conflicting directions that should be resolved before treating it as a conformance oracle:

- §10.4 says Kite intentionally has no `json.decode<T>` and directs types to provide `decode`, while §12.2 and Appendix A use `json.decode<User>` and `json.decode<[Task]>`.
- §12.1 says channels are intentionally absent, while §12.4 lists `sync.Channel` as a standard-library wrapper.

## Verification notes

This review is based on static tracing across the lexer, parser, resolver, type checker, standard library, driver, VM, native backend, and Wasm backend. I also confirmed the JavaScript index conversion independently with Node: for `"😀a"`, the implementation's formula produces 2 while the character index is 1.

I could not run `cargo test --workspace` because neither `cargo` nor `rustc` is installed in the review environment. Findings above are therefore grounded in directly cited code paths and small semantic traces, but the proposed reproducers should be added as compiler and cross-backend differential tests once a Rust toolchain is available.
