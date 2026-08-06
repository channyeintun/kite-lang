# The Kite Language Specification

**Version:** 0.1 (draft)
**Date:** August 2026
**Status:** Implemented, on three backends, except where a section says
otherwise. Where this document and the compiler disagree, the compiler is
right and the disagreement is a bug in this file.

---

## Table of contents

1. [Design rationale](#1-design-rationale)
2. [Lexical structure](#2-lexical-structure)
3. [Types](#3-types)
4. [Declarations and visibility](#4-declarations-and-visibility)
5. [Expressions](#5-expressions)
6. [Statements and control flow](#6-statements-and-control-flow)
7. [Error handling](#7-error-handling)
8. [Structs and methods](#8-structs-and-methods)
9. [Enums and pattern matching](#9-enums-and-pattern-matching)
10. [Traits](#10-traits)
11. [Generics](#11-generics)
12. [Concurrency](#12-concurrency)
13. [Modules and packages](#13-modules-and-packages)
14. [Memory model](#14-memory-model)
15. [Foreign function interface](#15-foreign-function-interface)
16. [Diagnostics](#16-diagnostics)

---

## 1. Design rationale

### 1.1 The concept budget

A language's difficulty is not measured in keywords but in **concepts that must
be held simultaneously to read a line of code**. Go has 25 keywords but requires
a beginner to understand goroutines, channels, `select`, value-versus-pointer
receivers, nil interfaces versus nil pointers, and slice aliasing. Kite's budget
is spent as follows, and this list is complete:

1. `let` / `var` — immutable and mutable bindings
2. Primitive types, slices, maps, tuples, optionals
3. `fn` — functions, including closures
4. `if` / `for` / `match` — control flow
5. `struct` + `impl` — data and its methods
6. `enum` — alternatives
7. `trait` — shared behaviour
8. `(T, error)` — fallible results
9. `async` / `await` — operations that take time
10. `pub` + modules — encapsulation

There is no eleventh concept. Everything else in this document is a consequence
of these ten.

### 1.2 Why explicitness beats terseness here

Kite assumes code is read far more often than written, and that a significant
share of it is drafted with machine assistance. Under those assumptions the
costs invert:

- **Typing cost approaches zero.** Verbosity that would have been a burden in
  1995 is now nearly free to produce.
- **Reading cost dominates.** A reader — human or machine — must be able to
  determine what a line does without holding the rest of the file in memory.
- **Hidden control flow is the expensive thing.** An exception that unwinds
  through six frames, an implicit conversion, an overloaded operator, a
  destructor with side effects: each forces the reader to consult code that is
  not on screen.

So the language is built on what a reader can see. **Every call is a call you
can point at.** Every failure path is written on the line where it happens.
Every allocation is an expression. A name resolves to one declaration, an
operator does one thing, and a value changes only where a `var` says so.

### 1.3 Why immutable by default

This decision, made once, pays three times:

1. **It eliminates the pointer/value distinction.** Go beginners must learn when
   to write `func (p *Point)` versus `func (p Point)`. In Kite structs are always
   GC references and always passed by reference, but you cannot mutate one unless
   its fields were declared `var`. The confusing case disappears.
2. **It maps exactly onto WasmGC.** A WasmGC `struct` type declares a mutability
   flag *per field*. Kite's `var` marker on a field is the same bit. Immutable
   fields let the engine hoist and constant-fold loads without alias analysis.
3. **It makes most types thread-shareable for free.** See
   [§12.3](#123-the-share-marker). A deeply immutable value is safe to share by
   construction. Because immutability is the default, the overwhelming majority
   of user types qualify without the user ever thinking about it.

---

## 2. Lexical structure

### 2.1 Source encoding

Source files are UTF-8. The file extension is `.kite`. Identifiers may contain
any Unicode `XID_Start` / `XID_Continue` characters, so non-Latin identifiers are
supported. Source is normalised to NFC before comparison, so visually identical
identifiers are the same identifier.

### 2.2 Keywords

Twenty-seven, complete:

```
async    await    as       break    check    continue
defer    else     enum     false    fn       for
if       impl     in       let      match    nil
pub      return   self     struct   trait    true
type     use      var
```

### 2.3 Comments

```kite
// Line comment.

/// Documentation comment. Attaches to the following declaration.
/// Markdown is permitted. Code fences are extracted and compiled as tests.
```

### 2.4 Literals

```kite
42            // int
1_000_000     // underscores permitted as separators
0xFF  0o755  0b1010_1101
3.14          // float
1e10  1.5e-3
"hello"       // str
"line\nbreak"
"""
multi-line string, leading indentation stripped
to match the closing delimiter
"""
true  false
nil
```

String interpolation uses `\(expr)`:

```kite
let name = "world"
io.print("hello, \(name), you are \(age) years old")
```

Interpolation calls `Display.show` on the operand: a hole is an ordinary
expression, evaluated where it stands.

`int`, `float`, `bool` and `str` render themselves; every other type renders
through its own `Display`. Because a hole is an expression,
`"\(if n > 1 { "s" } else { "" })"` is a pluraliser and needs nothing added to
the language.

### 2.5 Semicolon insertion

Statements are newline-terminated. Semicolons are never written. A statement
continues onto the next line when the line ends in an operator, an open
delimiter, or a comma. The same rule as Swift and Kotlin, and unambiguous here
because a statement never begins with `(` or `[`.

**The operator ends the line it continues, and `||` is why that is a rule and
not a style.** A line opening with `||` is not the tail of the expression
above it: `||` where a value is expected is a closure with no parameters, so
the line parses, means nothing, and is discarded.

```kite
let ok = (c >= 48 && c <= 57)
    || (c >= 65 && c <= 90)     // error[E0117] — a closure, thrown away
```

The first line is already a complete statement. `&&` cannot do this, because it
may not begin an expression and so is a syntax error; `||` can, and it is the
one continuation a reader coming from another language writes by habit. It is
rejected as `E0117` rather than allowed to be quietly wrong.

---

## 3. Types

### 3.1 Primitives

| Type | Description | Wasm representation |
|---|---|---|
| `bool` | `true` / `false` | `i32` |
| `int` | 64-bit signed | `i64` |
| `float` | 64-bit IEEE-754 | `f64` |
| `str` | Immutable UTF-8 string | `externref` (JS string) or `array i8` |
| `error` | A failure, and what it says ([§7](#7-error-handling)) | GC reference |
| `JsValue` | An opaque host object; web only ([§15.1](#151-jsvalue)) | `externref` |

**There are one integer type and one float, and nothing converts between them
implicitly.** `let x: float = count` is a compile error; write `count as float`.
Sized numerics (`i32`, `u8`, `f32`) and `char` are *not* part of the language,
and a numeric literal may not carry a type suffix: `42i32` is `E0004`.

Integer overflow traps in debug builds and wraps in release builds, matching
the default most users expect while keeping release performance predictable.
`math.wrapping_add` wraps in both, and `math.checked_add` answers `Option<int>`
in both, for the code that has to mean one of the two regardless of how it was
built. Both are ordinary Kite over `math.max_int()` and `math.min_int()`, and
neither may overflow while deciding
whether an overflow would happen — which is why they subtract to ask rather
than adding and looking.

**On `str`:** the web target has two representations and a program cannot tell
them apart. By default a `str` is an index into a table the generated glue
holds, which needs no linear memory and runs in any engine with WasmGC. With
`--js-strings` it is an `externref` carrying the JavaScript string itself,
through the **JS String Builtins** proposal: constants arrive as imported
globals the engine synthesises from the literals, `+` and `==` compile to
intrinsics, and passing a string to a DOM API costs nothing — no copy, no
encoding pass, no lookup. It is a flag rather than the default because the
builtins are not in every engine, and a module that will not instantiate is a
worse failure than one that makes a call.

On native and bytecode targets, `str` is a GC-managed UTF-8 string. Kite
programs cannot observe any of this: `str` is indexed by character, never by
byte offset and never by UTF-16 code unit — which is why `len`, `slice`,
`index_of` and `code_at` remain host calls even where the builtins offer
something with the same name and a different meaning.

**What a `str` can do**, and it is deliberately little:

| Operation | Meaning |
|---|---|
| `s.len()` | Characters, not bytes and not UTF-16 code units |
| `s.slice(from, to)` | Characters `from..to`, clamped rather than trapping |
| `s.index_of(needle)` | The character index, or -1 |
| `s.trim()` | Leading and trailing whitespace removed |
| `s.code_at(i)` | The code point at character `i`, or -1 past the end |

Everything else — `split`, `starts_with`, `replace`, `join`, `words`,
`parse_int`, case folding — is written in Kite on top of these and lives in the
prelude, where it can be read. Each of these is a boundary two runtimes have to
agree about, and every one added is a thing that can drift.

`code_at` is the one that is not a string operation at all, and it is here
because it is the one thing nothing else can be built from: without a way to
see a character as a number, a hash, an ordering and a number parser each have
to become a boundary of their own. One general primitive is cheaper than three
special ones.

### 3.2 Composite types

```kite
[T]           // slice — a copy-on-write sequence
[N]T          // array — fixed length N, known at compile time
{K: V}        // map — hash map with deterministic iteration order
(A, B, C)     // tuple
Option<T>     // optional — either a T or nil
fn(A, B) -> C // function type
```

Maps iterate in **insertion order**. Go randomises map iteration to prevent
reliance on order; Kite instead guarantees an order, which is cheaper to reason
about and removes an entire class of nondeterministic test failure.

### 3.3 Optionals

`Option<T>` is the only place `nil` may appear other than the `error` slot
([§7](#7-error-handling)). There is no null reference. A `Config` is always a
`Config`; an `Option<Config>` might be nil, and the compiler will not let you use
it as a `Config` until you have handled that.

**An optional is opened by testing it**, and an `if` expression does that in one
line:

```kite
let maybe: Option<User> = users.find(id)

// The compiler narrows `maybe` to `User` in the branch where it cannot be nil.
let name = if maybe == nil { "anon" } else { maybe.name }

match maybe {
    nil  => io.print("not found"),
    user => io.print(user.name),    // `user` is bound as User, not Option<User>
}
```

**Narrowing** is what makes this ergonomic rather than tedious. Testing an
optional against `nil` narrows it to the unwrapped type on the branch where it
cannot be absent — in the `else` of `x == nil`, and in the `then` of `x != nil`.
The same narrowing applies in a `match` arm once an earlier arm has covered
`nil`.

### 3.4 Type declarations

```kite
type UserId = int                   // alias — interchangeable with int
type Celsius = float                // alias
type Prices = {str: int}            // any type, not just a primitive

pub struct Point {
    x: float
    y: float
}

pub enum Status {
    Active
    Suspended(reason: str)
    Deleted(at: Timestamp, by: UserId)
}
```

An alias is *replaced* by the type it names before anything else is checked, so
the two are the same type everywhere — a `UserId` adds no safety over an `int`,
and is not a way to get one. Aliases may name each other and may be declared in
any order.

Two forms are rejected, both because the replacement is the whole feature. A
circular alias (`type A = B` with `type B = A`) names nothing to be replaced
by. A generic alias (`type Pair<T> = (T, T)`) would need its arguments
substituted through the alias at every use, which is a second instantiation
path alongside the one structs and enums have; Kite has one. Both are
[`E0214`](#16-diagnostics).

---

## 4. Declarations and visibility

### 4.1 Bindings

```kite
let x = 42              // immutable; type inferred as int
let y: float = 3.0      // immutable, explicit type
var count = 0           // mutable
count = count + 1

let z: int              // declaration without initialiser
if condition {
    z = 1
} else {
    z = 2
}
// `z` is definitely-assigned here and immutable from now on
```

Deferred initialisation of a `let` is permitted provided the compiler can prove
exactly one assignment occurs on every path before first use. This removes the
main reason people reach for `var`.

Shadowing within a nested scope is permitted. Shadowing within the *same* scope
is an error — it is almost always a typo.

### 4.2 Visibility

`pub` is the only visibility modifier. There are exactly two levels:

- **Unmarked** — visible within the declaring module (a directory).
- **`pub`** — visible to anything that imports the module.

`pub` applies to modules, functions, types, struct fields, enum variants, traits,
and trait methods. A `pub struct` with unmarked fields is an opaque type: callers
can hold it and pass it, but cannot read, construct, or destructure it.

```kite
pub struct Connection {
    pub host: str       // readable by importers
    socket: Socket      // module-private
    var retries: int    // module-private and mutable
}
```

Two levels, and they compose with the module system rather than with a second
hierarchy: what a module exports is what `pub` marks, and what it keeps is
everything else.

### 4.3 Functions

```kite
pub fn add(a: int, b: int) -> int {
    return a + b
}

fn greet(name: str) {           // no return type means it returns nothing
    io.print("hello \(name)")
}

pub fn divide(a: int, b: int) -> (int, error) {
    if b == 0 {
        return _, errors.new("division by zero")
    }
    return a / b, nil
}
```

Parameters are immutable inside the body unless declared `var`. There are no
default arguments, no variadic parameters, no named arguments at call sites, and
no overloading. If a function needs many optional inputs, it takes a struct:

```kite
pub struct RequestOptions {
    method: str
    /// Milliseconds. There is no `Duration` type: `std/time` names the units
    /// in the functions that build one — `time.seconds(30)` is `30000` — and a
    /// wrapper around an `int` would buy nothing the name does not.
    timeout: int
    headers: {str: str}
}

pub fn request(url: str, opts: RequestOptions) -> (Response, error)

// call site
let (res, err) = http.request(url, RequestOptions{
    method:  "POST",
    timeout: time.seconds(30),
    headers: {"content-type": "application/json"},
})
```

Struct literals require field names, so this reads as well as named arguments
would, using machinery the language already has.

### 4.4 Closures

```kite
let double = |x: int| -> int { return x * 2 }
let double = |x| x * 2                       // types inferred, expression body

let total = items.fold(0, |acc, item| acc + item.price)
```

**Closures capture by value, taken when the closure is made.** Because `let`
bindings are immutable, the vast majority of captures are trivially safe: the
value cannot change afterwards, so by-value and by-reference cannot be told
apart.

**Capturing a `var` is a compile error** ([E0211](#16-diagnostics)). A by-value
capture of a mutable binding would not see later writes, and code that reads one
expecting it to is a bug that no diagnostic could find afterwards. Promoting the
binding to a heap cell would make the write visible, and was specified here
before the compiler was built — but it buys shared mutable state through a
capture list, which is the thing this language spends most of its omissions
avoiding.

```kite
var total = 0
let add = |n: int| { total = total + n }    // error[E0211]
```

To let a closure change something, **capture a `let` handle to a struct and pass
it to a function that takes it as `var`.** Structs are references
([§14](#14-memory-model)), so the write lands where the holder can see it, and
it happens through a named function rather than through a capture:

```kite
struct Counter {
    var count: int
}

let state = Counter{ count: 0 }
let bump = || { increment(state) }          // captures a `let`, by value

fn increment(var c: Counter) {
    c.count = c.count + 1
}
```

This is the idiom for every event handler, timer and observer callback a program
writes, and it is deliberate that mutation is spelled out in a signature rather
than implied by a capture.

A closure that captures a host reference is not `Share`
([§12.3](#123-the-share-marker)).

---

## 5. Expressions

### 5.1 Operator precedence

Highest to lowest:

| Level | Operators | Associativity |
|---|---|---|
| 1 | `a.b`  `a(…)`  `a[…]` | left |
| 2 | `-a`  `!a` | prefix |
| 3 | `as` | left |
| 4 | `*`  `/`  `%` | left |
| 5 | `+`  `-` | left |
| 6 | `<<`  `>>` | left |
| 7 | `&`  `^`  `\|` | left |
| 8 | `==` `!=` `<` `<=` `>` `>=` | non-associative |
| 9 | `&&` | left |
| 10 | `\|\|` | left |
| 11 | `..`  `..=` | non-associative |


Bitwise operators bind tighter than comparison, unlike C. `a & b == c` means
`(a & b) == c`, which is what everyone intends and C gets wrong. Comparison is
non-associative: `a < b < c` is a syntax error, not a silent bug.

A range is the loosest operator there is, so `0..n + 1` is `0..(n + 1)`, which
is how it reads. It is non-associative too: `a..b..c` has no meaning to give.

### 5.2 Equality

`==` is structural for all types: two structs are equal when their fields are
equal, two slices when their elements are. There is no reference equality
operator in the surface language; `ptr.same(a, b)` is a compiler builtin, for
the rare case that needs it.

`ptr.same` answers whether two names refer to one heap cell, which `==` cannot
express: two distinct values with identical fields are equal and are not the
same cell. Both arguments must have the same type, and that type must be a
**struct, enum or map** — the three that *are* a cell two names can share.
Everything else is rejected — `E0213` — each for its own reason: a number or a
`str` has no cell; a slice has one but is copy-on-write, so two sharing a
buffer is an allocator fact that a write to either would end; a function and a
`dyn` have no stable identity to report, which is why `==` is undefined on them
too.

The motivating case is a fixpoint. A loop that repeats while a value keeps
changing must ask "is this the value I passed in?", and structural equality
answers a different question at the cost of walking the whole value.

Floating-point `==` follows IEEE-754, so `nan != nan`. The compiler emits a
warning when both operands of `==` are statically known to be floats and neither
is a literal, suggesting `math.approx_eq`.

### 5.3 Struct literals

```kite
let p = Point{ x: 1.0, y: 2.0 }

// functional update — produces a new value, does not mutate
let q = Point{ ..p, y: 5.0 }
```

All fields must be given unless `..base` is used. There are no zero values in
Kite — a struct literal that omits a field without `..` is a compile error. This
removes Go's most common production bug, where a forgotten field silently
becomes `0`, `""`, or `nil`.

### 5.4 Slices and maps

```kite
let xs = [1, 2, 3]
let ys: [int] = []
let m = {"a": 1, "b": 2}

xs[0]                 // int — bounds-checked, traps on failure
xs.get(0)             // Option<int> — bounds-checked, nil on failure
xs[1..3]              // [int] — subslice, half-open
xs.len()              // int
m["a"]                // Option<int> — map indexing always yields an optional
```

Map indexing returns `Option<V>`, never a zero value. Slice indexing with `[]` traps on
out-of-bounds because that is a program bug, not a runtime condition; `.get()` is
provided for the case where it genuinely is a runtime condition.

---

## 6. Statements and control flow

### 6.1 `if`

```kite
if x > 10 {
    io.print("big")
} else if x > 5 {
    io.print("medium")
} else {
    io.print("small")
}
```

Parentheses around the condition are not permitted. Braces are always required.
The condition must be `bool` — there is no truthiness.

`if` is also an expression when every branch yields a value and an `else` is
present:

```kite
let label = if x > 10 { "big" } else { "small" }
```

### 6.2 `for`

`for` is the only loop keyword. It has three forms.

```kite
// 1. Iterate a slice, a range, or a map. (The `Iterate` trait that would
//    generalise this to a user type needs associated types; see §10.4.)
for item in items {
    io.print(item)
}

for i in 0..10 { }          // range, half-open: 0 through 9
for i in 0..=10 { }         // inclusive range

for (key, value) in m { }   // maps yield tuples
for (i, item) in items.enumerate() { }

// 2. Conditional
for count < 10 {
    count = count + 1
}

// 3. Unconditional
for {
    if done { break }
}
```

The three headers above are the whole of iteration: over a sequence, while a
condition holds, and forever. Labelled
`break` and `continue` are supported for nested loops:

```kite
outer: for row in grid {
    for cell in row {
        if cell.empty { continue outer }
    }
}
```

### 6.3 `defer`

```kite
fn process(path: str) -> (Data, error) {
    let (file, err) = fs.open(path)
    check err
    defer file.close()

    // ... any return from here closes the file
}
```

Deferred calls run in reverse order of registration when the enclosing function
returns, by any path. Unlike Go, `defer` cannot modify the return value — it is
purely for release of resources, which is the only use that survives scrutiny.

### 6.4 `match`

See [§9](#9-enums-and-pattern-matching).

---

## 7. Error handling

This is the part of Kite that differs most from its influences, so the reasoning
is given in full.

### 7.1 The problem being solved

Go's `(T, error)` convention is correct in philosophy: errors are ordinary
values, every failure point is visible in the source, and there is no invisible
unwinding. Its flaws are not in the shape but in the enforcement:

1. **An error can be silently dropped.** `v, _ := f()` compiles, and so does
   simply never testing `err`.
2. **The value is valid-looking on the error path.** When `f` fails, `v` is the
   zero value — `0`, `""`, `nil` — and it flows onward indistinguishably from a
   real result. This is the mechanism behind a large share of production nil
   dereferences.
3. **There is no exhaustiveness.** Nothing checks that you handled the error at
   all.

In 2025 the Go team formally announced they will pursue **no further
error-handling syntax proposals**, closing the door on fixing this within Go. So
the shape is worth keeping and the enforcement is worth adding.

### 7.2 The `error` type

```kite
pub trait Error {
    fn message(self) -> str
}
```

`error` is a built-in nil-able type — either nil, or a value describing a
failure.

`Error` is declared in the prelude, beside `Display` and `Debug`, and **any type
may implement it**. A value whose type does is accepted wherever an `error` is
expected; the conversion happens at that point and is an ordinary call in the
IR, so nothing about it is hidden from a reader of the generated code.

> **What is built and what is not.** `impl Error for MyType` compiles, and
> returning one in an error slot works on all three backends. What an `error`
> *carries* is still the message: the conversion renders it where the failure
> happened, and the original value is not kept. So `errors.chain`,
> `errors.is<T>` and `errors.as<T>` in [§7.6](#76-adding-context) are still
> absent — they need the value alongside the message, which is a change to the
> representation rather than to the conversion, and it is
> [Phase 24's remaining half](../docs/06-roadmap.md#phase-24--concrete-error-types).
> `cause` is absent for the same reason.

```kite
pub struct NotFound {
    pub resource: str
    pub id: str
}

impl Error for NotFound {
    fn message(self) -> str {
        return "\(self.resource) \(self.id) not found"
    }
}
```

### 7.3 Correlated results and taint analysis

A function returning `(T, error)` returns a **correlated pair**. The compiler
tracks two flow-sensitive states across the function body:

- The error binding is **Unchecked** or **Checked**.
- The value binding is **Tainted** or **Clean**.

The rules:

> **R1.** After `let (v, e) = f()`, `e` is Unchecked and `v` is Tainted.
>
> **R2.** Reading a Tainted binding is a compile error (`E0301`).
>
> **R3.** An Unchecked binding going out of scope is a compile error (`E0302`).
>
> **R4.** On any path where the compiler proves `e == nil`, `e` becomes Checked
> and `v` becomes Clean.
>
> **R5.** On any path where `e != nil`, `e` becomes Checked and `v` remains
> Tainted permanently. The value slot on an error path holds no value at all —
> not a zero value — and cannot be read.
>
> **R6.** A call left as a bare statement, whose type is `error` or `(T,
> error)`, is a compile error (`E0302`). Binding nothing is not a way out of
> binding an error.

R1–R5 are about bindings, and R6 is what closes the shape they leave open: a
call written as a statement makes no binding, so nothing in R1–R5 ever sees it,
and `dom.set_text(e, "hi")` would drop its failure in silence. That is
[§7.1](#71-the-problem-being-solved)'s first flaw arriving through the one door
the analysis did not watch, and it is the ordinary shape on the web, where
nearly every `std/dom` function answers with a bare `error`.

To drop one deliberately, say so:

```kite
_ = dom.set_text(note, "…")
```

`_` already means *a hole where a value would go* in a return and in a
destructuring; this is the same word in the one position that lacked it. What
it buys is not safety — the error is just as gone — but that it is gone because
somebody decided, on a line a reader can see and `grep` can find.

The analysis is a standard forward dataflow pass over the control-flow graph,
run after type checking. It is not a borrow checker; it has no notion of
ownership, aliasing, or lifetimes, and it terminates in a single pass because the
lattice has height two.

In practice:

```kite
fn title_of(document: str) -> (str, error) {
    let (parsed, err) = json.parse(document)
    // parsed: Tainted    err: Unchecked

    if err != nil {
        return _, err       // parsed is still Tainted here — cannot be used
    }
    // parsed: Clean     err: Checked

    return json.text_or(parsed, "title", "untitled"), nil
}
```

Attempting to skip the check:

```kite
fn broken(document: str) -> str {
    let (parsed, err) = json.parse(document)
    return json.text_or(parsed, "title", "untitled")
}
```

```
error[E0301]: `parsed` is used before `err` has been checked
   ┌─ titles.kite:3:31
   │
 2 │     let (parsed, err) = json.parse(document)
   │          ------  --- this error is never checked
   │          │
   │          `parsed` is only valid when `err` is nil
 3 │     return json.text_or(parsed, "title", "untitled")
   │                         ^^^^^^ used here while still tainted
   │
help: check the error first
   │
 3 │     check err
 4 │     return json.text_or(parsed, "title", "untitled"), nil
   │
```

### 7.4 The `check` keyword

The propagation case — *"if this failed, my caller should deal with it"* — is
the overwhelming majority of error handling in real code. It gets one keyword:

```kite
check err
```

which is defined as exactly:

```kite
if err != nil {
    return _, err
}
```

`check` is only valid inside a function whose last return component is `error`.
`_` in a return's value position means *no value*; it is not a zero value and
the correlated pair records the error branch.

This is deliberately **not** Rust's `?`. A postfix `?` disappears into the middle
of an expression and permits nesting failures inside a larger expression. `check`
occupies its own line, is greppable, and preserves Go's central virtue: you can
scan the left margin of a function and see every place it can fail.

```kite
pub fn load_config(path: str) -> (Config, error) {
    let (bytes, err) = fs.read(path)
    check err

    let (text, err) = str.from_utf8(bytes)
    check err

    let (cfg, err) = toml.parse(text)
    check err

    return cfg, nil
}
```

Rebinding `err` in the same scope is permitted, and is the one exception to the
same-scope shadowing rule in [§4.1](#41-bindings) — but only because the previous
`err` is provably Checked at that point.

### 7.5 Handling a failure in place

To handle a failure rather than propagate it, test the error. In the branch where
it is nil, the value becomes readable:

```kite
let (port, err) = config.get_int("port")
let port = if err != nil { 8080 } else { port }
```

The branch is written out, on the line where the failure happens, which is what
makes every failure path visible in the source.

### 7.6 Adding context

```kite
let (bytes, err) = fs.read(path)
check errors.wrap(err, "loading config from \(path)")
```

`errors.wrap` returns nil when given nil, so this composes with `check`
directly. The context goes in front of the message, so a failure that crosses
four layers reads as the four sentences that produced it.

### 7.7 Unrecoverable failures

Some conditions are not errors — they are bugs. Array index out of range,
integer division by zero, an exhausted invariant. These **trap**: the Wasm
`unreachable` instruction on the web target, `abort` on native. A trap is not
catchable. There is no `recover`, no panic handler, and no unwinding.

`assert(cond, msg)` traps when `cond` is false. It is compiled out in release
builds; `require(cond, msg)` is the always-on variant.

This is a deliberate rejection of Go's `panic`/`recover`, which creates a second,
invisible error-propagation channel alongside the visible one.

---

## 8. Structs and methods

### 8.1 Declaration

```kite
pub struct Rect {
    pub width:  float
    pub height: float
    pub var label: str      // mutable field
}
```

Struct values are GC-managed references. Assignment copies the reference, not the
contents. Because fields are immutable unless marked `var`, this is
indistinguishable from value semantics for the majority of types, without the
copying cost or the pointer/value receiver distinction.

### 8.2 Methods

```kite
impl Rect {
    pub fn area(self) -> float {
        return self.width * self.height
    }

    pub fn scaled(self, factor: float) -> Rect {
        return Rect{ ..self, width: self.width * factor, height: self.height * factor }
    }

    pub fn rename(var self, name: str) {
        self.label = name       // permitted: `var self` and `label` is `var`
    }

    // Associated function — no self
    pub fn square(side: float) -> Rect {
        return Rect{ width: side, height: side, label: "" }
    }
}
```

`self` is immutable unless the method declares `var self`. A method with `var
self` cannot be called on a binding the caller does not own mutably.

`Rect.square(2.0)` calls the associated function; `r.area()` calls the method.

Multiple `impl` blocks for the same type are permitted within a module. A type's
inherent methods must be declared in the module that declares the type — there
are no extension methods, so `x.foo()` can always be resolved by looking at where
`x`'s type is defined.

---

## 9. Enums and pattern matching

### 9.1 Enums

```kite
pub enum Shape {
    Circle(radius: float)
    Rect(width: float, height: float)
    Point
}

pub enum Json {
    Null
    Bool(bool)
    Number(float)
    Text(str)
    Array([Json])
    Object({str: Json})
}
```

Variants may carry named or positional payloads. Enums are recursive by default —
`Json` above needs no boxing annotation, because every Kite aggregate is already
a GC reference.

### 9.2 `match`

```kite
let description = match shape {
    Circle(radius) => "circle of radius \(radius)",
    Rect(w, h) if w == h => "square of side \(w)",
    Rect(w, h) => "rect \(w)x\(h)",
    Point => "a point",
}
```

`match` is exhaustive. Omitting a variant is a compile error that names the
missing variants:

```
error[E0210]: non-exhaustive match
   ┌─ shapes.kite:4:22
   │
 4 │     let d = match shape {
   │                   ^^^^^ variants `Point` and `Rect` not covered
   │
help: add the missing arms, or a catch-all `_ =>`
```

Exhaustiveness is what makes adding an enum variant safe: the compiler shows you
every place that must change.

### 9.3 Patterns

```kite
match value {
    0            => "zero",              // literal
    1 | 2 | 3    => "small",             // alternation
    4..=9        => "medium",            // range
    n if n < 0   => "negative",          // guard
    _            => "large",             // wildcard
}

match point {
    Point{ x: 0.0, y: 0.0 } => "origin",   // struct pattern
    Point{ x: 0.0, y }      => "on y axis at \(y)",
    Point{ x, y }           => "at \(x),\(y)",
}

match pair {
    (nil, nil)   => "neither",
    (a, nil)     => "first only",
    (nil, b)     => "second only",
    (a, b)       => "both",
}
```

Bindings introduced by patterns are immutable. There is no `ref` or `mut` in
patterns because there are no references to bind.

---

## 10. Traits

### 10.1 Declaration and implementation

```kite
pub trait Display {
    fn show(self) -> str
}

pub trait Comparable {
    fn compare(self, other: Self) -> Ordering

    // Default methods
    fn less_than(self, other: Self) -> bool {
        return self.compare(other) == Ordering.Less
    }
}

impl Display for Rect {
    fn show(self) -> str {
        return "Rect(\(self.width) x \(self.height))"
    }
}
```

Trait implementation is **explicit and nominal**, unlike Go's structural
interfaces. The reasoning: structural satisfaction produces error messages that
name the missing method but cannot name the intent, and it makes accidental
satisfaction possible. `impl Display for Rect` is a statement the author made on
purpose, and the compiler can say "`Rect` does not implement `Display`" with a
precise place to point at.

`Self` inside a trait refers to the implementing type.

### 10.2 Coherence

A trait implementation is permitted only in the module that declares the trait or
the module that declares the type. This is the orphan rule, and it guarantees
that a given (trait, type) pair has exactly one implementation program-wide,
which is what makes trait resolution decidable and separate compilation possible.

### 10.3 Static and dynamic dispatch

```kite
// Static — monomorphised at compile time, zero-cost, no indirection
fn render<T: Display>(item: T) {
    io.print(item.show())
}

// Dynamic — one machine-code copy, indirect call through a vtable
fn render_all(items: [dyn Display]) {
    for item in items {
        io.print(item.show())
    }
}
```

`dyn Trait` is required to be explicit. A heterogeneous collection needs `dyn`;
a generic function does not. On the Wasm target, `dyn Trait` lowers to a WasmGC
struct holding the data reference plus a vtable of typed function references
(from the typed-function-references feature ratified in Wasm 3.0), so the
indirect call is type-checked by the engine rather than through a signature
table.

Not every trait can be made `dyn`. A trait is **object-safe** when no method
takes or returns `Self` by value and no method is generic. Non-object-safe traits
can still be used as generic bounds; the error message says which method is
responsible.

### 10.4 Built-in traits

Three of these the compiler applies on its own; the rest are asked for, and one
is refused. The distinction is not arbitrary — it is whether a mechanical answer
is the *right* answer.

| Trait | Meaning | How it arrives |
|---|---|---|
| `Eq` | `==` and `!=` | Structural, on every type, always |
| `Share` | Safe to move across tasks | Inferred structurally — see [§12.3](#123-the-share-marker) |
| `Display` | String interpolation, `io.print` | Written by hand, never derived |
| `Debug` | A rendering for a programmer | `@derive(Debug)` |
| `Hash` | One integer standing for a value | `@derive(Hash)` |
| `Encode` / `Decode` | To and from `json.Json` | `@derive(Encode, Decode)` |
| `Ord` | `<` `<=` `>` `>=` | Not a trait — see below |
| `Iterate` | `for x in …` | Not written — see below |

**`Eq` is not a trait.** `==` is structural on every Kite value: a struct
compares its fields, an enum its tag and then its payload, a slice its length
and then its elements. There is nothing to implement, nothing to derive, and no
type that lacks it — so a trait for it would be a second spelling for what the
language already does, and a second spelling is a chance for two answers.

**`Display` is deliberately not derived.** How a type presents itself to a human
is a design decision, not a mechanical one, and a `Password` whose derived form
printed its field is the case where being wrong matters.

**`Ord` is not a trait either.** `<` on aggregates is not defined: what order
two structs are in is a decision with several defensible answers, and the
language declines to pick one. Sorting takes the comparison as an argument —
`sorted(people, |a, b| a.age < b.age)` — which is where the decision belongs.

#### `@derive`

`@derive(…)` writes a body from a type's fields. It is one of the two
attributes Kite has, and the bar an attribute must clear is that it names
something the compiler must do which no amount of Kite could say instead.

```kite
@derive(Debug, Hash, Encode, Decode)
pub struct User {
    name: str
    age: int
    tags: [str]
}
```

What it produces is **ordinary Kite**, expanded before resolution: it is
checked, lowered and optimised like anything hand-written, `kitec --emit hir`
shows what actually ran, and a derived method is not privileged over a written
one. Deriving something a type already implements by hand is an error rather
than a silent replacement.

The walk handles primitives, slices, maps, optionals, tuples, and other types
that derive the same trait. Where it cannot go — a function field, a `dyn
Trait`, a type parameter — it says which field stopped it and what would fix
it, and the hand-written implementation is still there to write.

`Decode` is an **associated function**, not a trait method: it produces the
implementing type, and a trait method cannot say that without `Self` in return
position. So a document becomes a value by naming the type, which is what a
caller has anyway:

```kite
let (doc, err) = json.parse(text)
check err
let (user, uerr) = User.decode(doc)
check uerr
```

There is no `json.decode<T>(text)`. Kite has no turbofish, so the type would
have to be inferred from the binding, and `User.decode` says the same thing
where it can be read.

**`Iterate` cannot be written in this language yet, and the implementation says
so rather than pretending.** A trait that yields values needs to name the type
it yields, which is an associated type — and [§11](#11-generics) excludes
associated types from version 1.0 on the grounds that they cost error-message
quality. The two decisions are in tension, and the tension is resolved for now
in favour of the simpler type system: `for x in …` works over ranges, slices
and maps, all three of which the compiler knows the element type of directly. A
user type becomes iterable by exposing a slice.

Whichever way this is settled later, it is a real change: adding associated
types is a type-system change, and special-casing `Iterate` in the compiler is
a language with one magic trait in it.

---

## 11. Generics

```kite
pub fn map<T, U>(items: [T], f: fn(T) -> U) -> [U] {
    var out: [U] = []
    for item in items {
        out.push(f(item))
    }
    return out
}

pub struct Cache<K: Hash, V> {
    var entries: {K: V}
    capacity: int
}

impl<K: Hash, V> Cache<K, V> {
    pub fn get(self, key: K) -> Option<V> {
        return self.entries[key]
    }
}
```

Generics are **monomorphised**: each distinct instantiation produces its own
specialised code. This gives static dispatch and full inlining, at the cost of
binary size when a generic function is instantiated at many types.

Because binary size is a first-order concern on the web, the compiler applies
**identical-code-folding** after monomorphisation: instantiations whose generated
Wasm bodies are byte-identical (very common — `[User]` and `[Post]` produce the
same code when the operations are all reference moves) are merged into one
function. Where folding is not possible and the instantiation count is large, the
compiler emits a size warning naming the function, and `dyn` is the suggested
remedy.

Bounds are trait names, and a parameter satisfies one by implementing it. That
is the whole of the system: a generic function is a function whose parameter
types are named rather than fixed, and monomorphisation makes each use an
ordinary call.

---

## 12. Concurrency

### 12.1 The model

```kite
pub async fn fetch_user(id: UserId) -> (User, error) {
    let (res, err) = await http.get("/api/users/\(id)")
    check err

    let (doc, err) = json.parse(res.body)
    check err

    let (user, uerr) = User.decode(doc)
    check uerr

    return user, nil
}
```

An `async fn` returns a `Task<T>`. `await` suspends until it completes. Calling
an `async fn` without `await` starts it and yields the `Task` — this is how
concurrency is expressed:

```kite
// Sequential — 200ms total
let (a, err) = await fetch_user(1)
check err
let (b, err) = await fetch_user(2)
check err

// Concurrent — 100ms total
let ta = fetch_user(1)
let tb = fetch_user(2)
let ((a, ea), (b, eb)) = await task.both(ta, tb)
check ea
check eb
```

`task.all([...])`, `task.race([...])`, and `task.timeout(t, ms)` cover the
remaining combinators. There is no channel type; a `Task<T>` *is* the
one-shot result channel, and it is awaited rather than received from.

### 12.2 Parallelism: the surface is thread-agnostic

**`async` says nothing about how many threads exist.** That is a property of the
runtime, and the surface does not change when the runtime gains one:

| Target | Scheduler | Real parallelism |
|---|---|---|
| `wasm32-gc` (web) | Cooperative loop on the main thread | **No** |
| `kbc` (bytecode VM) | Cooperative loop; the VM's values are `Rc`-based | **No** |
| `native-*` | Cooperative loop | **No** |
| any, later | Work-stealing pool, one worker per core | **When the runtime has one, with no source change** |

**Nothing here runs on two cores today, and this table used to claim two of
them did.** That was the specification describing the intended runtime rather
than the one that exists: there is no thread spawned anywhere in the compiler or
the runtime, and `task.parallel` walks its input in order. The rest of this
section is about why the *type system* is finished even though the scheduler is
not, and that part is true.

The web restriction is not a design choice. WasmGC references **cannot currently
cross a thread boundary at all** — there is no way to share a reference value
between Wasm threads. The
[shared-everything-threads proposal](https://github.com/WebAssembly/shared-everything-threads)
exists precisely to fix this and is still a **draft**. This is why Kotlin/Wasm's
`Dispatchers.Default` and `Dispatchers.IO` silently execute on the main thread,
and why Flutter's multi-threaded web rendering requires COOP/COEP headers and
still cannot share its object graph.

**The point of specifying `Share` now is that Kite programs become parallel on
the web the day that proposal ships, without a source change.** The type system
already enforces the invariant the proposal will require. This is the single
most important forward-compatibility decision in the language.

`task.parallel` is the shape that work will arrive in, and **it is not
parallel on any target today** — it applies the function to each item in turn,
yielding between them. What is real about it now is the *rule*: its argument and
its result must be `Share`, so the day a reference can cross a thread boundary,
the same source starts using cores and nothing about it is rewritten. That is
the whole point of specifying the marker before the platform can honour it:

```kite
let results = await task.parallel(chunks, |chunk| {
    return heavy_transform(chunk)     // chunk and result must be Share
})
```

### 12.3 The `Share` marker

`Share` is an auto-derived marker trait meaning *"a value of this type may be
moved to another thread or isolate."*

A type is `Share` when:

- it is a primitive, or
- it is a `str`, or
- it is a struct or enum **all of whose fields are `Share` and none of which is
  `var`**, or
- it is a slice, map, or tuple of `Share` elements, or
- it is explicitly wrapped: `sync.Mutex<T>` or `sync.Atomic`.

A type is **not** `Share` when it has a `var` field anywhere in its transitive
structure, or when it holds a `JsValue` — a DOM node, a canvas context, a file
handle ([§15.1](#151-jsvalue)). A host reference belongs to the isolate that
created it, and an integer standing in for one would carry none of that: it
would satisfy every rule above and mean nothing on the other side.

Because struct fields are immutable by default, **most user types are `Share`
without the author doing anything or knowing the trait exists.** The marker only
becomes visible when it is violated:

```
error[E0520]: `Counter` cannot be moved to another task
   ┌─ worker.kite:12:31
   │
12 │     await task.parallel(items, |c| c.tick())
   │                                    ^ `Counter` is not Share
   │
   ┌─ counter.kite:2:5
   │
 2 │     var count: int
   │     --- because this field is mutable, `Counter` may not be shared
   │
help: two values of a mutable type in two threads is a data race. Either
      make `count` immutable and return a new Counter, or wrap the type
      in `sync.Mutex<Counter>` to serialise access.
```

Kite therefore has **no data races by construction**, on every target, with no
annotation burden in the common case. This is the same insight as Rust's `Send`
and Swift 6's `Sendable`, made nearly invisible by choosing immutability as the
default.

### 12.4 Implementation

`async fn` compiles to a state machine: the function body is split at each
`await` into a resumable coroutine object, with locals that live across a
suspension point stored in a WasmGC struct. This is the same transformation
Rust, C#, and Kotlin use, and it requires no Wasm features beyond those ratified
in 3.0.

The [stack-switching proposal](https://github.com/WebAssembly/stack-switching)
would permit a cheaper implementation using real coroutine stacks. It is
post-3.0 and not yet shipped. Kite's semantics are compatible with either
lowering, so adopting it later is a compiler change with no language change.

---

## 13. Modules and packages

### 13.1 Structure

A **module** is a directory. Every `.kite` file in it contributes to the same
namespace — there are no per-file imports of sibling files and no header/
implementation split.

```
myapp/
  kite.toml
  src/
    main.kite
    config/
      load.kite
      schema.kite      // same module as load.kite
    ui/
      app.kite
      theme.kite
```

```kite
use config
use ui
use std/http
use std/json as j

fn main() {
    let (cfg, err) = config.load("app.toml")
    check err
    ui.run(ui.App{ config: cfg })
}
```

Imports are always qualified by module name at the use site. There is no
wildcard import and no way to bring a bare name into scope. `config.load` always
tells you where `load` came from.

### 13.2 Manifest

```toml
[package]
name    = "myapp"
version = "0.1.0"

[targets]
web    = { entry = "src/main.kite", renderer = "dom" }
native = { entry = "src/main.kite" }

[dependencies]
markdown = { git = "https://github.com/example/kite-markdown", tag = "v1.2.0" }
```

Dependencies are resolved to a lockfile with content hashes. There is no
post-install script mechanism, no transitive-dependency hoisting, and no way for
a dependency to execute code at build time — the supply-chain attack surface that
has repeatedly compromised npm is absent by construction rather than by policy.

### 13.3 Cycles

Module cycles are an error. Cyclic dependencies make separate compilation,
incremental rebuilds, and initialisation order all harder, and every cycle can be
broken by extracting the shared part.

---

## 14. Memory model

Kite is garbage-collected on every target. There is no manual allocation, no
`free`, no ownership, no borrowing, and no lifetimes.

| Target | Collector |
|---|---|
| `wasm32-gc` | **The host engine's collector.** WasmGC objects are allocated with `struct.new` / `array.new` and traced by V8, SpiderMonkey, or JavaScriptCore directly. Kite ships no collector in the binary. |
| `native-*` | Precise tracing collector: generational, non-moving in v1. Type maps emitted by the compiler give exact root and field information. |
| `kbc` | Same collector as native. |

Delegating collection to the browser engine on the web target is the single
largest binary-size win available in 2026, and it is why this design was not
viable before WasmGC reached cross-browser baseline in Safari 18.2.

**Known consequences of WasmGC's current shape**, accepted deliberately:

- **No interior pointers.** A reference always points to the head of an object.
  Kite has no `&x.field`, so this is unobservable.
- **No unboxed aggregates inside arrays.** `[Point]` is an array of references to
  `Point` objects, not a flat buffer of `(f64, f64)`. For numeric work where the
  layout matters, `buffer.F64` provides a flat typed buffer over linear memory,
  which is the escape hatch for anything holding a great many numbers — a
  simulation, a signal, a mesh.
- **No weak references or finalizers.** A `Cache` that must not retain its
  entries uses an explicit eviction policy rather than weak keys.

### 14.1 Exclusivity

Collection settles memory safety. It does not settle the one hazard that
reference semantics introduce on their own: the same object arriving at a
function twice, under two names, where writing through one is invisible to the
other.

```kite
fn transfer(var from: Account, var to: Account, amount: int) {
    from.balance = from.balance - amount
    to.balance   = to.balance   + amount
}

transfer(a, a, 50)          // rejected: E0800
```

Written this way the balance is set to 50 and then back to 100. Nothing traps
and nothing is unsafe — the memory is real on both lines — and the program is
silently wrong.

**The rule: while an object is being written through one argument, no other
argument of the same call may name it.** Two arguments name the same object when
one path is a prefix of the other, so `f(o, o.inner)` is rejected alongside
`f(a, a)`; `f(o.left, o.right)` is not, because neither path contains the other.
A literal index distinguishes elements, so `f(xs[0], xs[1])` is accepted and
`f(xs[i], xs[j])` is not — the compiler cannot show that `i` and `j` differ, and
the call is wrong on the run where they do not.

Only reference types participate — a struct or a `dyn Trait`. Slices, maps and
tuples are copy-on-write values, so a `var [T]` parameter is the callee's own
copy and two of them cannot interfere.

**This is not borrowing.** There is no ownership, no move, no lifetime, and
nothing to annotate. A borrow checker exists to replace a collector, which is
what forces it to reason about every reference in the program and to be complete
enough that a rejected program has a rewrite. Kite collects, so this rule is free
to be incomplete: it reads one call site, and it reports only what is written
there.

The consequence, stated plainly: **aliasing arranged elsewhere is not detected.**

```kite
let shared = Account{ balance: 100 }
let pair   = Pair{ left: shared, right: shared }

transfer(pair.left, pair.right, 50)     // accepted — the same bug
```

Seeing through that assignment is alias analysis, and alias analysis is the rest
of a borrow checker. The bug it leaves behind is a wrong number, not a wrong
address, and the collector guarantees it stays that way.

Two rules a Rust programmer would expect are absent because Kite's value
semantics already settle them. A `for x in xs` loop walks a snapshot, so growing
`xs` in the body terminates and is defined; and a slice passed to a function is
copied, so a push inside is not something the caller can observe.

---

## 15. Foreign function interface

The web target has no direct DOM access — no Wasm proposal for calling Web IDL
without JavaScript glue has been standardised, and none is imminent. Kite
therefore defines the boundary explicitly rather than pretending it is not
there.

### 15.1 `JsValue`

```kite
pub struct Element {
    raw: JsValue        // unmarked: opaque outside this module
}
```

`JsValue` is a host reference. On the web it lowers to `externref`; on every
other target it names a diagnostic rather than a value, because there is nothing
for it to refer to.

| Property | Reason |
|---|---|
| Opaque | Kite cannot read inside it. It is the host's object, not a Kite one. |
| Not `Share` | It belongs to one isolate ([§12.3](#123-the-share-marker)). |
| Not comparable with `==` | `externref` is outside Wasm's `eq` hierarchy, so there is no structural answer to give. Identity is `js.same(a, b)`, which is `===`. Writing `==` on one is a compile error rather than a quiet wrong answer. |
| Cannot be forged | There is no literal for it. |

**Lifetime needs no rule.** On the web the Wasm heap *is* the JavaScript heap, so
a Kite struct holding an element — whose listener holds a Kite closure, which
holds the struct — is a cycle across the boundary that the one collector
collects. There is no ownership protocol, no release call, and no table of
integers to keep in step. This is the whole argument for a reference over a
handle, and it is not recoverable by any amount of care with integers: nothing
can tell the host that Kite dropped a number, and WasmGC has no finalizers.

### 15.2 Two mechanisms, and which is which

**`extern` declares one named function.**

```kite
@host("net")
extern fn connect(host: str, port: int) -> JsValue
```

Direct, monomorphic, and checked at the call. It is how `std/fs`, `std/http`,
`std/socket` and `std/crypto` are built, and how the standard library reaches
anything it calls often enough for a name lookup to matter. Drawing does not use
it at all: the drawing calls are compiler builtins, so a program that paints
needs no `extern`.

**`std/js` declares nothing.** It is a fixed set of about twenty primitives
through which any host object can be reached:

| | |
|---|---|
| `js.global(name)` / `js.nothing()` | the root — `window`, `document`, a constructor — and `null` |
| `js.get(v, name)` / `js.set(v, name, x)` | properties |
| `js.at(v, i)` / `js.length(v)` | an array-like thing |
| `js.call0(v, name)` … `js.call4(v, name, a, b, c, d)` | methods, by arity |
| `js.new0(name)` … `js.new3(name, a, b, c)` | construction, by arity |
| `js.func(f)` | a Kite closure the host can call |
| `js.settle(p, done, failed)` | both halves of a promise |
| `js.same(a, b)` / `js.is_nothing(v)` / `js.kind_of(v)` / `js.instance_of(v, name)` | identity and kind |
| `of_str` `of_num` `of_bool` `of_int` / `as_str` `as_num` `as_bool` `as_int` | conversion, both ways |
| `str_or` `num_or` `bool_or` | a property with a value to fall back on |

Everything else — `std/dom`, and any browser API a program needs — is ordinary
Kite written over these.

**One function per arity, and no `js.await`.** A slice is a Kite aggregate and
does not cross the boundary, so `call` and `new` are spelled out rather than
taking an argument list. And a promise arrives through `js.settle`, which
requires a handler for the rejection as well as the result: `then` with one
callback compiles, runs, and throws a rejection away, which is the failure this
language spends its error design preventing everywhere else.

**Why the general mechanism is the primary one.** The browser has thousands of
interfaces. With `extern` alone, the first one the standard library never
covered forces a user to hand-write a JavaScript host object and register it
with `provide`. That user is now writing and shipping JavaScript, which is the
thing Kite exists to replace. **A language whose extension mechanism is "go and
write the other language" has conceded its own argument on the first day of real
use.** The primitives close that: they are the last JavaScript anyone writes,
and the generated glue is a fixed size no matter how much of the platform a
program touches.

The cost is a name looked up when the program runs rather than fixed when it
compiles, and it is paid twice: a small amount of speed, and a mistyped name
that compiles. §15.4 is what makes the second one survivable.

### 15.3 Everything catches

A host exception must never cross the boundary raw. Every primitive that can
fail returns a value and an error:

```kite
let (node, err) = js.call1(document, "querySelector", js.of_str("#form"))
check err
```

The taint analysis ([§7.3](#73-correlated-results-and-taint-analysis)) then makes
the check mandatory. This is not defensive style; it is the difference between a
mistyped method name failing one call and a thrown exception unwinding through
the Wasm frames and taking every running task with it.

It also removes JavaScript's most common class of bug by construction. Reading a
property that is not there yields `undefined`, and `undefined` becoming `0` or
`NaN` somewhere later is untraceable. `as_num` returns an error, and the error
must be tested before the number can be used.

**Numbers cross as `f64`.** JavaScript numbers *are* `f64`, and an `int` is an
i64, so every crossing would otherwise allocate a BigInt. The safe-integer check
happens on the Kite side, where the failure is a value.

**Absence is `Option`.** A host call that may find nothing returns `Option<T>`. There is
no tolerated zero handle and no null object anywhere in the boundary — a
convention that returns something usable-looking for "not found" is the zero
zero value [§5.3](#53-struct-literals) exists to prevent, wearing a different
hat.

### 15.4 The hygiene boundary

`JsValue` is untyped. If it reaches application code, the type system has stopped
helping and Kite is JavaScript with more syntax. Two rules keep it in:

**Wrap it in an opaque struct.** A `pub struct` with unmarked fields
([§4.2](#42-visibility)) can be held and passed but not read, built or
destructured. So `Element` outside `std/dom` is a real closed type, and no
ordinary code can reach the value inside it.

**Provide exactly one door out.** `dom.raw(e)` and `dom.wrap(v)`, greppable and
documented. Sealing a wrapper completely sounds safer and is not: the user who
needs one method the library never wrapped cannot reach their own element, and
what they do instead is rebuild a parallel untyped world beside the typed one.
One marked escape is a boundary that holds; a wall is a boundary that gets
climbed.

`std/js` is a separate module so that importing it is visible in a file's first
three lines. It is the floor below the typed world, and it carries the same
cultural marking Rust gives `unsafe` — normal inside a module whose job is
wrapping, a smell in an application's public interface.

### 15.5 What is admitted

Two things are true about this design and are recorded rather than defended.

**A mistyped name compiles.** `extern` did not have this problem. Three things
reduce it: the typed layer is written once and covered by tests, so a typo lives
in one place; users call the typed function and never write the string; and the
long tail can be **generated** from the browser's own interface definitions,
where the names come from the specification and cannot be mistyped at all. That
generator is a build step, and a build step is where code generation belongs.
It is not the first step: the definitions carry
overloads, which Kite has no way to express, and unions, which each need a
decision.

**This is reflection over the host.** Not over Kite — no Kite metadata is
retained, so dead-code elimination stays sound and the reason Kite has no
reflection over its own values is untouched. But the spirit of "no second language inside the
language" is genuinely strained at the raw layer, where a typo is a runtime
mystery rather than a diagnostic. The honest resolution is the one above: a
bounded dynamic floor, marked, fenced at public boundaries, with generation as
the long-term exit.

---

## 16. Diagnostics

Error message quality is a language design constraint in Kite, not a
post-implementation concern. Several decisions in this specification — nominal
traits over structural, explicit `dyn`, no implicit conversions, no overloading —
were made because they let the compiler produce a message that names one cause
and one fix.

Every diagnostic carries a stable code (`E0301`), a primary span, secondary spans
explaining *why*, and where possible a machine-applicable fix.

```
error[E0114]: cannot assign to immutable binding `total`
   ┌─ cart.kite:14:5
   │
 9 │     let total = 0
   │         ----- declared immutable here
   ⋮
14 │     total = total + item.price
   │     ^^^^^ cannot assign
   │
help: make the binding mutable
   │
 9 │     var total = 0
   │     ~~~
```

Requirements on the implementation:

- **One error per cause.** A single missing brace must not produce forty errors.
  The parser recovers at statement and declaration boundaries.
- **Type errors name the source of the expectation**, not just the mismatch —
  the parameter or return type that created the constraint gets a secondary span.
- **`--explain E0301`** prints the full rationale for the rule.
- **`kitec fix`** applies every machine-applicable suggestion.
- **Source maps** are emitted for the Wasm target so browser stack traces name
  `.kite` files and lines.

---

## Appendix A — A complete program

**This compiles.** `crates/kite-driver/tests/spec.rs` extracts it from this
document and checks it on every run, which is the only way a specification stops
being able to lie about the language it describes. It could, until recently: the
program that stood here used `use std/io`, `impl Error for LoadError` and
`json.decode<[Task]>` — three things that did not exist — and nothing noticed,
because nothing was checking.

Two were the appendix being wrong about the language and were corrected. The
third was the language being unfinished, and it was built rather than explained
away: `LoadError` below is a real concrete error type, and the test is what says
so.

```kite
use std/fs
use std/json
use std/http

@derive(Decode)
pub struct Task {
    pub id:    int
    pub title: str
    pub var done: bool
}

impl Display for Task {
    fn show(self) -> str {
        let mark = if self.done { "x" } else { " " }
        return "[\(mark)] \(self.id). \(self.title)"
    }
}

// `Absent` rather than `Missing`: a pattern names a variant without its enum,
// so two enums in scope may not share a variant name — and `std/fs` already has
// a `Missing`. The rule is in §9.3, and this is what it looks like in practice.
pub enum LoadError {
    Absent(path: str)
    Malformed(path: str, detail: str)
}

impl Error for LoadError {
    fn message(self) -> str {
        return match self {
            Absent(path)            => "no task file at \(path)",
            Malformed(path, detail) => "\(path) is not valid task JSON: \(detail)",
        }
    }
}

pub fn load(path: str) -> ([Task], error) {
    if !fs.exists(path) {
        return _, LoadError.Absent(path: path)
    }

    let (bytes, err) = fs.read(path)
    check errors.wrap(err, "reading \(path)")

    let (doc, perr) = json.parse(bytes)
    if perr != nil {
        return _, LoadError.Malformed(path: path, detail: perr.message())
    }

    var tasks: [Task] = []
    for item in json.items(doc) {
        let (task, derr) = Task.decode(item)
        check errors.wrap(derr, "\(path) has a task that is not one")
        tasks.push(task)
    }
    return tasks, nil
}

pub async fn sync(tasks: [Task], endpoint: str) -> (int, error) {
    var uploaded = 0
    let pending = filter(tasks, |t: Task| !t.done)

    for task in pending {
        let (res, err) = await http.post(endpoint, "\(task.id)")
        check errors.wrap(err, "uploading task \(task.id)")

        if res.status != 200 {
            return _, errors.new("server returned \(res.status)")
        }
        uploaded = uploaded + 1
    }

    return uploaded, nil
}

pub async fn main() {
    let (tasks, err) = load("tasks.json")
    if err != nil {
        io.error("could not load tasks: \(err.message())")
        return
    }

    for task in tasks {
        io.print(task.show())
    }

    let (count, serr) = await sync(tasks, "https://api.example.com/tasks")
    if serr != nil {
        io.error("sync failed: \(serr.message())")
        return
    }
    io.print("synced \(count) tasks")
}
```

## Appendix B — Keyword census

| Keyword | Purpose |
|---|---|
| `async` `await` | Concurrency |
| `as` | Explicit conversion |
| `break` `continue` `for` `if` `else` `match` `return` | Control flow |
| `check` | Error propagation |
| `defer` | Scope-exit release |
| `enum` `struct` `trait` `type` | Type declaration |
| `false` `true` `nil` | Literals |
| `fn` | Function declaration |
| `impl` | Method and trait implementation |
| `in` | Iteration |
| `let` `var` | Bindings |
| `pub` | Visibility |
| `self` | Receiver |
| `use` | Import |

**Total: 27.** Go has 25, C has 32, Rust has 39, Swift has over 90.

The count is asserted by a test: `kinds::KEYWORDS.len() == TokenKind::KEYWORD_COUNT`
in `crates/kite-lexer`. It cannot drift from the implementation without failing
the build.
