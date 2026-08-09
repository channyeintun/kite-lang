# Error handling in Kite

Kite keeps Go's `(T, error)` shape and makes it enforceable. Go's shape is right
— errors are ordinary values, every failure point is visible, nothing unwinds —
and its enforcement is what Kite adds: in Go an error can be dropped silently
(`v, _ := f()`, or just never testing `err`), the value on the failure path is a
valid-looking zero value that flows onward, and nothing checks that you handled
the failure at all. Here each of those is a compile error.

The deltas that will break your assumptions, in the order you will hit them:

1. **A two-value return exists only for fallibility.** `return a, b` is a compile
   error unless the function's second return component is `error`.
2. **On the failure path there is no value at all.** Not a zero value, not `nil`
   — the value slot is unreadable, and reading it is `E0301`.
3. **An error cannot be dropped**, except two ways the analysis cannot see. An
   uninspected `err` from a destructuring, `_` in the error slot, and a bare call
   statement are all `E0302`; binding a bare `-> error` result, or a whole pair,
   to a single name is not caught. See *An error cannot be dropped* below.
4. **`check err` is a statement, not a postfix `?`.** It is greppable and it
   occupies its own line, by design.
5. **`err.message()` needs a proof that `err` is not nil.** An untested `error`
   has no message, and asking for one is `E0301`.
6. **Traps are not catchable.** No `panic`/`recover`, no unwinding, no handler.

Everything below was checked against `target/release/kitec`.

## The canonical shape

```kite
fn divide(a: int, b: int) -> (int, error) {
    if b == 0 {
        return _, errors.new("division by zero")
    }
    return a / b, nil
}

fn ratio(a: int, b: int) -> (int, error) {
    let (q, err) = divide(a, b)
    check err
    let (scaled, err) = divide(q * 1000, 10)
    check err
    return scaled, nil
}

fn main() {
    let (r, err) = ratio(10, 2)
    if err != nil {
        io.print("failed: " + err.message())
    } else {
        io.print(r)
    }
}
```

Three things to read off it: `_` in the value slot of a `return` means *no value*
(it is not a zero value); `check err` propagates; and `err` may be rebound in the
same scope, which no other binding may be.

`_` is not checked against the error you return beside it. `return _, nil`
compiles, and the caller — holding a nil, Checked error — is then allowed to read
the hole and gets whatever the slot holds: an `int` renders as `nil`, a `str`
traps at first use. Write `_` only beside an error you know is non-nil.

## The `error` type

`error` is a built-in nil-able type: either nil, or a value describing a failure.
It is the only nil-able thing besides `Option<T>`. The prelude declares

```kite ignore
pub trait Error {
    fn message(self) -> str
}
```

and any struct or enum may implement it. A value of such a type is accepted
wherever `error` is expected; the conversion happens at that point and keeps the
original value and its type tag alongside the rendered message.

Two constructors are compiler builtins and need no import:

- `errors.new(message: str) -> error`
- `errors.because(message: str, cause: error) -> error`

Everything else lives in `std/errors` and needs `use std/errors`.

### `err.message()` requires a non-nil proof

This is the single most common mistake. `error` is nil-able, so the compiler
refuses `.message()` until the binding has been narrowed:

```kite fails
fn main() {
    let e = errors.new("boom")
    io.print(e.message()) //~ E0301
}
```

The diagnostic reads ``` `e` may be nil here, so it has no message ```. Narrow it
first — with `if e != nil`, or by leaving early:

```kite
fn main() {
    let e = errors.new("boom")
    if e != nil {
        io.print(e.message())
    }
    let f: error = nil
    if f == nil {
        io.print("nothing failed")
    }
}
```

The proof is tracked per *binding*. A call result used directly —
`errors.new("x").message()` — is not tracked and compiles; on a nil call result
that yields an empty string at runtime rather than a diagnostic. Bind it.

### `error` is not printable

`io.print` takes `int`, `float`, `bool`, `str`, and anything implementing
`Display`. `error` implements none of them:

```kite fails
fn main() {
    let e = errors.new("boom")
    if e != nil {
        io.print(e) //~ E0200
    }
}
```

Print `e.message()`, or interpolate it: `"\(e.message())"`.

## A pair return is only ever for fallibility

`return a, b` — two expressions after one `return` — is the *fallible pair* form.
It requires the function's last return component to be `error`:

```kite fails
fn pair() -> (int, int) {
    return 1, 2 //~ E0200
}

fn main() {
    let (a, b) = pair()
    io.print(a + b)
}
```

> `E0200: returning a pair from a function that is not fallible`

The specification implies `-> (int, int)` is itself rejected. **The compiler
accepts it** — as an ordinary tuple type. What is rejected is the two-value
`return` statement inside it. Write a tuple literal instead:

```kite
fn pair() -> (int, int) {
    return (1, 2)
}

fn main() {
    let p = pair()
    io.print(p.0 + p.1)
    let (a, b) = pair()
    io.print(a + b)
}
```

So the rule to hold in your head is: **`(T, error)` is a pair, `(A, B)` is a
tuple, and they behave differently.** A pair is not a tuple —

- it has no fields: `p.0` on a `(int, error)` is `E0200`;
- it is not built with a tuple literal: `return (1, nil)` in a fallible function
  is `E0203` (*a fallible function returns two values … only one value returned*);
- it has exactly two components. `-> (int, str, error)` is not a fallible
  signature; `return 1, "a", nil` in it is `E0200` and then a parse error.
- `-> error` alone is also a fallible signature, and is enough for `check`.

## Taint analysis

After a fallible call the compiler runs a forward dataflow pass over the CFG,
tracking two flow-sensitive states. It is not a borrow checker: no ownership, no
aliasing, no lifetimes.

| | rule |
|---|---|
| R1 | after `let (v, e) = f()`, `e` is **Unchecked** and `v` is **Tainted**. A destructuring is the *only* thing that makes a binding Unchecked — `let e = f()` on a `-> error` function makes none |
| R2 | reading a Tainted binding is `E0301` |
| R3 | an Unchecked binding going out of scope is `E0302` |
| R4 | on a path where `e == nil` is proved, `e` becomes Checked and `v` Clean |
| R5 | on a path where `e != nil`, `e` becomes Checked and `v` stays Tainted **permanently** |
| R6 | a bare-statement call whose type is `error` or `(T, error)` is `E0302` |

R2 in practice:

```kite fails
fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let (v, err) = load()
    io.print(v) //~ E0301
    if err != nil {
        io.print(0)
    }
}
```

R5 is the part people miss — inside the `err != nil` branch the value is *still*
unreadable, and stays unreadable for the rest of that path:

```kite fails
fn f() -> (int, error) { return 1, nil }

fn g() -> (int, error) {
    let (v, err) = f()
    if err != nil {
        io.print(v) //~ E0301
        return _, err
    }
    return v, nil
}

fn main() {
    let (v, e) = g()
    if e != nil {
        io.print(e.message())
    } else {
        io.print(v)
    }
}
```

## An error cannot be dropped

Six spellings of "drop it": three rejected, one deliberate, two holes.

**Never inspected** (R3) — the error slot of a destructuring:

```kite fails
fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let (v, err) = load() //~ E0302
    io.print(1)
}
```

**`_` in the error slot of a destructuring** — the Go `v, _ := f()` habit:

```kite fails
fn f() -> (int, error) { return 1, nil }

fn main() {
    let (v, _) = f() //~ E0302
    io.print("hi")
}
```

> `E0302: an error may not be discarded with `_` — the error slot cannot be dropped`

**A bare-statement call** (R6). This is the door the binding rules do not watch,
and it is the ordinary shape in `std/dom`, where nearly every function answers
with a bare `error`:

```kite fails
fn touch() -> error {
    return errors.new("no")
}

fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    touch() //~ E0302
    load() //~ E0302
}
```

**`_ = …`, which is allowed** — the one way to throw an error away, chosen so
that it is a line a reader sees and `grep` finds:

```kite
fn touch() -> error {
    return errors.new("no")
}

fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    _ = touch()
    _ = load()
    io.print("done")
}
```

Note the asymmetry: `_ = f()` discards the *whole* call and is fine; `_` standing
in for the error inside a destructuring is not. `_ = …` takes only `=` — there is
no `_ +=`, since that would read the hole.

### The two holes

Both are cases where nothing is destructured, so R1 never makes an Unchecked
binding and R3 has nothing to fire on. **Neither is a diagnostic — this compiles
and silently throws two failures away:**

```kite
fn touch() -> error {
    return errors.new("no")
}

fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let e = touch()     // a lone `error` binding is never Unchecked
    let p = load()      // the whole pair under one name
    io.print("both errors are gone")
}
```

The first is the dangerous one, because `let e = dom.set_text(…)` is exactly what
you write by habit and `std/dom` answers with a bare `error` nearly everywhere.
Only the *statement* form `dom.set_text(…)` is caught (R6); naming the result
buys silence. Bind it and test it, or write `_ = …` and mean it.

The second binding is close to useless anyway — `p` has no fields (`E0200`) and
no methods (`E0205`) — and `let (v, e) = p` afterwards re-enters the normal
rules. Do not reach for either.

## `check`

`check err` is exactly, in a `-> (T, error)` function:

```kite ignore
if err != nil {
    return _, err
}
```

and in a `-> error` function, `if err != nil { return err }`. Its operand must
have type `error` — `check x` on an `int` is `E0200: `check` needs an `error``.

A bare `-> error` return is fallible enough for `check`:

```kite
fn touch(x: int) -> error {
    if x < 0 {
        return errors.new("negative")
    }
    return nil
}

fn twice(x: int) -> error {
    let err = touch(x)
    check err
    check touch(x + 1)
    return nil
}

fn main() {
    let e = twice(-1)
    if e != nil {
        io.print(e.message())
    }
}
```

`check` in a function that is not fallible is `E0303`. `main` is not fallible, so
this is the error you will hit first when writing a scratch program:

```kite fails
fn f() -> (int, error) { return 1, nil }

fn main() {
    let (v, err) = f()
    check err //~ E0303
    io.print(v)
}
```

In `main`, test the error instead. (`fn main() -> error` *is* accepted by the
compiler, but a non-nil error returned from it is silently discarded and the
process still exits 0 — do not rely on it.)

`check` composes inside loops, and the value is Clean after it:

```kite
fn f(n: int) -> (int, error) {
    if n == 2 {
        return _, errors.new("two")
    }
    return n, nil
}

fn sum() -> (int, error) {
    var total = 0
    for i in 0..4 {
        let (v, err) = f(i)
        check err
        total = total + v
    }
    return total, nil
}

fn main() {
    let (t, err) = sum()
    if err != nil {
        io.print("failed: " + err.message())
    } else {
        io.print(t)
    }
}
```

### Rebinding the error

Same-scope shadowing is `E0112` in Kite — except for an error binding that is
provably Checked, which is what makes the `check`-per-step idiom readable. The
exception is by *type*, not by the name `err`:

```kite
fn f(n: int) -> (int, error) { return n, nil }

fn g() -> (int, error) {
    let (a, oops) = f(1)
    check oops
    let (b, oops) = f(2)
    check oops
    return a + b, nil
}

fn main() {
    let (v, e) = g()
    if e != nil {
        io.print(e.message())
    } else {
        io.print(v)
    }
}
```

Rebinding before checking is still `E0302` — the first error is gone:

```kite fails
fn f(n: int) -> (int, error) { return n, nil }

fn g() -> (int, error) {
    let (a, err) = f(1) //~ E0302
    let (b, err) = f(2)
    check err
    return b, nil
}

fn main() {
    let (v, e) = g()
    if e != nil {
        io.print(e.message())
    } else {
        io.print(v)
    }
}
```

And the value binding gets no such exception:

```kite fails
fn f(n: int) -> (int, error) { return n, nil }

fn g() -> (int, error) {
    let (a, e1) = f(1)
    check e1
    let (a, e2) = f(2) //~ E0112
    check e2
    return a, nil
}

fn main() {
    let (v, e) = g()
    if e != nil {
        io.print(e.message())
    } else {
        io.print(v)
    }
}
```

## Handling a failure in place

**The specification's example for this (§7.5) does not compile.** It writes

```kite fails
fn get_int(k: str) -> (int, error) {
    if k == "port" {
        return 8080, nil
    }
    return _, errors.new("no key \(k)")
}

fn main() {
    let (p, err) = get_int("prt")
    let port = if err != nil { 80 } else { p } //~ E0301
    io.print(port)
}
```

The taint states are joined at statement granularity, so the `else` arm of an
`if`-*expression* does not see `p` as Clean. Use a statement `if`/`else`:

```kite
fn get_int(k: str) -> (int, error) {
    if k == "port" {
        return 8080, nil
    }
    return _, errors.new("no key \(k)")
}

fn main() {
    let (p, err) = get_int("prt")
    if err != nil {
        io.print(err.message())
    } else {
        io.print(p)
    }

    var port = 80
    let (q, e2) = get_int("port")
    if e2 == nil {
        port = q
    }
    io.print(port)
}
```

…or an early return, which is the cleanest and leaves the rest of the function
working with a Clean value:

```kite
fn get_int(k: str) -> (int, error) {
    if k == "port" {
        return 8080, nil
    }
    return _, errors.new("no key \(k)")
}

fn main() {
    let (p, err) = get_int("port")
    if err != nil {
        io.print("failed: " + err.message())
        return
    }
    io.print(p)
}
```

## Adding context

`errors.wrap(err, context)` returns nil for nil, so it composes with `check` on
one line. It is built on `errors.because`, so it **keeps** what it wrapped rather
than flattening it into text.

```kite
use std/errors

fn f(n: int) -> (int, error) {
    if n < 0 {
        return _, errors.of("input", "must not be negative")
    }
    return n * 2, nil
}

fn g(n: int) -> (int, error) {
    let (v, err) = f(n)
    check errors.wrap(err, "doubling \(n)")
    return v, nil
}

fn main() {
    let (v, err) = g(-1)
    if err != nil {
        io.print(err.message())                       // doubling -1: input: must not be negative
        io.print(join(errors.chain(err), " <- "))     // outermost first
        io.print(errors.mentions(err, "negative"))    // true
        io.print(errors.message_or(err, "none"))
        let root = errors.root(err)
        if root != nil {
            io.print(root.message())                  // input: must not be negative
        }
        let c = err.cause()
        if c != nil {
            io.print("caused by: " + c.message())
        }
    } else {
        io.print(v)
    }
}
```

`err.cause()` is an `error`, **not** an `Option<error>` — `error` is already
nil-able and two ways to say absent is one too many. It is nil when nothing was
wrapped. `std/errors` gives you `chain` (every message, outermost first), `root`
(the innermost failure), `of(subject, problem)`, `mentions(err, needle)` and
`message_or(err, absent)`; `chain` and `root` are bounded at 64 links.

## Errors that carry a type

An error made from an `impl Error` value keeps that value and its type tag, so a
caller four layers up can ask *which* failure this was instead of matching on
text. There is no turbofish in Kite, so the type names itself at the front:
`NotFound.is(err)` and `NotFound.as(err)`. `as` is a keyword, admitted here
because after a `.` it can only be a member name.

```kite
struct Config { path: str }

struct NotFound {
    resource: str
    id: str
}

impl Error for NotFound {
    fn message(self) -> str {
        return "\(self.resource) \(self.id) not found"
    }
}

fn load(id: str) -> (Config, error) {
    if id == "app" {
        return Config{ path: "app.toml" }, nil
    }
    return _, NotFound{ resource: "config", id: id }
}

fn main() {
    let (cfg, err) = load("nope")
    if err != nil {
        io.print(err.message())
        if NotFound.is(err) {
            io.print("that was a NotFound")
        }
        let hit = NotFound.as(err)      // Option<NotFound>
        if hit != nil {
            io.print("missing id: " + hit.id)
        }
    } else {
        io.print(cfg.path)
    }
}
```

`T.as(err)` returns `Option<T>`, spelled `Option<T>` — there is no `?T` syntax;
`?` is not even a Kite token. Narrow it with `if hit != nil { … }`.

An enum works as well as a struct, and pairs nicely with `match`:

```kite
enum Fault {
    Timeout
    Refused(code: int)
}

impl Error for Fault {
    fn message(self) -> str {
        return match self {
            Timeout => "timed out",
            Refused(c) => "refused with \(c)",
        }
    }
}

fn dial(port: int) -> (int, error) {
    if port == 0 {
        return _, Timeout
    }
    if port < 0 {
        return _, Refused(code: port)
    }
    return port, nil
}

fn main() {
    let (v, err) = dial(0)
    if err != nil {
        io.print(err.message())
        let f = Fault.as(err)
        if f != nil {
            io.print(match f {
                Timeout => "retry",
                Refused(c) => "gave up: \(c)",
            })
        }
    } else {
        io.print(v)
    }
}
```

A struct that has not declared itself an error is `E0200`, and the diagnostic
tells you to write the `impl`:

```kite fails
struct Plain {
    x: int
}

fn f() -> (int, error) {
    return _, Plain{ x: 1 } //~ E0200
}

fn main() {
    let (a, e) = f()
    if e != nil {
        io.print(e.message())
    } else {
        io.print(a)
    }
}
```

### `is`/`as` do not walk the cause chain

`T.is` is a single tag comparison on the error you hand it. Wrapping produces a
*new* error carrying no typed value, so the test must be applied to the root:

```kite
use std/errors

struct Filler { z: int }

struct NotFound { id: str }

impl Error for NotFound {
    fn message(self) -> str {
        return "no \(self.id)"
    }
}

fn main() {
    let base: error = NotFound{ id: "7" }
    let w = errors.wrap(base, "loading")
    if w != nil {
        io.print(NotFound.is(w))                  // false — wrapping loses the tag
        let r = errors.root(w)
        if r != nil {
            io.print(NotFound.is(r))              // true
        }
    }
}
```

**Compiler bug, verified:** the type tag is the struct's id and the *first struct
declared in the program's root file* gets id 0 — which is also the tag meaning
"this error carries no typed value". If your `Error` struct is the first struct in
the file, `T.is(err)` answers `true` for every error, including `errors.new(…)`
results and `nil`, and `T.as` still correctly answers absent. Declaring any other
struct ahead of it (the `Filler` above) restores correct behaviour. Enums are
unaffected — their tags are offset by `0x8000_0000`.

## Unrecoverable failures

Some conditions are bugs, not errors, and they **trap**: `unreachable` on the Wasm
target, `abort` on native. A trap is not catchable — no `recover`, no panic
handler, no unwinding. This is a deliberate rejection of Go's second, invisible
propagation channel.

What traps: slice index out of range, integer division by zero, a failed
`assert`, a failed `require`. The runtime prints e.g.

```
error: index 10 is out of range for a slice of length 3
note: traps are not catchable; Kite has no `recover`
```

and exits non-zero.

```kite
fn checked_div(a: int, b: int) -> (int, error) {
    if b == 0 {
        return _, errors.new("division by zero")
    }
    return a / b, nil
}

fn main() {
    require(1 == 1, "arithmetic works")
    assert(2 > 1, "ordering holds")

    let (v, err) = checked_div(10, 0)
    if err != nil {
        io.print(err.message())
    } else {
        io.print(v)
    }
}
```

`assert(cond, msg)` is compiled out under `--release`; `require(cond, msg)` is
always on. Verified: `kitec run --release` on a program whose only failure is an
`assert(false, …)` exits 0.

## Diagnostic codes

| code | `--explain` title | triggered by |
|---|---|---|
| `E0301` | value used before its error was checked | reading a Tainted value; also `.message()` on an error not proved non-nil |
| `E0302` | error is never checked | Unchecked binding out of scope; `let (v, _) = f()`; bare-statement fallible call |
| `E0303` | `check` outside a fallible function | `check` where the return type has no error component |
| `E0200` | type mismatch | `return a, b` in a non-fallible function; a non-`Error` type in an error slot; `check` on a non-`error`; `io.print(err)` |
| `E0203` | missing return value | `return (1, nil)` in a fallible function (*a fallible function returns two values*); a missing `return` (*not every path returns a value*) |
| `E0112` | duplicate definition | rebinding a non-error name; rebinding an error that is still Unchecked reports `E0302` instead |

There is no `E0300` and no `E0304`; `kitec --explain <code>` lists every code it
knows.

## Where the specification is wrong or incomplete

- §7.5's in-place-handling example (`let port = if err != nil { 8080 } else { port }`)
  does not compile: two same-scope `let port` bindings is `E0112`, and reading the
  value from the `else` arm of an `if`-*expression* is `E0301` even when the error
  was tested in its condition. Use a statement `if`/`else` or an early return.
- §7.3 implies `-> (int, int)` is refused. It is accepted, as a tuple type; only
  the two-value `return a, b` statement inside it is refused.
- §7.6 says `errors.wrap` "keeps what it wrapped", which is true of the cause
  chain but not of the type tag: `errors.because` stores no value or tag of its
  own, so `T.is` must be applied to `errors.root(err)`, never the wrapper. The
  spec never says this, and its own §7.6 example is correct only because it uses
  `errors.root`.
- §7.3's R3 ("an Unchecked binding going out of scope is a compile error") reads
  as though it covers every error binding. Only a destructured `e` is ever
  Unchecked: `let e = touch()` on a `-> error` function compiles and drops the
  failure, and so does `let p = load()` on a pair.
- Undocumented: `err.message()` requires the error to be proved non-nil (`E0301`);
  `error` is not printable by `io.print`; `let (v, _) = f()` has its own `E0302`
  wording; `-> error` alone satisfies `check`; and `return _, nil` is accepted,
  handing the caller a hole it is allowed to read.
