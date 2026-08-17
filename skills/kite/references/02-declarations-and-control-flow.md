# Kite: declarations, visibility, expressions, control flow

## Surprises, first

Everything below is checked against `target/release/kitec`, not against the prose.

- **A module-level `let` is a constant; a module-level `var` does not exist** (`E0118`).
  A file is `use` lines — which must come first — then declarations (`fn`, `struct`,
  `enum`, `trait`, `impl`, `type`, a constant `let`, and `extern fn` under its `@host(…)`
  attribute). Every *mutable* binding lives inside a function body.
- **A closure may not capture a `var`** (`E0211`). Captures are by value at closure-creation
  time, so a later write would be invisible. Mutation goes through a function that takes a
  `var` parameter.
- **An `if` used as a value takes exactly one expression per branch.** No statements, no
  trailing-expression block. `let x = if c { let y = 1  y } else { 0 }` is `E0200`.
  A `match` arm follows the same rule: `{ 1 }` is still the value `1`, but the moment the
  block holds a statement the arm is `()`.
- **`;` is not a token.** Newlines end statements. A statement continues only when the
  previous line *ends* in an operator, an open delimiter, or a comma. A line that *starts*
  with `||` is a zero-parameter closure, silently discarded — `E0117` exists to catch
  exactly that.
- **`xs[a..b]` clamps; `xs[i]` traps.** A window may run off the end; an element may not.
- **Map indexing always yields `Option<V>`** — never a zero value, and `io.print` will not
  take it.
- **Open-ended ranges do not exist.** `xs[..2]` and `xs[1..]` are parse errors, and a range
  is not a value: you cannot bind, pass, or return `0..n`.
- **Bitwise binds tighter than comparison** (unlike C), but `&`, `^`, `|` are three separate
  levels among themselves, exactly as in C.
- **Struct declaration fields are newline-separated, not comma-separated** — and a field is
  immutable unless the field itself says `var`, whatever the binding says.
- **An enum variant pattern that binds a payload must be written bare** — `Circle(r)`,
  never `Shape.Circle(r)` — see the trap at the end of the `match` section.
- **`assert(cond, msg)` always takes two arguments** and is a builtin, not a value.

---

## 1. Bindings

`let` is immutable, `var` is mutable, and the type is inferred unless you write it. A `let`
may be declared without an initialiser and assigned later, provided the compiler can prove
exactly one assignment happens on every path before the first read. A `var` may not:
it must be initialised where it is declared.

```kite
fn main() {
    let x = 42              // immutable, inferred `int`
    let y: float = 3.0      // immutable, explicit
    var count = 0           // mutable
    count = count + 1
    count += 2              // `=` `+=` `-=` `*=` `/=` `%=`

    let z: int              // deferred initialisation
    if x > 10 {
        z = 1
    } else {
        z = 2
    }
    io.print("\(y) \(count) \(z)")

    // Destructuring binds a tuple. `_` discards an element.
    let (a, b) = split_at(x)
    let (_, tail) = split_at(x)
    io.print("\(a) \(b) \(tail)")
}

fn split_at(n: int) -> (int, int) {
    return (n / 2, n - n / 2)
}
```

Destructuring is a `let` feature only — `var (a, b) = …` is a parse error, because the
grammar's `VarStmt` takes a single identifier.

Shadowing is allowed in a *nested* scope and rejected in the *same* scope.

```kite
fn main() {
    let x = 5
    if x > 3 {
        let x = "shadow"    // nested scope: fine
        io.print(x)
    }
    io.print(x)
}
```

```kite fails
fn main() {
    let n = 1
    var n = 2 //~ E0112
    io.print(n)
}
```

```kite fails
fn main() {
    let total = 0
    total = 1 //~ E0114
    io.print(total)
}
```

```kite fails
fn main() {
    var n: int //~ E0110
    n = 1
    io.print(n)
}
```

```kite fails
fn main() {
    let z: int
    if true {
        z = 1
    }
    io.print(z) //~ E0110
}
```

A shared constant is a module-level `let`. Every use of the name is replaced by the value,
so nothing is looked up and nothing is allocated at run time.

```kite
let NAMESPACE = "payments:"
pub let ACT_SETTLE = "\(NAMESPACE)settle"
pub let MAX_BODY: int = 1 << 20
```

The right-hand side has to be one the compiler can work out: a literal, an operator applied
to constants, an interpolation whose holes are all constants, or another constant —
including an imported one, `limits.MAX_BODY`. A **call is not** (`E0118`), even one that
would always return the same answer. The types are `bool`, `int`, `float` and `str`; a
slice or map constant would be an allocation, so it stays an ordinary `let` inside the
function that wants it. A `float` may not be interpolated *into* a constant (`E0118`) —
the browser and the native runtime write one differently at the exponent boundary.

There is no module-level `var`: a mutable binding every function can reach is state none
of their signatures mentions. Put it in a struct and pass it to what changes it.

```kite fails
var counter = 0 //~ E0118

fn main() {
    counter = counter + 1 //~ E0114
}
```

Mutating *through* a binding also needs `var`. `xs[0] = 9`, `xs.push(3)`, `m["k"] = v` and
`s.field = v` all require the binding to be `var` (`E0114`) — a struct is no exception,
even though it is a reference. A struct field needs a second `var`, on the field itself,
before anything may assign it at all.

```kite fails
struct Point {
    x: int
}

fn main() {
    var p = Point{ x: 1 }
    p.x = 2 //~ E0114
    io.print(p.x)
}
```

The one way the contents of a `let` change is a call: hand it to a function whose parameter
is `var`, and the write lands where the `let` can see it — §3.

---

## 2. Visibility

`pub` is the only visibility modifier and there are exactly two levels: unmarked (visible
inside the declaring module) and `pub` (visible to importers). **A module is a directory**;
every `.kite` file in it shares one namespace and files in a module never import each other.
An importer always spells the module name — there is no wildcard import.

```kite ignore
// inventory/item.kite — one file of the `inventory` module
pub struct Item {
    pub name: str
    pub price: int
}

pub fn item(name: str, price: int) -> Item {
    return Item{ name: name, price: price }
}

fn line_total(it: Item) -> int {     // unmarked: private to `inventory`
    return it.price
}
```

```kite ignore
// main.kite
use inventory
use std/math as m                     // `as` renames, but the name is still required

fn main() {
    let it = inventory.item("bolt", 3)
    io.print(it.name)
    io.print(m.round(1.5))
    // inventory.line_total(it)       // error[E0401]: private to module `inventory`
}
```

`E0401` is the diagnostic for reaching into another module for something unmarked.

> **Compiler vs specification.** SPECIFICATION.md §4.3 says a `pub struct` with unmarked
> fields is opaque — importers "cannot read, construct, or destructure it". The compiler
> does not enforce this. `check_visible` in `crates/kite-resolve/src/lib.rs` runs only for
> `Res::Fn`, `Res::Type` and `Res::Variant`; field-level `pub` is parsed and then ignored,
> so an importer can read a private field and write a struct literal naming it. Treat field
> `pub` as documentation until that gap closes. §4.3 lists enum variants as taking `pub`
> too; there the parser rejects it outright (`E0100`) — a variant's visibility is its
> enum's.

---

## 3. Functions

```kite
pub fn add(a: int, b: int) -> int {
    return a + b
}

fn greet(name: str) {                 // no `->` means it returns `()`
    io.print("hello \(name)")
}

pub fn halve(n: int) -> (int, error) {    // fallible: the pair is a return *form*
    if n % 2 != 0 {
        return _, errors.new("odd")
    }
    return n / 2, nil
}

fn main() {
    greet("ada")
    io.print(add(2, 3))
    let (v, err) = halve(4)
    if err != nil {
        io.print("odd")
        return
    }
    io.print(v)
}
```

There is **no overloading, no default argument, no variadic parameter, and no named
argument at the call site.** One name, one signature, exact arity.

```kite fails
fn f(a: int) -> int { return a }
fn f(a: str) -> str { return a } //~ E0112

fn main() { io.print(f(1)) }
```

```kite fails
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    io.print(add(1)) //~ E0113
}
```

```kite fails
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    io.print(add(a: 1, b: 2)) //~ E0113
}
```

A declared `-> T` is a promise every path has to keep; falling off the end is `E0203`,
never an implicit zero.

```kite fails
fn size(xs: [int]) -> int { //~ E0203
    io.print(xs.len())
}

fn main() {
    io.print(size([1]))
}
```

A function that wants many optional inputs takes a struct. Struct literals name every
field, so the call site reads like named arguments using machinery the language already has.

```kite
struct RequestOptions {
    method: str
    timeout: int
}

fn request(url: str, opts: RequestOptions) -> str {
    return "\(opts.method) \(url) (\(opts.timeout)ms)"
}

fn main() {
    io.print(request("https://example.com", RequestOptions{
        method:  "POST",
        timeout: 30000,
    }))
}
```

Parameters are immutable inside the body unless declared `var`. For a scalar, `var` is a
local copy; for a struct — which is a reference — the write lands where the caller can see
it. That is the whole mutation story of the language.

```kite
struct Counter {
    var count: int
}

fn increment(var c: Counter) {        // the caller's binding may be `let`
    c.count = c.count + 1
}

fn bump_scalar(var n: int) -> int {   // a local copy; the caller sees nothing
    n = n + 1
    return n
}

fn main() {
    let state = Counter{ count: 0 }
    increment(state)
    increment(state)
    io.print(state.count)             // 2
    io.print(bump_scalar(1))          // 2
}
```

A **method** taking `var self` is stricter than a function taking a `var` parameter: it
requires the *receiver binding* to be `var`, even though the struct is a reference.

```kite
struct Rect {
    var label: str
}

impl Rect {
    fn rename(var self, name: str) {
        self.label = name
    }
}

fn main() {
    var r = Rect{ label: "a" }        // `let r` here would be E0114
    r.rename("b")
    io.print(r.label)
}
```

Two arguments that may be the same struct are rejected as soon as **either** parameter is
`var` — `E0800`, "one object under two argument names". One argument reaching the other
counts: `f(o, o.inner)` is caught exactly as `f(x, x)` is. With no `var` among the
parameters, passing one object twice is fine.

```kite fails
struct C {
    var n: int
}

fn merge(var a: C, b: C) {
    a.n = a.n + b.n
}

fn main() {
    var x = C{ n: 1 }
    merge(x, x) //~ E0800
    io.print(x.n)
}
```

---

## 4. Closures

A closure is `|params| body`. The body is either a single expression or a block. A block
body that *produces* a value needs an explicit `-> T`, because there is nothing else to
check its `return` statements against; a block that only acts — `|| { increment(state) }` —
needs nothing. Parameter types come from the context the closure is used in, so
`apply(|x| x * 2, 21)` infers `x` from `apply`'s signature; where there is no context,
annotate them.

```kite
fn apply(f: fn(int) -> int, x: int) -> int {
    return f(x)
}

fn adder(n: int) -> fn(int) -> int {
    return |x: int| x + n             // `n` is copied in; each adder has its own
}

fn main() {
    io.print(apply(|x: int| x * 2, 21))

    let sign = |n: int| -> str {      // block body: `-> str` is required
        if n < 0 {
            return "negative"
        }
        return "positive"
    }
    io.print(sign(-4))

    let factor = 3                    // capturing a `let`: always fine
    io.print(apply(|x: int| x * factor, 7))

    io.print(apply(adder(5), 1))
    io.print(fold([1, 2, 3], 0, |acc: int, n: int| acc + n))
}
```

**Captures are by value, taken when the closure is made, and a `var` may not be captured.**

```kite fails
fn main() {
    var total = 0
    let add = |n: int| n + total //~ E0211
    io.print(add(1))
}
```

The fix is not a capture list — the language has none. Capture a `let` handle to a struct
and let a named function do the writing, so the mutation is spelled out in a signature.

```kite
struct Counter {
    var count: int
}

fn increment(var c: Counter) {
    c.count = c.count + 1
}

fn main() {
    let state = Counter{ count: 0 }
    let bump = || { increment(state) }   // captures a `let`, by value
    bump()
    bump()
    io.print(state.count)
}
```

`E0211` also covers a parameter whose type cannot be inferred, and a block-bodied closure
that fails to return on some path.

```kite fails
fn main() {
    let f = |x| x * 2 //~ E0211
    io.print(f(3))
}
```

### The `||` line-continuation trap

A statement continues to the next line only when it *ends* in an operator. A `||` opening
a line is therefore not a continuation — it is a closure with no parameters in expression
position, built and thrown away. `E0117` catches it.

```kite fails
fn classify(c: int) -> bool {
    let unreserved = (c >= 48 && c <= 57)
    || (c >= 65 && c <= 90) //~ E0117
    return unreserved
}

fn main() {
    io.print(classify(65))
}
```

Put the operator at the end of the line it continues:

```kite
fn classify(c: int) -> bool {
    return (c >= 48 && c <= 57) ||
        (c >= 65 && c <= 90)
}

fn main() {
    io.print(classify(65))
}
```

---

## 5. Expressions

### 5.1 Precedence

Tightest to loosest, as the compiler's Pratt table has it
(`crates/kite-parser/src/prec.rs`):

| | Operators | Associativity |
|---|---|---|
| 1 | `a.b`  `a(…)`  `a[…]` | left (postfix) |
| 2 | `-a`  `!a`  `await a` | prefix |
| 3 | `as` | left |
| 4 | `*`  `/`  `%` | left |
| 5 | `+`  `-` | left |
| 6 | `<<`  `>>` | left |
| 7 | `&` | left |
| 8 | `^` | left |
| 9 | `\|` | left |
| 10 | `==` `!=` `<` `<=` `>` `>=` | **non-associative** |
| 11 | `&&` | left |
| 12 | `\|\|` | left |
| 13 | `..`  `..=` | **non-associative** |

Bitwise operators bind tighter than comparison, so `a & b == c` is `(a & b) == c` — the
one thing C gets wrong. A range is the loosest operator there is, so `0..n + 1` is
`0..(n + 1)`.

> **Compiler vs specification.** The §5.1 table and `docs/05-grammar.ebnf` both put `&`,
> `^` and `|` on a single left-associative level. The compiler gives them three distinct
> levels, `&` tightest, in C's relative order: `2 | 1 ^ 3` evaluates to `2`
> (`2 | (1 ^ 3)`), not `0` (`(2 | 1) ^ 3`). Parenthesise when mixing them.

```kite
fn main() {
    assert(2 + 3 * 4 == 14, "* before +")
    assert(1 << 3 + 1 == 16, "+ before <<")
    assert((1 & 3) == 1, "bitwise before comparison")
    assert(1 & 3 == 1, "same thing without the parentheses")
    assert(2 | 1 ^ 3 == 2, "^ binds tighter than |")
    assert(approx_eq(-3 as float, -3.0, 0.001), "prefix before as")
    assert(approx_eq(2.0 * 3 as float, 6.0, 0.001), "as before *")
    io.print("ok")
}
```

Comparison is non-associative, so a chain is a parse error rather than a silent bug.

```kite fails
fn main() {
    let a = 1
    let b = 2
    let c = 3
    io.print(a < b < c) //~ E0100
}
```

A range is syntax, not a type. It exists only in a `for` header and in an index. There is
no `Range` value to bind, pass, or return — `E0200`, "a range cannot be held".

### 5.2 Equality

`==` is structural for every type: two structs are equal when their fields are, two slices
when their elements are. There is no reference-equality operator. `ptr.same(a, b)` is a
builtin that answers whether two names denote one heap cell, and it accepts **structs,
enums and maps only**.

```kite
struct Point {
    x: int
    y: int
}

fn main() {
    let p = Point{ x: 1, y: 2 }
    let q = Point{ ..p, y: 5 }
    io.print(p == Point{ x: 1, y: 2 })   // true — structural
    io.print([1, 2] == [1, 2])           // true
    io.print(ptr.same(p, q))             // false
    io.print(ptr.same(p, p))             // true
}
```

```kite fails
fn main() {
    io.print(ptr.same(1, 1)) //~ E0213
}
```

A slice is rejected by `ptr.same` too: it is copy-on-write, so buffer sharing is an
allocator fact a write would end. Float `==` follows IEEE-754 (`nan != nan`) and raises
`E0201` as a *warning* — the program still compiles — when both operands are statically
float and neither is a literal.

### 5.3 Struct literals

Every field must be given, or `..base` must supply the rest. There are no zero values, so a
missing field is a compile error rather than a silent `0`. `Point{ ..p, y: 5 }` is a
functional update: it produces a new value and does not mutate `p`.

```kite fails
struct Rect {
    width: int
    height: int
}

fn main() {
    let r = Rect{ width: 1 } //~ E0200
    io.print(r.width)
}
```

Fields in a **declaration** are separated by newlines. Commas are a parse error — a
difference from the literal syntax, where commas separate the initialisers.

```kite fails
struct Point { x: int, y: int } //~ E0100

fn main() {
    io.print(Point{ x: 1, y: 2 }.x)
}
```

A struct literal is not permitted where a condition is expected, so that `if x { … }` is
never ambiguous. Wrap it: `if (Point{ x: 0, y: 0 }).is_origin() { … }`.

### 5.4 Slices and maps

```kite
fn main() {
    let xs = [1, 2, 3]
    let m  = {"a": 1, "b": 2}

    io.print(xs[0])                 // int — bounds-checked, traps on failure
    io.print(xs.len())
    io.print(xs[1..3].len())        // 2  — half-open subslice
    io.print(xs[1..=2].len())       // 2  — inclusive
    io.print(xs[2..100].len())      // 1  — a window CLAMPS
    io.print(xs[4..1].len())        // 0  — inverted is empty, not an error

    let got = xs.get(9)             // Option<int> — nil rather than a trap
    io.print(if got == nil { -1 } else { got })

    let hit = m["a"]                // Option<int>, always
    io.print(if hit == nil { -1 } else { hit })

    io.print("hello"[1..3])         // a `str` slices the same way
}
```

`xs[i]` names an element the program believes is there, so a miss is a bug and it traps
(uncatchable — Kite has no `recover`). `xs[a..b]` names a *window*, and a window wider than
the data is what the last page of a paging loop produces, so it clamps. Use `.get(i)` when
absence is a runtime condition rather than a bug.

Open-ended ranges are not in the language, even though the EBNF's `Postfix` rule permits
them — **the compiler is right**: write both endpoints.

```kite fails
fn main() {
    let xs = [1, 2, 3]
    io.print(xs[..2].len()) //~ E0100
}
```

A map is indexed by key only; `m[a..b]` is `E0200`, because keys have no order for a range
to name. And because indexing a map gives an `Option`, it cannot be printed directly.

```kite fails
fn main() {
    let m = {"a": 1}
    io.print(m["a"]) //~ E0200
}
```

Narrow it with `if v == nil { … } else { … }`, with `or_else(v, fallback)` from the
prelude, or with `match v { nil => …, x => … }`.

An empty slice literal has nothing to infer an element type from, so it needs the
annotation: `let ys: [int] = []`.

```kite fails
fn main() {
    let ys = [] //~ E0204
    io.print(ys.len())
}
```

---

## 6. Statements

- Newlines terminate statements. **`;` is not a token in Kite** (`E0002`, "invalid
  character").
- A statement continues onto the next line only when the previous line ends in an operator,
  an open delimiter, or a comma.
- `_ = f()` discards a result on purpose. Only plain `=`; a compound assignment would be
  reading the hole.
- Assignment targets are `x`, `x.field`, and `x[i]`, with `=` `+=` `-=` `*=` `/=` `%=`.

```kite
fn f() -> int { return 3 }

fn main() {
    _ = f()
    var n = 1
    n += 2
    io.print(n)
}
```

---

## 7. `if`

Parentheses around the condition are not idiomatic, but the parser has no rule against them
— the condition is just an expression, so `if (x > 1) { … }` compiles, whatever
SPECIFICATION.md §6.1 says. Braces are always required, and the condition must be exactly
`bool`: **there is no truthiness.**

```kite fails
fn main() {
    if 1 { //~ E0202
        io.print("yes")
    }
}
```

`if` is also an expression when an `else` is present and **every branch is a single
expression**. This is the delta from Rust: a value-`if` branch is not a block with a
trailing expression, it is one expression.

```kite
fn main() {
    let n = 12
    let label = if n > 10 { "big" } else if n > 5 { "mid" } else { "small" }
    io.print(label)
}
```

```kite fails
fn main() {
    let n = 4
    let x = if n > 2 { //~ E0200
        let doubled = n * 2
        doubled + 1
    } else {
        0
    }
    io.print(x)
}
```

```kite fails
fn main() {
    let x = if 3 > 2 { "big" } //~ E0100
    io.print(x)
}
```

To compute across statements, use a function, or a deferred-initialisation `let` assigned
in an `if` *statement*.

---

## 8. `for`

`for` is the only loop keyword and it has three headers: iterate, while-a-condition, and
forever. Iteration works over a **range or a slice** — not over a `str`, and a map only
through a two-element destructuring binding.

```kite
fn main() {
    let items = ["a", "b"]

    for item in items {
        io.print(item)
    }

    for i in 0..3 { io.print(i) }        // half-open: 0, 1, 2
    for i in 0..=3 { io.print(i) }       // inclusive: 0, 1, 2, 3

    let m = {"x": 1, "y": 2}
    for (k, v) in m {                    // maps yield tuples, in insertion order
        io.print("\(k)=\(v)")
    }

    // `enumerate` and `zip` are prelude functions returning [(A, B)] — the
    // tuple form above is what makes them work; there is nothing special here.
    for (i, s) in enumerate(items) { io.print("\(i):\(s)") }
    for (a, b) in zip([1, 2], items)  { io.print("\(a)\(b)") }

    var count = 0
    for count < 3 {                      // conditional
        count = count + 1
    }

    var n = 0
    for {                                // unconditional
        n = n + 1
        if n > 2 { break }
    }
    io.print("\(count) \(n)")
}
```

The loop variable is a `let`: assigning to it is `E0114`. Labelled `break` and `continue`
work on nested loops; the label goes before the `for` with a colon.

```kite
fn main() {
    let grid = [[1, 0], [3, 4]]
    outer: for row in grid {
        for cell in row {
            if cell == 0 { continue outer }
            if cell == 4 { break outer }
            io.print(cell)
        }
    }
}
```

A single binding over a map is rejected — there is no key-only iteration form.

```kite fails
fn main() {
    let m = {"a": 1}
    for k in m { //~ E0200
        io.print(k)
    }
}
```

```kite fails
fn main() {
    break //~ E0115
}
```

---

## 9. `defer`

`defer` takes a **call** and nothing else. The receiver and arguments are evaluated where
the `defer` is written, into hidden locals; the call happens when the enclosing *function*
returns, by any path, in reverse order of the `defer` statements. A `defer` inside an `if`
that never runs never runs. A deferred call cannot change the return value — the return
expression is evaluated first — though when that value is a struct it is a reference, so a
deferred write *into* it is visible to the caller.

```kite
struct File {
    name: str
}

fn close(f: File) {
    io.print("closing \(f.name)")
}

fn process() -> int {
    let f = File{ name: "data" }
    defer close(f)
    io.print("working")
    return 1
}

fn main() {
    io.print(process())     // working / closing data / 1
}
```

```kite fails
fn main() {
    defer 1 + 2 //~ E0200
    io.print("x")
}
```

> **Compiler vs specification.** §6.3 says deferred calls run "in reverse order of
> registration". The implementation is one hidden flag and one set of hidden operand locals
> **per syntactic `defer` site** (`defer_stmt` in `crates/kite-types/src/lib.rs`), so a
> `defer` in a loop body runs **once**, at function exit, with the last iteration's values —
> not once per iteration. Do not put `defer` inside a loop; move the body into a function
> and defer there.

---

## 10. `match`

Arms are `Pattern [if guard] => Expr-or-Block`, separated by commas or by newlines. A
`match` must be exhaustive (`E0210`) and every arm must produce the same type (`E0200`).

```kite
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}

fn describe(s: Shape) -> str {
    return match s {
        Circle(r) if r > 10 => "big circle",
        Circle(r) => "circle",
        Rect(w, h) if w == h => "square",
        Rect(w, h) => "rect",
        Point => "a point",
    }
}

fn bucket(n: int) -> str {
    return match n {
        0 => "zero",
        1 | 2 | 3 => "small",       // or-pattern
        4..=9 => "medium",          // range pattern
        _ => "large",
    }
}

fn main() {
    io.print(describe(Circle(radius: 2)))
    io.print(describe(Rect(width: 3, height: 3)))
    io.print(describe(Point))
    io.print(bucket(7))
}
```

Patterns also cover `nil`, tuples, and struct fields with `..` for the rest:

```kite
struct P {
    x: int
    y: int
}

fn find(id: int) -> Option<str> {
    if id == 1 { return "ada" }
    return nil
}

fn main() {
    io.print(match find(2) {
        nil => "none",
        n => n,
    })

    let p = P{ x: 1, y: 2 }
    match p {
        P{ x: 0, .. } => io.print("on the y axis")
        P{ x: a, y: b } => io.print("\(a),\(b)")
    }

    io.print(match (1, "a") {
        (n, label) => "\(n)\(label)",
    })
}
```

**A block arm holding a single expression is still that value; a block that holds a
statement is `()`.** `0 => { 1 }` is an `int` arm, exactly as `0 => 1` is. Add one line
above the expression and the arm becomes `()`, and the `match` with it.

```kite fails
fn main() {
    let n = 1
    let d: int = match n { //~ E0200
        0 => {
            let a = 1
            a + 1
        }
        _ => { 2 }
    }
    io.print(d)
}
```

Blocks earn their keep on arms that *act* — including arms that `return` from the enclosing
function.

```kite
fn classify(n: int) -> str {
    match n {
        0 => { return "zero" }
        1..=9 => { return "small" }
        _ => { return "big" }
    }
}

fn main() {
    io.print(classify(0))
    io.print(classify(50))
}
```

```kite fails
enum Shape {
    Circle(radius: int)
    Point
}

fn describe(s: Shape) -> str {
    return match s { //~ E0210
        Circle(r) => "circle",
    }
}

fn main() {
    io.print(describe(Point))
}
```

```kite fails
fn main() {
    let n = 1
    let d = match n {
        0 => 1,
        _ => "other", //~ E0200
    }
    io.print(d)
}
```

### Trap: qualified variant patterns

Variant *constructors* may be written bare (`Circle(radius: 2)`) or qualified
(`Shape.Circle(radius: 2)`). Variant *patterns* must be bare when they bind a payload. A
qualified pattern parses, but its bindings are never bound, and the compiler reports the
downstream `E0110` rather than the real mistake.

```kite fails
enum Shape {
    Circle(radius: int)
    Point
}

fn main() {
    io.print(match Circle(radius: 2) {
        Shape.Circle(r) => r, //~ E0110
        Point => 0,
    })
}
```

```kite
enum Shape {
    Circle(radius: int)
    Point
}

fn main() {
    io.print(match Circle(radius: 2) {
        Circle(r) => r,
        Point => 0,
    })
}
```

Which enum an unqualified pattern names is decided by the scrutinee, so two enums may each
declare `Slow` without ambiguity — and a name that matches a variant of the scrutinee is
that variant, not a fresh binding. A *constructor* has no scrutinee to go on: once two
enums share a variant name, the bare `Slow` is `E0111`, "cannot find `Slow`", and the call
has to say `A.Slow`.

---

## 11. `assert` and `require`

Both are **builtins, not functions**, and both take exactly two arguments: a `bool`
condition and a `str` message. They trap when the condition is false, and a trap is not
catchable.

- `assert(cond, msg)` is compiled out under `kitec --release`.
- `require(cond, msg)` is always on.

```kite
fn main() {
    let n = 3
    assert(n > 0, "n must be positive, got \(n)")
    require(n != 0, "n is a divisor here")
    io.print(60 / n)
}
```

```kite fails
fn main() {
    assert(1 == 1) //~ E0113
}
```

Being builtins, they have no value form — you cannot pass `assert` around.

```kite fails
fn main() {
    let f = assert //~ E0200
    io.print("x")
}
```

---

## Diagnostic codes used here

| Code | Meaning |
|---|---|
| `E0002` | invalid character in source (`;`) |
| `E0100` | unexpected token / parse error |
| `E0110` | possibly-uninitialised binding, or `var` without an initialiser |
| `E0111` | unknown name |
| `E0112` | duplicate definition, or same-scope shadowing |
| `E0113` | wrong number of arguments, or named arguments |
| `E0114` | assignment to an immutable binding, field, or receiver |
| `E0115` | `break`/`continue` outside a loop |
| `E0117` | statement has no effect (the `\|\|` continuation trap) |
| `E0200` | type mismatch |
| `E0201` | operator applied to mismatched types; float `==` warning |
| `E0202` | condition must be `bool` |
| `E0203` | a `-> T` function that can reach its end without returning |
| `E0204` | cannot infer the element type (an empty slice literal) |
| `E0210` | non-exhaustive `match`, or `match` with no arms |
| `E0211` | invalid closure (captures a `var`, uninferable parameter, missing return) |
| `E0213` | `ptr.same` applied outside struct/enum/map |
| `E0401` | private item reached from another module |
| `E0800` | one object under two argument names |

`kitec --explain E0nnn` prints the prose for any of them.
