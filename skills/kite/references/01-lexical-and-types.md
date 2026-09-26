# Kite: lexical structure, literals, and the type system

Everything below was checked against `target/release/kitec`. Where the
specification or `docs/05-grammar.ebnf` says otherwise, the compiler wins and
the disagreement is called out inline.

## Surprises, first

| You would assume | Kite |
|---|---|
| `;` ends a statement | `;` is not a token at all — `E0002 invalid character` |
| A continued line may start with `\|\|` | It parses as a zero-argument closure and is discarded — `E0117` |
| …or with `-` | It is a fresh statement negating a number. **No diagnostic**, silently wrong answer |
| Struct fields are comma-separated | They are **newline**-separated; a `,` after a field is `E0100` (reported once; the fields are kept) |
| `let f: float = 3` | `E0200`. No implicit numeric conversion, not even for literals. Write `3.0` |
| `a & b == c` is `a & (b == c)` | Bitwise binds **tighter** than comparison, so it is `(a & b) == c` |
| `a < b < c` compiles | `E0100`. Comparison is non-associative |
| `'a'` is a char | There is no `char` type. `'a'` lexes, then `E0200` |
| `42i64`, `1.0f32` | `E0004`. One integer type, one float, no suffixes |
| `[3]int` is a fixed array | No fixed-length arrays. Only `[T]` slices |
| `s[0]` indexes a string | `E0200`. Strings are not indexable; `s[1..4]` slices, `s.code_at(1)` reads |
| `for k in someMap` gives keys | A map needs a **pair** binding, `for (k, v) in m`. One binding is `E0200` |
| `let r = 0..n` | `E0200`. A range is syntax, not a value; there is no `Range` type |
| `if x != nil && x.field` | Narrowing does **not** cross `&&` — `E0200`. Nest the `if` |
| `x as str`, `flag as int` | `E0212`. `as` converts `int` ↔ `float` and nothing else |
| Shadowing in the same block | `E0112 duplicate definition`. Only a nested scope may shadow |
| `/* … */` | `E0005`. Only `//`, `///`, `//!` |
| `if 1 { }` | `E0202`. No truthiness; the condition must be `bool` |

---

## 1. Source encoding and identifiers

Source is UTF-8, extension `.kite`. Identifiers follow UAX #31 `XID_Start` /
`XID_Continue`, plus `_` in either position, so non-Latin identifiers work.
Source is normalised to **NFC** before comparison, so `café` written with a
precomposed U+00E9 and `café` written with `e` + U+0301 are the same identifier.

```kite
struct 座標 {
    x: int
}

fn main() {
    let นาม = "thai"
    let café = 1
    io.print(นาม)
    io.print(café)
    io.print(座標{ x: 3 }.x)
}
```

## 2. Comments

Three forms, all line comments. There is no block comment.

```kite
//! Module documentation — the file's own text. Goes at the top.

/// Documentation for the declaration that follows. Markdown is permitted.
///
/// ```kite
/// assert(double(2) == 4, "doubling works")
/// ```
pub fn double(n: int) -> int {
    // An ordinary line comment.
    return n * 2
}
```

```kite fails
/* block comments do not exist */ //~ E0005
fn main() {
    io.print(1)
}
```

### Doc fences are compiled and run

A ` ```kite ` fence inside a `///` comment is a **test**. `kitec test`
extracts it, appends it to the module it was written in — so everything the
comment documents is in scope with no import — and runs it alongside the file's
test functions. Two shapes, and they do not mix:

- A fence of **statements** is wrapped in a function and executed. `kitec test`
  reports it as `(doc example)`.
- A fence of **declarations** (a `struct`, an `fn`) lands at file scope and is
  only type-checked. Reported as `(doc example, compiled)`.
- A fence that starts with a declaration and then has a bare statement is a
  parse error (`expected a declaration`). Put the statements inside an `fn`.

` ```kite ignore ` marks an illustration and is not compiled. A fence tagged
anything else is prose. This is the same convention `std/*.kite` uses.

## 3. Semicolon insertion

**This is the rule models get wrong most often.** Statements are
newline-terminated. Semicolons are never written and `;` is not a token:

```kite fails
fn main() {
    let x = 1; //~ E0002
    io.print(x)
}
```

A line continues onto the next **only when it ends in** one of these tokens:

```
(  [  {          open delimiters
,  :  ->  =>     separators
+  -  *  /  %    arithmetic
&  &&  |  ||  ^  <<
=  ==  !=  <  <=  >=
+=  -=  *=  /=  %=
!
.  ..  ..=
as  in  check  await  else
```

Three things are **not** on that list and routinely surprise people:

- `>` and `>>`. They read as operators but far more often close a type argument
  list, and a field declared `pub width: Option<float>` must end, so the lexer
  keeps the line break after them. The parser then skips it after a `>` it has
  read as a comparison: `a >` at the end of a line continues exactly as `a <`
  does, and the `>` of `Option<float>` still ends its line.
- `return`. It can end a statement on its own, so a line ending in it ends
  there, and the line after a bare `return` is dead code (`E0116`), not its
  value.
- `)`, `]`, `}`. A closing delimiter ends the statement.
- Newlines inside `(` or `[` are *always* insignificant, so argument lists and
  slice literals wrap freely. Inside `{` they are significant, because blocks
  need statement separation.

Independently, a newline is *not* a separator when the **next** line begins with
`.` (method chaining), `else`, `)` or `]`.

```kite
fn add(a: int, b: int) -> int { return a + b }

fn main() {
    let c = 50

    // Operator at the END of the line it continues.
    let ok = (c >= 48 && c <= 57) ||
        (c >= 65 && c <= 90)
    io.print(ok)

    // Open delimiters and commas continue too.
    let total = add(
        1,
        2,
    )
    let xs = [
        1,
        2,
    ]
    io.print(total + xs.len())

    // Leading `.` continues a method chain.
    let n = "abcd"
        .trim()
        .len()
    io.print(n)

    // `}` newline `else` is one statement.
    if n > 3 {
        io.print("long")
    }
    else {
        io.print("short")
    }
}
```

The failure mode the language goes out of its way to catch: a line **opening**
with `||`. Where a value is expected, `||` is a closure with no parameters, so
the line parses, means nothing, and is thrown away. The previous line was
already a complete statement and its answer is silently wrong.

```kite fails
fn main() {
    let c = 50
    let ok = (c >= 48 && c <= 57)
        || (c >= 65 && c <= 90) //~ E0117
    io.print(ok)
}
```

`&&` cannot do this — it may not begin an expression, so a leading `&&` is a
plain `E0100 expected an expression`. So is a leading `*`, `+`, `==`, or any
other operator that is **only** infix.

`-` is the exception, and it is worse than `||`: it is also a prefix operator,
so a leading `-` starts a fresh statement that is a negated number, computes
nothing, and gets **no diagnostic at all**. E0117 is raised by `inert_closure`
in `crates/kite-types`, which fires on a closure expression statement and
nothing else, so it never sees this. The line below checks clean and prints
`10`.

```kite
fn main() {
    let n = 10
        - 2
    io.print(n)     // 10, not 8 — the `- 2` is a separate, inert statement
}
```

Note the spec's §2.5 phrasing ("the line ends in an operator") leaves out the
leading-`.` / leading-`else` cases, which continue without any trailing
operator. The list above is what `TokenKind::continues_line` and
`should_separate` implement, with the parser's skip after `>` and `>>`.

## 4. Keywords

Twenty-seven, complete, and the count is asserted by a test in
`crates/kite-lexer`:

```
async    await    as       break    check    continue
defer    else     enum     false    fn       for
if       impl     in       let      match    nil
pub      return   self     struct   trait    true
type     use      var
```

`dyn` and `error` are **contextual**, not reserved — both are usable as ordinary
identifiers. `dyn` is special only before a type path in type position; `error`
is a built-in type name a binding may shadow.

```kite
fn main() {
    let error = 5
    let dyn = 6
    io.print(error + dyn)
}
```

`Self` is contextual: inside a trait it is the implementing type, and inside an
`impl` block the type the block is for — `fn me(self) -> Self`. Anywhere else it
is an ordinary (unknown) type name.

## 5. Literals

### Numbers

```kite
fn main() {
    let dec = 1_000_000        // underscores between digits
    let hex = 0xFF
    let oct = 0o755
    let bin = 0b1010_1101
    io.print(dec + hex + oct + bin)

    let pi   = 3.14
    let big  = 1e10            // float; `1E10` too
    let tiny = 1.5e-3
    let sep  = 1_0.0_1
    io.print(pi + big + tiny + sep)
}
```

Details the grammar does not make obvious:

- A float needs digits on **both** sides of the point, but the two halves fail
  differently. `.5` is `E0100 expected an expression`; `1.` lexes as the integer
  `1` followed by a field access and fails as `E0200 int has no fields`. Write
  `1.0` and `0.5`.
- A `_` separator sits between two digits, in every radix and every part of a
  literal. `1000_`, `0xFF_`, `0x_FF`, `1__0` and `1_.5` are all `E0004`.
- An integer literal above `9223372036854775807` is `E0004 integer literal is
  out of range` at compile time, and a float literal too large to be finite
  (`1e999`) is `E0004 float literal is out of range`.
- After a `.`, a number is a tuple index: `t.0.1` is the second element of the
  first, not `t` followed by the float `0.1`.
- No type suffixes:

```kite fails
fn main() {
    let x = 42i32 //~ E0004
    io.print(x)
}
```

### Strings

`str` literals use `"`. A `"` string may not span lines. Escapes are exactly
`\n \t \r \0 \\ \" \'` and `\u{…}`; anything else is `E0003`.

Block strings use `"""` and strip the leading indentation of the **closing**
delimiter from every line. There is no trailing newline. `docs/05-grammar.ebnf`
writes `MultiString = '"""' NEWLINE …`, but the lexer does not require the
newline: `"""one line"""` is a legal one-line block string, and text left on
the opening line is kept verbatim.

```kite
fn main() {
    io.print("nul[\0] quote['] backslash[\\] unicode[\u{4E2D}]")
    io.print("tab\tsplit\nline")

    let block = """
        indented block
          deeper
        """
    io.print("[\(block)]")
}
```

**Holes do not change the dedent.** A block string with `\(…)` in it loses the
same whitespace as one without: the parser cuts the literal text around each
hole, leaving the indentation out, so the pieces joined are the dedented text.
(Compilers through 0.1.9 switched the dedent off at the first hole.)

```kite
fn main() {
    // No hole: the closing delimiter's indentation comes off every line.
    let clean = """
        alpha
        beta
        """
    io.print("[\(clean)]")     // [alpha\nbeta]

    // A hole, and the same thing happens.
    let n = 5
    let held = """
        alpha \(n)
        beta
        """
    io.print("[\(held)]")      // [alpha 5\nbeta]
}
```

Build the text with `+`, or interpolate the finished block into a `"` string,
when a block string needs a hole and its indentation removed.

### Interpolation

`\(expr)` — the hole is an ordinary expression, evaluated where it stands, and
rendered by `Display.show`. `int`, `float`, `bool` and `str` render themselves;
**everything else needs an `impl Display`**, including `[int]`, `Option<int>`
and tuples.

```kite
struct Point {
    x: int
    y: int
}

impl Display for Point {
    fn show(self) -> str {
        return "(\(self.x), \(self.y))"
    }
}

fn main() {
    let n = 3
    io.print("p = \(Point{x: 1, y: 2})")
    // A hole is an expression, so this is a pluraliser with no new syntax.
    io.print("\(n) item\(if n > 1 { "s" } else { "" })")
}
```

```kite fails
struct Point {
    x: int
    y: int
}

fn main() {
    let p = Point{ x: 1, y: 2 }
    io.print("point is \(p)") //~ E0207
}
```

`Display` is deliberately not derivable — `@derive` covers `Debug`, `Hash`,
`Encode`, `Decode` only.

### Characters — there are none

```kite fails
fn main() {
    let c = 'a' //~ E0200
    io.print(c)
}
```

A character is an `int` code point; `s.code_at(i)` produces one.

## 6. Operators and precedence

Loosest to tightest, as `crates/kite-parser/src/prec.rs` implements it:

```
..  ..=            range (non-associative, and the loosest thing there is)
||
&&
==  !=  <  <=  >  >=   (non-associative — at most one per expression)
&  ^  |            (one level, left to right)
<<  >>
+  -
*  /  %
as
-  !  await        prefix
.  ()  []          postfix
```

Two deliberate departures from C, both verified:

- **Bitwise binds tighter than comparison.** `6 & 3 == 2` is `(6 & 3) == 2`,
  i.e. `true`.
- **Comparison does not chain.**

`&`, `^` and `|` share one level, as §5.1 and `docs/05-grammar.ebnf` say, so they
group in the order they are written: `1 | 2 ^ 3` is `(1 | 2) ^ 3`, i.e. `0`.
(Compilers through 0.1.9 layered them the way C does.)

```kite
fn main() {
    io.print(6 & 3 == 2)      // true  — (6 & 3) == 2
    io.print(1 | 2 ^ 3)       // 0     — (1 | 2) ^ 3
    io.print(1 << 2 + 1)      // 8     — 1 << (2 + 1)
    io.print(-7 as float / 2.0)  // -3.5 — (-7 as float) / 2.0
}
```

```kite fails
fn main() {
    let a = 1
    let b = 2
    let c = 3
    let bad = a < b < c //~ E0100
    io.print(bad)
}
```

`await` is a prefix operator tighter than any binary one: `await f() + 1` is
`(await f()) + 1`. `?` is not a token — no optional chaining, no coalescing, no
ternary.

## 7. Primitives

| Type | Notes |
|---|---|
| `bool` | `true` / `false`. No truthiness anywhere |
| `int` | 64-bit signed. The only integer type |
| `float` | 64-bit IEEE-754. The only float |
| `str` | Immutable sequence of **Unicode scalar values** |
| `error` | A failure and its message (`e.message()`) |
| `JsValue` | Opaque host object, web only; `==` on one is `E0201` |

`int` has **no methods** at all (`(1).abs()` is `E0205`); the prelude has
`abs`, `min`, `max`, `clamp`, and `math` has the rest.

### No implicit conversion, ever

```kite fails
fn main() {
    let f: float = 3 //~ E0200
    io.print(f)
}
```

```kite fails
fn main() {
    let n = 3
    io.print(n + 1.5) //~ E0201
}
```

```kite
fn main() {
    let n = 3
    let f: float = 3.0
    io.print(n as float + 1.5)
    io.print(f)
}
```

### Arithmetic behaviour

- Integer division truncates toward zero: `7 / 2 == 3`, `-7 / 2 == -3`.
  `%` takes the sign of the dividend: `-7 % 2 == -1`.
- **Overflow traps in debug builds and wraps in release builds.** `int_max + 1`
  aborts under `kitec run` and yields `int_min` under `kitec run --release`.
  Traps are not catchable — Kite has no `recover`. Use `math.wrapping_add` /
  `math.checked_add` when the behaviour must be the same in both.
- Integer divide-by-zero traps. Float divide-by-zero gives `inf`.

### `str`

One character per element, everywhere: `"😀".len()` is `1`, and indexing and
length count Unicode scalar values, never bytes and never UTF-16 code units.
A `str` has exactly **five** methods; everything else (`split`, `join`,
`starts_with`, `replace`, `words`, `parse_int`, case folding) is a prelude
function written in Kite on top of them.

| Method | Meaning |
|---|---|
| `s.len()` | Characters |
| `s.slice(from, to)` | Characters `from..to`, **clamped**, never trapping |
| `s.index_of(needle)` | Character index, or `-1` |
| `s.trim()` | Leading and trailing whitespace removed |
| `s.code_at(i)` | Code point at character `i`, or `-1` past the end |

```kite
fn main() {
    let s = "  héllo wörld  "
    io.print(s.len())              // 15
    io.print(s.trim().len())       // 11
    io.print(s.index_of("wörld"))  // 8
    io.print(s.slice(2, 7))        // héllo
    io.print(s.slice(-5, 100))     // clamped: the whole string
    io.print(s.code_at(2))         // 104
    io.print(s.code_at(999))       // -1
    io.print("😀".len())            // 1

    // `+` concatenates; `<` and `==` compare by scalar value.
    io.print("a" + "b")
    io.print("a" < "b")

    // Slicing syntax works, indexing does not.
    io.print(s.trim()[0..5])
}
```

```kite fails
fn main() {
    let s = "hello"
    io.print(s[0]) //~ E0200
}
```

## 8. Composite types

```
[T]             slice — a copy-on-write sequence
{K: V}          map — insertion-ordered hash map
(A, B, C)       tuple — fields are .0, .1, .2
Option<T>       optional
fn(A, B) -> C   function type
dyn Trait       trait object
```

**There is no fixed-length array.** `docs/05-grammar.ebnf` still lists
`"[" IntLit "]" Type`; the compiler rejects it.

```kite fails
fn main() {
    let xs: [3]int = [1, 2, 3] //~ E0100
    io.print(xs.len())
}
```

Method surfaces are tiny and the compiler tells you the whole list on a miss:

- slice: `len`, `get`, `push`. Everything else is a prelude *function* taking
  one — `map(xs, f)`, `filter(xs, test)`, `sorted(xs, less)`, `enumerate(xs)`,
  `fold`, `zip`, `unique`, `flatten`, `chunked`, …
- map: `len`, `keys`, `values`. Read with `m[key]` (yields `Option<V>`), write
  with `m[key] = value`.
- tuple: positional fields only.

Slices are **values**: assigning one and pushing to the copy leaves the original
alone. Reading a missing map key gives `nil`, not a zero value.

```kite
fn main() {
    var ys = [1, 2, 3]
    var zs = ys
    zs.push(4)
    io.print(ys.len())     // 3 — copy-on-write
    io.print(zs.len())     // 4
    io.print(ys[1..3].len())

    let t = (1, "two", 3.0)
    io.print(t.0)
    io.print(t.2)
    let (a, b, c) = t
    io.print("\(a) \(b) \(c)")
}
```

### Nested slices and slices in fields change in place

`grid[i][j] = v`, `grid[i].push(x)`, `b.cells.push(x)`, `b.counts["k"] = 1`,
`ms[0].remove(k)` and the compound forms (`grid[i][j] += 1`) all work, to any
depth. The compiler copies each level out, changes the innermost and writes
every level back — what you would otherwise write by hand — with the indices
and the right-hand side evaluated once, first. So a copy of the outer slice
taken before the write keeps its old contents: slices are values.

```kite
struct Board {
    var cells: [int]
}

fn main() {
    var grid = [[1, 2], [3, 4]]
    let before = grid
    grid[0][1] = 9
    grid[1].push(5)
    grid[1][0] += 10
    var b = Board{ cells: [] }
    b.cells.push(7)
    b.cells[0] *= 2
    io.print("\(grid[0][1]) \(grid[1][0]) \(grid[1].len()) \(before[0][1])") // 9 13 3 2
    io.print(b.cells[0]) // 14
}
```

Every level must allow the write: the root binding is a `var` (a `let` is
`E0114`), a field on the way is declared `var` (else `E0114`), and a struct is
reached through a binding that may change it. A call's result (held by
nothing) and a tuple element (fixed once built) are `E0200`: bind the value to
a `var` first.

```kite fails
fn rows() -> [[int]] {
    return [[1]]
}

fn main() {
    let grid = [[1, 2]]
    grid[0][0] = 5 //~ E0114
    rows()[0].push(2) //~ E0200
}
```

A loop that pushes thousands of times through a field copies the field's slice
on each push; for a hot loop, build the slice in a `var` local and assign it to
the field once.

### An empty `[]` or `{}` has nothing to infer from

Both are `E0204`, and the fix is an annotation on the binding — inference never
looks ahead to the first `push` or the first key written.

```kite fails
fn main() {
    let m = {} //~ E0204
    io.print(m.len())
}
```

> ``error[E0204]: cannot infer the map's types`` — "an empty map has no entries
> to infer from", and the note spells the fix: ``let m: {str: int} = {}``. A bare
> `[]` is the same code with "cannot infer the element type".

```kite
fn main() {
    var m: {str: int} = {}
    m["a"] = 1
    var xs: [int] = []
    xs.push(1)
    io.print(m.len() + xs.len())
}
```

### Maps iterate in pairs

A map **is** iterable, in insertion order, but only through a two-element
**pair binding**. `for x in m` — one binding — is
`E0200 cannot iterate a {str: int}`, because a single binding takes a range or
a slice. This is the shape to reach for:

```kite
fn main() {
    var m = {"b": 2, "a": 1}
    m["c"] = 3

    // A pair binding iterates the map itself.
    for (k, v) in m {
        io.print("\(k)=\(v)")          // b=2, a=1, c=3
    }

    // `keys()` and `values()` are slices, so they take one binding.
    for k in m.keys() {
        io.print(k)
    }
    for v in m.values() {
        io.print(v)
    }
}
```

```kite fails
fn main() {
    var m = {"a": 1}
    for k in m { //~ E0200
        io.print(k)
    }
}
```

A pair binding also destructures a slice whose element is a two-element tuple,
which is exactly what `enumerate` and `zip` answer with — so the pair form is
not a map special case:

```kite
fn main() {
    for (i, x) in enumerate(["a", "b"]) {
        io.print("\(i):\(x)")          // 0:a, 1:b
    }
    for (a, b) in zip([1, 2], ["x", "y"]) {
        io.print("\(a)\(b)")           // 1x, 2y
    }
}
```

Anything else under a pair binding is `E0200 cannot iterate a [int] in pairs`,
and a binding of the wrong width is `E0200 expected 2 bindings, found 3`.
`for (k, v) in m` is lowered to a loop over `m.keys()` with a lookup per key,
which is why the order is the insertion order `keys()` already promised.

### Dropping a key is `remove`, and assigning `nil` is not it

`m.remove(key)` takes the entry out and shifts the ones after it down, so
insertion order still means what it says. A key that is not there is not a
mistake — dropping state for a row that never had any is the ordinary case —
so a miss changes nothing and reports nothing.

```kite
fn main() {
    var m = {"a": 1, "b": 2, "c": 3}
    m.remove("b")
    m.remove("gone")               // absent: nothing happens
    io.print("\(m.len())")          // 2
    io.print("\(m["b"] == nil)")    // true
}
```

The receiver has to be somewhere the change can be kept, exactly as
`xs.push(v)`'s does: maps are copy-on-write values, so the write lands on what
holds the map. A `var` binding, a `var` field (`b.counts.remove(k)`) and an
element of a `var` slice (`ms[0].remove(k)`) all are; a `let` is `E0114`.

**Assigning `nil` is not the same thing.** On a `{str: int}` it is
`E0200 expected int, found nil`; on a `{str: Option<int>}` it is accepted and
leaves the key **in place** with a `nil` value — `len()` is unchanged and the
key still comes out of `keys()` and out of a pair loop.

The whole map surface is what the `E0205` note lists: "a map has: len, keys,
values, remove; read with `m[key]`, which yields an optional, and write with
`m[key] = value`".

### A range is not a value

```kite fails
fn main() {
    let r = 0..3 //~ E0200
    io.print(r)
}
```

`a..b` is syntax for a `for` header and for a slice/`str` window. There is no
`Range` type to bind, pass or return; carry the two ends instead. A window may
leave out either end — `xs[2..]`, `xs[..2]`, `xs[..]`, and `s[6..]` on a `str`
— which fills in `0` or the largest `int`, and the clamp does the rest. Only an
index may: `0..` on its own is `E0100`.

```kite
fn main() {
    let xs = [1, 2, 3, 4]
    io.print(xs[2..].len())     // 2
    io.print(xs[..1].len())     // 1
    io.print(xs[..].len())      // 4
    io.print("hello world"[6..])  // world
}
```

## 9. Optionals, `nil`, and narrowing

`Option<T>` is the only place `nil` may appear other than the `error` slot.
There is no null reference. A `T` is assignable to an `Option<T>` implicitly;
the reverse requires handling the `nil`.

```kite fails
fn main() {
    let n: int = nil //~ E0200
    io.print(n)
}
```

**Narrowing** unwraps an optional on the branch where it cannot be nil: the
`else` of `x == nil`, the `then` of `x != nil`, the code after an early return
guarded by `x == nil`, and a `match` arm once an earlier arm covered `nil`.

```kite
struct User {
    name: str
}

fn find(id: int) -> Option<User> {
    if id == 1 {
        return User{ name: "ada" }
    }
    return nil
}

fn main() {
    // if-expression, one line
    let u = find(1)
    io.print(if u == nil { "anon" } else { u.name })

    // then-branch of `!= nil`
    if u != nil {
        io.print(u.name)
    }

    // early-return guard narrows the rest of the function
    let v = find(1)
    if v == nil {
        io.print("missing")
        return
    }
    io.print(v.name)

    // match: the second arm binds the unwrapped User
    match find(2) {
        nil => io.print("none"),
        found => io.print(found.name),
    }

    io.print(or_else(find(9), User{ name: "fallback" }).name)
}
```

**Narrowing does not cross `&&`.** Both operands of a condition are checked
against the unnarrowed type, so this is an error, and nesting is the fix:

```kite fails
struct User {
    name: str
}

fn find(id: int) -> Option<User> {
    if id == 1 {
        return User{ name: "ada" }
    }
    return nil
}

fn main() {
    let u = find(1)
    if u != nil && u.name == "ada" { //~ E0200
        io.print("both")
    }
}
```

An `Option<T>` also cannot be compared to a bare `T` — `xs.get(0) == 1` is
`E0201`. Compare against `nil`, or use `or_else`.

## 10. Conditions are `bool`

```kite fails
fn main() {
    if 1 { //~ E0202
        io.print("yes")
    }
}
```

## 11. Type declarations and aliases

Struct and enum members are **newline-separated**. A comma between struct
fields is a parse error.

```kite
type UserId = int                   // alias
type Celsius = float
type Prices = {str: int}            // any type, not just a primitive
type Pair = (int, int)

pub struct Point {
    pub x: float
    pub y: float
    var label: str                  // `var` field — assignable through a `var self`
}

pub enum Status {
    Active
    Suspended(reason: str)          // named payload
    Deleted(int, str)               // positional payload
}

fn describe(s: Status) -> str {
    return match s {
        Active => "active",
        Suspended(reason) => "suspended: \(reason)",
        Deleted(at, by) => "deleted at \(at) by \(by)",
    }
}

fn main() {
    let id: UserId = 7
    io.print(id + 1)                // interchangeable with int
    let p = Point{ x: 1.0, y: 2.0, label: "origin" }
    io.print(p.label)
    io.print(describe(Suspended(reason: "spam")))
    io.print(describe(Deleted(3, "root")))
}
```

An alias is **replaced** by the type it names before anything else is checked,
so `UserId` and `int` are the same type everywhere — an alias buys no safety.
Aliases may name each other and may be declared in any order. Two forms are
rejected, both `E0214`:

```kite fails
type A = B
type B = A //~ E0214

fn main() {
    let x: A = 1
    io.print(x)
}
```

```kite fails
type Pair<T> = (T, T) //~ E0214

fn main() {
    io.print(1)
}
```

A struct literal may not appear in the condition position of `if` / `for` /
`match` without parentheses — the same rule Go and Rust have. Write
`if (Point{x: 1.0, y: 2.0}).is_origin() { … }`.

Structural `==` is defined on structs and enums (field-by-field), so
`P{x:1} == P{x:1}` is `true` with no derive. It is *not* defined on functions,
`dyn` values, or `JsValue`.

## 12. `as`

`as` converts between `int` and `float`. **Nothing else.** No `bool as int`, no
`int as str`, no pointer-ish casts, no widening or narrowing between sizes
(there are no other sizes).

- `float as int` **truncates toward zero** and emits a warning, `E0212`
  *this cast discards the fractional part* — a warning, so the program still
  compiles.
- A float too large for an `int` **saturates** to `int` max/min rather than
  trapping, and `NaN as int` is `0`.
- `int as float` is exact up to 2^53 and rounds beyond it, as IEEE-754 requires.

```kite
type Celsius = float

fn main() {
    let c: Celsius = 21.5
    io.print(c as int)          // 21, with an E0212 warning
    io.print(-3.99 as int)      // -3
    io.print(1e30 as int)       // 9223372036854775807 — saturated
    io.print(5 as float)        // 5.0
    io.print(5 as float as int) // 5
}
```

```kite fails
fn main() {
    io.print(true as int) //~ E0212
}
```

```kite fails
fn main() {
    io.print(65 as str) //~ E0212
}
```

## 13. Binding rules that bite in the lexer/type layer

Same-scope redefinition is an error; a nested scope may shadow.

```kite fails
fn main() {
    let x = 1
    let x = 2 //~ E0112
    io.print(x)
}
```

A `let` may be declared without an initialiser, but the compiler must prove
exactly one assignment happens on every path before the first read.

```kite
fn main() {
    let x: int
    x = 4
    io.print(x)
}
```

```kite fails
fn main() {
    let x: int
    io.print(x) //~ E0110
}
```

## 14. How floats print

`io.print` and `"\(x)"` write a float the same way on every backend. A whole
number below `1e21` gets all its digits and `.0`: `3.0`, `1000000.0`,
`10000000000.0` for `1e10`. Anything else is the shortest form that reads back,
plain from `1e-7` up to `1e21` and in exponent form outside: `0.3333333333333333`,
`0.30000000000000004` for `0.1 + 0.2`, `0.000001`, `1e-7`, `1e+21`. Overflow is
`inf` or `-inf`, `0.0 / 0.0` is `NaN`, and negative zero is `-0.0`.

## 15. Where the written sources are wrong

The compiler is authoritative. Checked disagreements, all in this file's scope:

1. `docs/05-grammar.ebnf` writes `MultiString = '"""' NEWLINE { AnyChar } '"""'`.
   The newline is not required — `"""one line"""` compiles.
2. `docs/05-grammar.ebnf` has no production for a pair-binding `for`. `ForHeader
   = Binding "in" Expr` covers it only because `Binding` admits a tuple; the
   grammar never says a map is what that iterates.
3. SPECIFICATION.md §2.5 states the continuation rule as "ends in an operator".
   A *leading* `.`, `else`, `)` or `]` also continues the previous line, with no
   trailing operator at all. §2.5 also says `||` is the only continuation that
   is quietly wrong; a leading `-` is quietly wrong too, and unlike `||` it is
   not diagnosed.

SPECIFICATION.md §3.2's "maps iterate in insertion order" is **correct** —
including under `for (k, v) in m`. An earlier draft of this page claimed maps
were not iterable at all; that was wrong.

## Diagnostic codes used above

`kitec --explain E0nnn` prints the rationale for any of them.

| Code | Meaning |
|---|---|
| `E0001` | unterminated string literal |
| `E0002` | invalid character in source |
| `E0003` | invalid escape sequence |
| `E0004` | invalid number literal (includes type suffixes and out-of-range) |
| `E0005` | block comments are not supported |
| `E0100` | unexpected token |
| `E0110` | use of possibly-uninitialised binding |
| `E0112` | duplicate definition |
| `E0117` | statement has no effect (the leading-`\|\|` trap) |
| `E0200` | type mismatch (also: no `char`, no narrowing here, not indexable) |
| `E0201` | cannot apply operator to these types |
| `E0202` | condition must be `bool` |
| `E0204` | unknown type; also an empty `[]` or `{}` with nothing to infer from |
| `E0205` | no such method, function, or callable value |
| `E0207` | value cannot be interpolated (needs `Display`) |
| `E0212` | invalid cast |
| `E0213` | type has no identity (`ptr.same` on a non-cell type) |
| `E0214` | invalid type alias (circular or generic) |
