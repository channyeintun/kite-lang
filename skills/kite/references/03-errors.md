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
3. **An error cannot be dropped** by accident. An uninspected `err` from a
   destructuring, `_` in the error slot, a bare call statement, and a call's
   `error` or whole `(T, error)` bound to one name and never read are all
   `E0302`; `_ = f()` is the one deliberate way. See *An error cannot be
   dropped* below.
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
same scope, which no other binding may be. Only the error slot of a pair, and
only over an earlier `error`: `let x = 1` then `let (y, x) = (2, 3)` is `E0112`,
and so is `let (a, a) = …` (or `for (k, k) in …`), which names one thing twice.

`_` is only allowed beside an error that is not nil: `return _, nil` is `E0200`,
because the caller would be allowed to read a value that does not exist. An error
the compiler cannot prove present, such as `return _, lookup()` where `lookup`
returns `error`, is tested when the `return` runs: if it is nil, the program traps
there, with the same message on every backend. `if err != nil { return _, err }`
and `return _, errors.new("…")` are proved present and never trap.

```kite fails
fn count() -> (int, error) {
    return _, nil //~ E0200
}

fn main() {
    let (n, err) = count()
    if err == nil {
        io.print(n)
    }
}
```

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

The proof is tracked per *binding*, so an error reached any other way — a
field (`r.err.message()`), a call (`err.cause().message()`), an element — is
`E0301` until it is bound and tested. The one exception is an error built on the
spot, `errors.new("x").message()`, which is never nil.

```kite fails
fn main() {
    let err = errors.new("outer")
    if err != nil {
        io.print(err.cause().message()) //~ E0301
        let cause = err.cause()
        if cause != nil {
            io.print(cause.message())
        }
    }
}
```

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
- it has no type to write: `(int, error)` written as a type is a tuple, so
  `let p: (int, error) = f()` is `E0200` (*expected the tuple …, found the
  result of a fallible call*). Bind it whole as `let p = f()`, or take it apart;
- it has exactly two components. `-> (int, str, error)` is not a fallible
  signature; `return 1, "a", nil` in it is `E0200` and then a parse error.
- `-> error` alone is also a fallible signature, and is enough for `check`.

## Taint analysis

After a fallible call the compiler runs a forward dataflow pass over the CFG,
tracking two flow-sensitive states. It is not a borrow checker: no ownership, no
aliasing, no lifetimes.

| | rule |
|---|---|
| R1 | after `let (v, e) = f()`, `e` is **Unchecked** and `v` is **Tainted** |
| R2 | reading a Tainted binding is `E0301` |
| R3 | an Unchecked binding going out of scope is `E0302` — at the end of its block, on the path of every `return`, `check`, `break` or `continue` that leaves it behind, and at a write over it (`e = nil`, `e = other`, `e = g()`) |
| R4 | on a path where `e == nil` is proved, `e` becomes Checked and `v` Clean — by `e == nil` or `!(e != nil)`, by either side of an `&&` that held, by either side of an `||` that failed, and past `check errors.wrap(…)`, nested or not. An error test crosses `&&` and `\|\|`, so `if err == nil && v > 0 { … }` and `if err != nil \|\| v < 0 { return }` both read `v` cleanly (an optional's narrowing does not) |
| R5 | on a path where `e != nil`, `e` becomes Checked and `v` stays Tainted **permanently** |
| R6 | a bare-statement call whose type is `error` or `(T, error)` is `E0302` — and so is the `Task` of one: an `async fn` answering with an error, called and never awaited — and so is a `defer` of any of them |
| R7 | an `error` or a whole `(T, error)` bound to one name — `let` or `var`, from a call, `await`, a value `if`, a tuple literal, or assigned later with `e = f()` — is **Unchecked**; only `nil` is not. A copy of another binding (`let e = err`) shares that binding's obligation: inspect either name. After `var e: error = nil` and `for i in 0..n { e = c }`, `e` holds `c`'s failure or nothing: testing `e` does not answer for `c`, so test `c`. A test inspects wherever it is written, the right operand of `&&` or `\|\|` included: `if verbose && e != nil { … }` meets `e`'s obligation even when `verbose` is false |

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

Six spellings of "drop it": five rejected, and one deliberate.

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

**A bare `error` bound to a name and never looked at.** `let e =
dom.set_text(…)` is exactly what you write by habit, and naming the result does
not buy silence: a lone `error` binding is watched like the error slot of a
destructuring. Test it, or write `_ = …` and mean it.

```kite fails
fn touch() -> error {
    return errors.new("no")
}

fn main() {
    let e = touch() //~ E0302
    io.print("dropped")
}
```

**An early exit leaves an error behind.** Checking it on the path that falls
through is not enough when a `return`, `break` or `continue` above the test
takes another way out. Check each error before the next exit, which in practice
means right after the call:

```kite fails
fn touch() -> error {
    return errors.new("no")
}

fn run(n: int) -> int {
    let e = touch()
    if n > 0 {
        return 1 //~ E0302
    }
    if e != nil {
        return -1
    }
    return 0
}

fn main() {
    io.print(run(1))
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

**A whole `(T, error)` bound to one name.** Nothing is destructured, so R1
never marks an `err` — the binding itself is marked instead, exactly as a lone
`error` binding is, and leaving scope unread is `E0302` (R7).

```kite fails
fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let p = load() //~ E0302
    io.print("dropped")
}
```

Taking it apart, `let (v, err) = p`, re-enters the normal rules, and returning
it passes it on; either clears the mark. `p` itself has no fields (`E0200`) and
no methods (`E0205`), so destructure where you bind.

**What the analysis does not follow:** a failure stored into a struct field
(`Held{ e: f() }`, `h.e = f()`), a map entry, or a slice element by index is
handed on to that value, like an argument, and nothing reports it if the value
is never read. So is a map literal of results, `{1: load(1)}`. `errs.push(f())`
*is* followed: a slice of errors is a binding. A `for` walk over a slice of
results counts as reading all of it, so a `break` or `return` out of the walk
drops the failures of the elements not yet reached, with no report.

**A tuple, slice or optional of results is watched the same way** — the
shapes `task.both`, `task.all` and `task.timeout` answer with. Bound to one name
it is Unchecked (`rs.len()` does not count as reading it); taking it apart, or
walking it with `for`, hands each failure to the name it lands in. `r.0` takes
one element of a tuple and leaves the others owed — the tuple is settled once
each has been taken, and taking it apart afterwards binds only the failures not
yet taken. `t.0.0` goes a level down: it leaves `t.0.1` owed, as
`let u = t.0` then `u.0` does. A projection of a tuple nobody holds, `(f(1), f(2)).0` or `pair().0`,
throws the other elements away on the spot, so each failure among them is
`E0302` there; bind the tuple and take it apart, with `_` for one you mean to
drop. `rs[0]` on a slice, though, counts as reading the whole slice, since a
slice cannot be followed element by element. Both results of `task.both` arrive
together, so deal with both before any exit — a `check` on the first leaves the
second behind:

```kite
use std/task

async fn load(n: int) -> (int, error) {
    if n == 2 {
        return _, errors.new("two failed")
    }
    return n * 10, nil
}

async fn sum() -> (int, error) {
    let (ra, rb) = await task.both(load(1), load(2))
    let (a, ea) = ra
    let (b, eb) = rb
    if ea != nil {
        _ = eb
        return _, ea
    }
    check eb
    var total = a + b
    for r in await task.all([load(3), load(4)]) {
        let (n, err) = r
        check err
        total += n
    }
    return total, nil
}

async fn main() {
    let (n, err) = await sum()
    if err != nil {
        io.print(err.message())
        return
    }
    io.print(n)
}
```

Testing a `task.timeout` result against `nil` only asks whether the work came in
time. Where it did, what it gave is still unread: after
`if r == nil { return }`, `r` is the `error` (or the pair) and has to be tested
(or taken apart) in turn. That second test is the one that proves the error
present: a task that came in time with a `nil` error gives a present optional,
so `r.message()` straight after `if r == nil { return }`, or inside
`if r != nil { … }` on the optional, is `E0301`. The same goes for an
`Option<error>` from anywhere else — `m[k]` on a map of errors, `xs.get(i)`. In full:

```kite
use std/task

async fn save() -> error {
    return errors.new("disk full")
}

async fn main() {
    let r = await task.timeout(save(), 100)
    if r == nil {
        io.print("timed out")
        return
    }
    if r != nil {
        io.print(r.message())
    }
}
```

So bind the result first: `if await task.timeout(save(), 100) == nil { … }`
tests a result nothing holds, and the failure in it is lost — `E0302` at the
`await`.

The same holds however the failure got into a single binding — `var e = f()`,
`let e = await f()`, `let e = if c { f() } else { g() }`, `let (n, e) = (1,
f())`: the binding has to be looked at. Only `nil` starts out Checked — and a
value `if` whose branches are bindings and `nil`: `let e = if x { c } else
{ nil }` holds `c`'s failure or nothing, so `c` answers for it, and once `c` is
checked, `e` holds nothing at all.

**Copying an error is not looking at it.** `let e = err` gives the failure a
second name, and one of the two has to be inspected — either will do. A
`match` whose every arm is `_` or a bare name tests nothing either; `match f()
{ _ => … }` is `E0302` at the scrutinee:

```kite fails
fn touch() -> error {
    return errors.new("no")
}

fn main() {
    let c = touch() //~ E0302
    let e = c
    match touch() { //~ E0302
        _ => io.print("nothing tested"),
    }
}
```

A guard is a test, though. An arm whose pattern is `_` or a bare name is passed
over only when its guard came out false, so the arms after it know what the
guard proved, and the arm itself runs knowing it held:

```kite
fn touch() -> error {
    return errors.new("no")
}

fn main() {
    let e = touch()
    match e {
        x if x != nil => io.print(x.message()),
        _ => io.print("fine"),
    }
}
```

A guard behind a pattern that can fail (`1 if e != nil`) may never run, so it
proves nothing to the arms after it.

Writing over a binding that still holds an unchecked failure drops that
failure, so it is `E0302` at the write, whatever is written:

```kite fails
fn f() -> error {
    return errors.new("boom")
}

fn main() {
    var e = f()
    e = nil //~ E0302
    io.print("dropped")
}
```

Test it first, or pass it on with `e = errors.wrap(e, "…")`, which reads the
old value and so carries its obligation into the new one.

**A loop's next iteration is a write too.** The body runs again from its end
and from every `continue`, so a failure left unchecked in a binding declared
outside the loop is written over by the next pass, and a `break` carries it out
to the code after the loop:

```kite fails
fn f(i: int) -> error {
    if i == 1 {
        return errors.new("boom")
    }
    return nil
}

fn main() {
    var e: error = nil
    for i in 0..3 {
        e = f(i) //~ E0302
    }
    if e != nil {
        io.print(e.message())
    }
}
```

Test it inside the body before the next write — `if e != nil { break }` — or
keep the error inside the loop. A `for { }` is left only by `break`, so the
retry loop `for { if err == nil { break } … err = attempt() }` needs nothing
after it; written with `return` in place of `break` and no `break` at all,
nothing follows the loop, so only the `return`s have to answer for `err`.

## `check`

`check err` is exactly, in a `-> (T, error)` function — and so it runs what was
`defer`red on the way out, like any other `return`:

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

Test the error: in the branch where it is nil the value is readable, in an `if`
used as a value exactly as in an `if` statement.

```kite
fn get_int(k: str) -> (int, error) {
    if k == "port" {
        return 8080, nil
    }
    return _, errors.new("no key \(k)")
}

fn main() {
    let (p, err) = get_int("prt")
    let port = if err != nil { 80 } else { p }
    io.print(port)
}
```

The value stays unreadable in the branch where the error is *not* nil, and after
the `if`. Bind it under a name of its own — `p` here — because a second `let port`
in the same scope is `E0112`. The statement form does the same work:

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
one line — and passing that `check` cleans the value `err` guards, because
`wrap` answers nil *only* for nil. That is known of `errors.wrap` alone: `check`
of what your own function returned proves nothing about the error it was handed.
It is built on `errors.because`, so it **keeps** what it wrapped rather than
flattening it into text.

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

Only a type that implements `Error` has `is` and `as`: on any other,
`Plain.is(err)` is `E0205`, since no error can carry it.

For a generic error type, each specialisation is its own type: write the one you
want on the binding, `let w: Option<Wrapped<int>> = Wrapped.as(err)`. A bare
`Wrapped.as(err)`, and `Wrapped.is(err)`, cannot say which, and are `E0209`.

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

## Unrecoverable failures

Some conditions are bugs, not errors, and they **trap**: `unreachable` on the Wasm
target, `abort` on native. A trap is not catchable — no `recover`, no panic
handler, no unwinding. This is a deliberate rejection of Go's second, invisible
propagation channel.

What traps: slice index out of range, integer division by zero, a failed
`assert`, a failed `require`, and a call chain deeper than the target allows —
100,000 frames on the VM and natively (`call depth exceeded 100000 frames`),
and the host's stack on WebAssembly, which is a few thousand frames of an
ordinary function. A recursion whose depth comes from input should bound it
and return an `error`. The runtime prints e.g.

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
| `E0302` | error is never checked | Unchecked binding out of scope; `let (v, _) = f()`; bare-statement fallible call; `defer` of one |
| `E0303` | `check` outside a fallible function | `check` where the return type has no error component |
| `E0200` | type mismatch | `return a, b` in a non-fallible function; a non-`Error` type in an error slot; `check` on a non-`error`; `io.print(err)` |
| `E0203` | missing return value | `return (1, nil)` in a fallible function (*a fallible function returns two values*); a missing `return` (*not every path returns a value*) |
| `E0112` | duplicate definition | rebinding a non-error name; rebinding an error that is still Unchecked reports `E0302` instead |

There is no `E0300` and no `E0304`; `kitec --explain <code>` lists every code it
knows.

## Where the specification is wrong or incomplete

- §7.3 implies `-> (int, int)` is refused. It is accepted, as a tuple type; only
  the two-value `return a, b` statement inside it is refused.
- §7.6 says `errors.wrap` "keeps what it wrapped", which is true of the cause
  chain but not of the type tag: `errors.because` stores no value or tag of its
  own, so `T.is` must be applied to `errors.root(err)`, never the wrapper. The
  spec never says this, and its own §7.6 example is correct only because it uses
  `errors.root`.
- Undocumented: `err.message()` requires the error to be proved non-nil (`E0301`);
  `error` is not printable by `io.print`; `let (v, _) = f()` has its own `E0302`
  wording; and `-> error` alone satisfies `check`.
