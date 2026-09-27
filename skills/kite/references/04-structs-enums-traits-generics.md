# Structs, methods, enums, matching, traits, generics

Everything here was checked against `target/release/kitec`. Where SPECIFICATION.md
§8–§11 says otherwise, the compiler wins and the divergence is named.

## Surprises, first

- **`Self` is the implementing type**, in a trait and in an `impl` block alike:
  `fn compare(self, other: Self) -> int`. A trait whose methods mention `Self`
  works through a bound (`T: Compare`) but cannot be a `dyn` (`E0206`).
- **A method may have type parameters of its own**, after its block's:
  `fn map<U>(self, f: fn(T) -> U) -> Box<U>`. The block's come from the
  receiver, the method's from its arguments — still no turbofish.
- **No turbofish, anywhere.** A type argument is inferred from the arguments, or
  from the annotation on the binding being assigned. When neither can say, the
  *type names itself* at the front of the call: `User.decode(doc)`,
  `NotFound.is(err)`, `let s: Stack<int> = Stack.empty()`.
- **`..base` in a struct literal comes FIRST**, not last: `P{ ..old, x: 1 }`.
  `P{ x: 1, ..old }` is a parse error. Opposite of Rust.
- **Struct fields and enum variants are newline-separated.** A comma between them
  is `E0100`. Commas are for *literals* and *patterns*, not declarations.
- **A `match` arm block of more than one statement produces `()`.** Kite has no
  tail expressions. A single-expression block arm does produce a value.
- **`impl Trait for Type` may contain only that trait's methods** — anything else
  is `E0200` and belongs in an inherent `impl`.
- **A `Display` bound makes a type parameter printable**, and so does holding a
  `dyn Display`: `io.print(x)` and `"\(x)"` both work on either.
- **`==` is structural on every value and is not a trait.** There is no `Eq` to
  implement or derive, no `Ord` at all, and no operator overloading.
- **There are no extension methods and no orphan impls.** An `impl` block
  belongs to the module that declares its type — or, for a trait
  implementation, the trait's module. Anything else is `E0406`: a trait of your
  own may be implemented for an imported type, but an imported trait for an
  imported type may not.
- **Unmarked fields and methods are private to their module** (`E0401`), as
  unmarked types and functions are: another module cannot read or write one,
  build the struct with a literal or `..base`, destructure it, or call the
  method.
- **A pattern may be qualified.** `Shape.Point` means the same variant in a
  pattern as in an expression, and is how a payload variant two enums share is
  matched. A qualified name the enum does not have is `E0111` — in a pattern,
  a value or a call alike; the enum's own name where a variant belongs,
  `Shape(r)`, is `E0200`.

---

## 1. Structs

Fields are immutable unless marked `var`. Each field is on its own line.

```kite
struct Rect {
    width: int
    height: int
    var label: str
}

fn main() {
    var r = Rect{ width: 3, height: 4, label: "first" }
    io.print(r.width)
    r.label = "second"
    io.print(r.label)
}
```

Struct values are GC references; assignment copies the reference. Because most
fields are immutable this is indistinguishable from value semantics, and there is
no pointer/value receiver distinction to make.

Two mutability gates, and they are independent: the **field** must be `var`, and
the **binding** must be `var`.

```kite fails
struct Rect {
    width: int
    var label: str
}

fn main() {
    var r = Rect{ width: 1, label: "x" }
    r.width = 2 //~ E0114
}
```

```kite fails
struct Rect {
    var label: str
}

fn main() {
    let r = Rect{ label: "x" }
    r.label = "y" //~ E0114
}
```

### Literals

Every field must be given — there are no defaults and no zero values.

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

A literal is also the only way the name may be used as a value. A bare `P` is
`E0200`, "`P` is a type, not a value … a type name cannot stand alone here" —
and the note suggests a struct literal even when the type is an enum. The name
stands alone nowhere: not as a binding, an argument, or a return value. It is
legal only at the head of a literal, of an associated call (`Rect.square(5)`,
§2), or of a qualified variant (`Shape.Point`, §3).

```kite fails
struct P {
    x: int
}

fn main() {
    let p = P //~ E0200
    io.print(1)
}
```

Field shorthand works when a binding already has the field's name. Update syntax
spreads a base value, and it must be the **first** element.

```kite
struct P {
    x: int
    y: int
    z: int
}

fn main() {
    let x = 1
    let base = P{ x, y: 2, z: 3 }
    let moved = P{ ..base, x: 9 }
    io.print(moved.x)
    io.print(moved.z)
    io.print(base.x)
}
```

```kite fails
struct P {
    x: int
    y: int
}

fn main() {
    let base = P{ x: 1, y: 2 }
    let moved = P{ x: 9, ..base } //~ E0100
    io.print(moved.x)
}
```

Equality is structural and always available — fields compared in declaration
order. Nothing is implemented or derived to get it.

```kite
struct P {
    x: int
    tags: [str]
}

fn main() {
    let a = P{ x: 1, tags: ["t"] }
    let b = P{ x: 1, tags: ["t"] }
    io.print(a == b)
}
```

An empty struct is legal (`struct Unit {}`), and a struct may be generic — see §7.

---

## 2. Methods, receivers, associated functions

An `impl Type { … }` block holds inherent methods. A receiver is `self`
(immutable) or `var self` (may write the type's `var` fields). A function with no
receiver is an **associated function**, called on the type.

```kite
struct Rect {
    width: int
    height: int
    var label: str
}

impl Rect {
    fn area(self) -> int {
        return self.width * self.height
    }

    fn scaled(self, factor: int) -> Rect {
        return Rect{ ..self, width: self.width * factor }
    }

    fn rename(var self, name: str) {
        self.label = name
    }

    fn square(side: int) -> Rect {
        return Rect{ width: side, height: side, label: "square" }
    }
}

fn main() {
    var r = Rect{ width: 3, height: 4, label: "first" }
    io.print(r.area())
    io.print(r.scaled(10).area())
    r.rename("renamed")
    io.print(r.label)
    io.print(Rect.square(5).area())
}
```

Calling a `var self` method needs a `var` binding at the call site — the receiver
mutability propagates outward.

```kite fails
struct Counter {
    var n: int
}

impl Counter {
    fn bump(var self) {
        self.n = self.n + 1
    }
}

fn main() {
    let c = Counter{ n: 0 }
    c.bump() //~ E0114
}
```

Several `impl` blocks for one type are allowed. `Self` names the type the block
is for:

```kite
struct P {
    n: int
}

impl P {
    fn twin(self) -> Self {
        return P{ n: self.n }
    }
}

fn main() {
    io.print(P{ n: 1 }.twin().n)
}
```

A method may introduce type parameters of its own, solved from its arguments:

```kite
struct P {
    n: int
}

impl P {
    fn pick<T>(self, x: T) -> T {
        return x
    }
}

fn main() {
    io.print(P{ n: 1 }.pick(3))
}
```

A type has one method of each name, across all its `impl` blocks — a second
`fn who` is `E0112`:

```kite fails
struct P {
    n: int
}

impl P {
    fn who(self) -> str {
        return "first"
    }
}

impl P {
    fn who(self) -> str { //~ E0112
        return "second"
    }
}

fn main() {
    io.print(P{ n: 1 }.who())
}
```

Enums take `impl` blocks on exactly the same terms:

```kite
enum Shape {
    Circle(radius: int)
    Point
}

impl Shape {
    fn area(self) -> int {
        return match self {
            Circle(r) => 3 * r * r
            Point => 0
        }
    }
}

fn main() {
    io.print(Shape.Circle(radius: 2).area())
}
```

One gap worth knowing, because the compiler is silent about it: an inherent
method shadows a derived one of the same name.

### `pub`, and how far it reaches

`pub` is accepted on a struct, an enum, a trait, a free function, a field and a
method, and it is enforced on all of them: naming an unmarked one from another
module is `E0401`, "`thing.Hidden` is private to module `thing`" — for a type
that includes naming it in a signature or an annotation.

A `pub struct` with an unmarked field is **opaque** outside its module: the
field cannot be read or written, the struct cannot be built with a literal —
`..base` included — and it cannot be taken apart in a pattern. That is what
makes a struct a place to keep an invariant; the module exports functions that
build and read it. An unmarked method or associated function is callable only
inside the module; the methods of a trait implementation are as visible as the
trait.

---

## 3. Enums

Variants are newline-separated. A payload is positional or named; enums are
recursive without any boxing annotation, because every aggregate is already a
reference.

```kite
enum Json2 {
    Null
    Bool(bool)
    Number(float)
    Text(str)
    Array([Json2])
    Object({str: Json2})
}

fn size(j: Json2) -> int {
    match j {
        Array(items) => {
            var total = 1
            for it in items {
                total = total + size(it)
            }
            return total
        }
        _ => {
            return 1
        }
    }
}

fn main() {
    io.print(size(Json2.Array([Json2.Null, Json2.Text("x")])))
}
```

```kite fails
enum E {
    A, B //~ E0100
}

fn main() {
    io.print(1)
}
```

### Construction and name resolution

A variant is written `Enum.Variant(…)` when qualified, in an expression or a
pattern. Unqualified `Variant(…)` in expression position works when exactly one
enum in scope declares the name; with two candidates it is `E0111: cannot find`.

An unqualified pattern for a variant *with a payload* is looked up by name across
every enum in scope rather than against the scrutinee's type. So two enums
declaring `Circle` make `Circle(r)` `E0111` even in a match whose scrutinee is
unambiguous — write `Shape.Circle(r)`. (A *unit* variant does resolve against the
scrutinee, and a shared name is fine.)

```kite
enum Shape {
    Circle(radius: int)
    Rect(int, int)
    Point
}

fn main() {
    let a = Circle(radius: 2)        // unqualified: only one `Circle` in scope
    let b = Shape.Circle(3)          // a named payload also accepts positionally
    let c = Shape.Rect(3, 4)
    io.print(match a { Circle(r) => r, _ => 0 })
    io.print(match b { Circle(radius: r) => r, _ => 0 })
    io.print(match c { Rect(w, h) => w * h, _ => 0 })
    io.print(match Shape.Point { Point => 1, _ => 0 })
}
```

A **named** payload may be filled positionally or by name. A **positional**
payload may only be filled positionally:

```kite fails
enum Shape {
    Rect(int, int)
}

fn main() {
    let a = Shape.Rect(width: 1, height: 2) //~ E0113
    io.print(1)
}
```

```kite fails
enum Shape {
    Circle(radius: int)
    Point
}

enum Hole {
    Circle(radius: int)
    Slot
}

fn f(s: Shape) -> int {
    return match s {
        Circle(r) => r //~ E0111
        Point => 0
    }
}

fn main() {
    io.print(f(Shape.Circle(radius: 2)))
}
```

Qualifying is the way out of that. `Shape.Circle(r)` is looked up on `Shape`,
whatever else shares the name, and is checked exactly like the bare form: it
matches that variant and nothing else, so the arms that follow are still reachable
and still required. This prints `7` then `0`:

```kite
enum Shape {
    Circle(radius: int)
    Point
}

enum Hole {
    Circle(radius: int)
    Slot
}

fn f(s: Shape) -> int {
    return match s {
        Shape.Circle(r) => r
        Shape.Point => 0
    }
}

fn main() {
    io.print(f(Shape.Circle(radius: 7)))
    io.print(f(Shape.Point))
}
```

A qualified name that is not a variant of that enum is `E0111`, and the enum's own
name where a variant belongs is `E0200` — neither is a catch-all:

```kite fails
enum Shape {
    Circle(radius: int)
    Point
}

fn f(s: Shape) -> int {
    return match s {
        Shape.Square(r) => r //~ E0111
        Shape(r) => 0 //~ E0200
    }
}

fn main() {
    io.print(f(Shape.Point))
}
```

The same name in an expression is the same `E0111`, called or not. (Before it
was, `let s = Shape.Square` compiled to a value of no type at all, and
`s == Shape.Point` printed `()`.)

```kite fails
enum Shape {
    Circle(radius: int)
    Point
}

fn main() {
    let s = Shape.Square //~ E0111
    let t = Shape.Oval(2) //~ E0111
    io.print(s == Shape.Point)
}
```

An *unqualified* pattern name that collides with a variant of the scrutinee's
enum is *that variant*, not a fresh binding — so it does not act as a catch-all,
and the remaining arms stay reachable (and required). A name matching no variant
is an ordinary binding, and does catch all.

---

## 4. `match`

`match` is exhaustive, in expression **and** statement position. The diagnostic
names the missing variants.

```kite fails
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}

fn main() {
    let s = Shape.Circle(radius: 2)
    match s { //~ E0210
        Circle(r) => io.print(r)
    }
}
```

> ``error[E0210]: non-exhaustive match: `Rect(_, _)`, `Point` not covered``

A **guarded** arm never contributes to exhaustiveness, even when the guard is
`true`:

```kite fails
enum E {
    A
    B
}

fn f(e: E) -> int {
    return match e { //~ E0210
        A if true => 1
        B => 2
    }
}

fn main() {
    io.print(f(E.A))
}
```

Coverage is worked out through nested patterns, not just the outermost one
(Maranget's usefulness algorithm). Each of these is exhaustive with no `_`, and
a pattern written against an optional is one for the value inside it:

```kite
enum Light {
    On(bool)
    Off
}

enum E {
    A
    B
}

fn main() {
    let l = Light.On(true)
    io.print(match l {
        On(true) => "bright",
        On(false) => "dim",
        Off => "off",
    })
    io.print(match (true, 1) {
        (true, _) => "t",
        (false, _) => "f",
    })
    let o: Option<E> = E.B
    io.print(match o {
        nil => "none",
        A => "a",
        B => "b",
    })
}
```

A missing case is named however deep it is — ``non-exhaustive match:
`Add(Num(_), _)` not covered``. An arm that no value can reach, because the arms
above it already take everything it matches, is a **warning**, `E0116`. The
commonest cause is a variant name spelled wrong, which makes it a binding that
catches everything:

```kite
enum Kind {
    Missing
    File
    Directory
}

fn main() {
    let k = Kind.File
    match k {
        Dir => io.print("dir"),       // warning: `Dir` is a binding; did you mean `Directory`?
        File => io.print("file"),     // warning: unreachable match arm
        Missing => io.print("missing"),
    }
}
```

### As an expression

Arms are separated by a newline or a comma (both accepted, mixable). Every arm
must produce the same type.

```kite
fn classify(n: int) -> str {
    return match n {
        0            => "zero",
        1 | 2 | 3    => "small",
        4..=9        => "medium",
        n if n < 0   => "negative",
        _            => "large",
    }
}

fn main() {
    io.print(classify(0))
    io.print(classify(2))
    io.print(classify(7))
    io.print(classify(-1))
    io.print(classify(99))
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

### Block arms and the absence of tail expressions

A block arm holding **one** expression produces that value. A block arm holding
more than one statement produces `()` — there is no falling off the end.

```kite
fn main() {
    let s = match 1 {
        0 => { "zero" }
        _ => "other"
    }
    io.print(s)
}
```

```kite fails
fn main() {
    let s = match 1 { //~ E0200
        0 => {
            let a = "zero"
            a
        }
        _ => "other"
    }
    io.print(s)
}
```

So an arm needing several statements *and* a result returns from the enclosing
function, with the `match` in statement position. Every arm leaving means the
`match` leaves, and the function needs no `return` after it:

```kite
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}

fn area(s: Shape) -> int {
    match s {
        Circle(r) => {
            let d = r * 2
            return 3 * d * d / 4
        }
        Rect(w, h) => {
            return w * h
        }
        Point => {
            return 0
        }
    }
}

fn main() {
    io.print(area(Shape.Circle(radius: 2)))
    io.print(area(Shape.Rect(width: 3, height: 4)))
    io.print(area(Shape.Point))
}
```

### Patterns

Literals, alternation `|`, inclusive ranges `..=`, guards, wildcard `_`, struct
patterns (with field shorthand and a trailing `..`), tuple patterns, and `nil`
for optionals. Bindings from patterns are immutable; there is no `ref` or `mut`,
because there are no references to bind.

```kite
struct Point {
    x: int
    y: int
}

fn where_is(p: Point) -> str {
    return match p {
        Point{ x: 0, y: 0 } => "origin"
        Point{ x: 0, y }    => "on the y axis at \(y)"
        Point{ x, .. }      => "off axis, x = \(x)"
    }
}

fn both(pair: (Option<int>, Option<int>)) -> str {
    return match pair {
        (nil, nil) => "neither"
        (a, nil)   => "first only"
        (nil, b)   => "second only"
        (a, b)     => "both"
    }
}

fn main() {
    io.print(where_is(Point{ x: 0, y: 0 }))
    io.print(where_is(Point{ x: 0, y: 5 }))
    io.print(where_is(Point{ x: 2, y: 5 }))
    io.print(both((1, nil)))
    io.print(both((nil, nil)))
}
```

---

## 5. Traits

Declaration is nominal and explicit — there is no structural satisfaction.
Methods may carry a default body.

```kite
struct Rect {
    width: int
    height: int
}

struct Circle {
    radius: int
}

pub trait Shape {
    fn area(self) -> int

    fn describe(self) -> str {
        return "a shape of \(self.area())"
    }
}

impl Shape for Rect {
    fn area(self) -> int {
        return self.width * self.height
    }

    fn describe(self) -> str {
        return "a rectangle"
    }
}

impl Shape for Circle {
    fn area(self) -> int {
        return 3 * self.radius * self.radius
    }
}

fn main() {
    io.print(Rect{ width: 3, height: 4 }.describe())
    io.print(Circle{ radius: 2 }.describe())
}
```

An implementation must supply every method without a default:

```kite fails
struct P {
    n: int
}

trait Shape {
    fn area(self) -> int
}

impl Shape for P { //~ E0200
}

fn main() {
    io.print(1)
}
```

…and may supply **nothing else**. Extra methods go in an inherent block:

```kite fails
trait Greet {
    fn hi(self) -> str
}

struct P {
    n: int
}

impl Greet for P {
    fn hi(self) -> str {
        return "hi"
    }
    fn extra(self) -> int { //~ E0200
        return 1
    }
}

fn main() {
    io.print(P{ n: 1 }.hi())
}
```

One implementation per (trait, type) pair:

```kite fails
struct P {
    n: int
}

trait Shape {
    fn area(self) -> int
}

impl Shape for P {
    fn area(self) -> int {
        return 1
    }
}

impl Shape for P { //~ E0112
    fn area(self) -> int {
        return 2
    }
}

fn main() {
    io.print(1)
}
```

The check is module-wide: two files of one module directory that each write
`impl Display for Item` are the same `E0112`.

**An `impl` belongs to the module of its type** — or, for a trait
implementation, the module of its trait (§10.2's orphan rule). There are no
extension methods: an `impl` block for an imported type is `E0406`, and so is
implementing an imported trait for an imported type. A trait of your own may be
implemented for an imported type, which is the way to give one new behaviour; a
plain function taking it is the other.

```kite fails
use std/json

impl json.Json { //~ E0406
    fn tag(self) -> str {
        return "?"
    }
}

fn main() {
    io.print(json.Json.Null.tag())
}
```

```kite
use std/json

trait Tagged {
    fn tag(self) -> str
}

impl Tagged for json.Json {
    fn tag(self) -> str {
        return match self {
            Null => "null"
            _ => "other"
        }
    }
}

fn main() {
    io.print(json.Json.Null.tag())
}
```

A trait may not be implemented for a primitive — `impl Doubler for int` is
`E0204`, "an `impl` block needs a type declared in this module".

When a type has both an inherent method and a trait method of the same name, the
one that wins on the concrete type is whichever `impl` block comes **first in the
file**. Inherent does not take priority, and nothing is reported — method lookup
is a scan in declaration order that never asks which kind of block it found. A
`dyn Trait` always runs the trait's body. This prints `trait` twice; swap the two
blocks and the first line becomes `inherent`.

```kite
trait Greet {
    fn hi(self) -> str
}

struct P {
    n: int
}

impl Greet for P {
    fn hi(self) -> str {
        return "trait"
    }
}

impl P {
    fn hi(self) -> str {
        return "inherent"
    }
}

fn main() {
    io.print(P{ n: 1 }.hi())
    let d: dyn Greet = P{ n: 1 }
    io.print(d.hi())
}
```

### The prelude's traits

`std/prelude.kite` declares four traits every program sees, plus `Share`:

| Trait | Method | How you get it |
|---|---|---|
| `Display` | `fn show(self) -> str` | hand-written only; drives `io.print` and `\(x)` |
| `Debug` | `fn debug(self) -> str` | `@derive(Debug)` or by hand |
| `Error` | `fn message(self) -> str` | by hand; lets the type stand in an `error` slot |
| `Hash` | `fn hash(self) -> int` | `@derive(Hash)` or by hand |
| `Share` | (no methods) | inferred structurally; never written |

`json.Encode` (`fn encode(self) -> Json`) lives in `std/json` because it names
`Json`. There is no `Eq` and no `Ord`: `==` is structural on every value, and `<`
on aggregates is simply undefined (`E0201`) — sorting takes the comparison as an
argument.

There is also no `Iterate`: `impl Iterate for L` is ``E0204: unknown trait
`Iterate` ``, and `for x in l` over a user type is ``E0200: cannot iterate a `L` ``.
A type is made iterable by exposing a slice and iterating that. The trait cannot
be written because it would need an associated type, and Kite has none —
`type Item` inside a trait is a parse error.

A trait method may take `var self`, and an implementation must match:

```kite
trait Bump {
    fn bump(var self)
}

struct C {
    var n: int
}

impl Bump for C {
    fn bump(var self) {
        self.n = self.n + 1
    }
}

fn main() {
    var c = C{ n: 0 }
    c.bump()
    io.print(c.n)
}
```

`Display` is the only route to printing a user type:

```kite
struct Money {
    pence: int
}

impl Display for Money {
    fn show(self) -> str {
        let pounds = self.pence / 100
        let rest = self.pence % 100
        let pad = if rest < 10 { "0" } else { "" }
        return "\(pounds).\(pad)\(rest)"
    }
}

fn main() {
    io.print(Money{ pence: 1999 })
    io.print("that costs \(Money{ pence: 250 })")
}
```

```kite fails
struct U {
    name: str
}

fn main() {
    let u = U{ name: "a" }
    io.print("\(u)") //~ E0207
}
```

A type implementing `Error` may be returned in an error slot, and a caller
recovers it by naming the type — `T.is(err) -> bool`, `T.as(err) -> Option<T>`.
This is the turbofish-free spelling of a downcast; a type that declares its own
`is`/`as` keeps them.

```kite
struct NotFound {
    resource: str
    id: str
}

impl Error for NotFound {
    fn message(self) -> str {
        return "\(self.resource) \(self.id) not found"
    }
}

fn load(id: str) -> (int, error) {
    if id == "" {
        return _, NotFound{ resource: "user", id: id }
    }
    return 1, nil
}

fn main() {
    let (n, err) = load("")
    if NotFound.is(err) {
        io.print("missing")
    }
    let found = NotFound.as(err)
    io.print(if found == nil { "other" } else { found.resource })
}
```

---

## 6. Trait objects (`dyn`)

`dyn Trait` is required to be explicit — a generic parameter is static dispatch, a
`dyn` is a vtable. A `dyn Trait` may be a parameter, a return type, a binding, a
slice element, or a struct field.

```kite
trait Drawable {
    fn area(self) -> int
    fn name(self) -> str

    fn describe(self) -> str {
        return "a \(self.name())"
    }
}

struct Circle {
    radius: int
}

struct Rect {
    width: int
    height: int
}

impl Drawable for Circle {
    fn area(self) -> int {
        return 3 * self.radius * self.radius
    }
    fn name(self) -> str {
        return "circle"
    }
}

impl Drawable for Rect {
    fn area(self) -> int {
        return self.width * self.height
    }
    fn name(self) -> str {
        return "rect"
    }
    fn describe(self) -> str {
        return "a \(self.width) by \(self.height) rect"
    }
}

struct Slot {
    held: dyn Drawable
}

fn total_area(shapes: [dyn Drawable]) -> int {
    var sum = 0
    for s in shapes {
        sum = sum + s.area()
    }
    return sum
}

fn main() {
    let shapes: [dyn Drawable] = [
        Circle{ radius: 2 },
        Rect{ width: 1, height: 5 },
    ]
    for s in shapes {
        io.print(s.describe())
    }
    io.print(total_area(shapes))
    io.print(Slot{ held: Circle{ radius: 1 } }.held.describe())
}
```

A concrete value coerces into `dyn Trait` only if it has an `impl` for that trait
— having the right methods is not enough:

```kite fails
trait Shape {
    fn area(self) -> int
}

struct Blob {
    n: int
}

impl Blob {
    fn area(self) -> int {
        return self.n
    }
}

fn main() {
    let s: dyn Shape = Blob{ n: 3 } //~ E0200
    io.print(s.area())
}
```

A `dyn Trait` exposes its trait's methods **and no others**:

```kite fails
trait Shape {
    fn area(self) -> int
}

struct Circle {
    r: int
}

impl Shape for Circle {
    fn area(self) -> int {
        return self.r * self.r
    }
}

impl Circle {
    fn radius(self) -> int {
        return self.r
    }
}

fn main() {
    let s: dyn Shape = Circle{ r: 2 }
    io.print(s.radius()) //~ E0205
}
```

**Object safety** (§10.3): a trait can be a `dyn` only when every method takes
`self`, none mentions `Self` otherwise, and none is generic — a `dyn` holds a
type the call cannot know, so each method has to mean the same thing whichever
it is. The diagnostic names the method and which rule it breaks.

```kite fails
trait Factory {
    fn make() -> int
    fn count(self) -> int
}

struct Widget {
    n: int
}

impl Factory for Widget {
    fn make() -> int {
        return 0
    }
    fn count(self) -> int {
        return self.n
    }
}

fn use_it(f: dyn Factory) { //~ E0206
    io.print(f.count())
}

fn main() {
    use_it(Widget{ n: 1 })
}
```

A non-object-safe trait is still usable as a generic bound, where the type is
known and every method is an ordinary call — `Self` included:

```kite
trait Same {
    fn same(self, other: Self) -> bool
}

struct A {
    x: int
}

impl Same for A {
    fn same(self, other: A) -> bool {
        return self.x == other.x
    }
}

fn all_same<T: Same>(xs: [T]) -> bool {
    for x in xs {
        if !x.same(xs[0]) {
            return false
        }
    }
    return true
}

fn main() {
    io.print(all_same([A{ x: 1 }, A{ x: 1 }]))
}
```

```kite fails
trait Same {
    fn same(self, other: Self) -> bool
}

fn first(xs: [dyn Same]) -> int { //~ E0206
    return xs.len()
}

fn main() {
    io.print(1)
}
```

And `==` does not reach through a trait object:

```kite fails
trait Shape {
    fn area(self) -> int
}

struct Sq {
    s: int
}

impl Shape for Sq {
    fn area(self) -> int {
        return self.s * self.s
    }
}

fn main() {
    let a: dyn Shape = Sq{ s: 2 }
    let b: dyn Shape = Sq{ s: 2 }
    io.print(a == b) //~ E0201
}
```

---

## 7. Generics

Type parameters go on free functions, on `impl` block headers, and on methods
(after the block's). Bounds are trait names joined with `+`. Instantiations are monomorphised — one specialised
copy per set of type arguments, so no backend ever sees a type parameter.

```kite
trait Named {
    fn name(self) -> str
}

trait Measured {
    fn area(self) -> int
}

struct Sq {
    s: int
}

impl Named for Sq {
    fn name(self) -> str {
        return "square"
    }
}

impl Measured for Sq {
    fn area(self) -> int {
        return self.s * self.s
    }
}

fn first<T>(items: [T]) -> Option<T> {
    if items.len() == 0 {
        return nil
    }
    return items[0]
}

fn pair<A, B>(a: A, b: B) -> (A, B) {
    return (a, b)
}

fn describe<T: Named + Measured>(x: T) -> str {
    return "\(x.name()) of \(x.area())"
}

fn main() {
    let n = first([3, 1, 4])
    io.print(if n == nil { -1 } else { n })
    let s = first(["a", "b"])
    io.print(if s == nil { "none" } else { s })
    match pair("count", 3) {
        (label, value) => io.print("\(label): \(value)")
    }
    io.print(describe(Sq{ s: 3 }))
}
```

Without a bound, nothing is known about the parameter and nothing can be called
on it:

```kite fails
trait Shape {
    fn area(self) -> int
}

struct Blob {
    n: int
}

impl Shape for Blob {
    fn area(self) -> int {
        return self.n
    }
}

fn total<T>(xs: [T]) -> int {
    var sum = 0
    for x in xs {
        sum = sum + x.area() //~ E0205
    }
    return sum
}

fn main() {
    io.print(total([Blob{ n: 1 }]))
}
```

A parameter satisfies exactly the bounds it was declared with, so a bounded one
may be passed on to the same bound — `fn outer<T: Shape>(x: T)` may call
`inner<T: Shape>(x)` — and an unbounded one may not.

A bound is checked at every call site of a generic **function**:

```kite fails
trait Shape {
    fn area(self) -> int
}

struct Blob {
    n: int
}

fn total<T: Shape>(xs: [T]) -> int {
    var sum = 0
    for x in xs {
        sum = sum + x.area()
    }
    return sum
}

fn main() {
    io.print(total([Blob{ n: 1 }])) //~ E0208
}
```

### Inference, and the absence of a turbofish

Type arguments come from the argument types. Two arguments that disagree about
one parameter is `E0209`; a parameter appearing in no parameter type is `E0209`
too, and there is no syntax to supply it.

```kite fails
fn same<T>(a: T, b: T) -> T {
    return a
}

fn main() {
    io.print(same(1, "two")) //~ E0209
}
```

```kite fails
fn make<T>() -> Option<T> {
    return nil
}

fn main() {
    let x = make() //~ E0209
    io.print(if x == nil { 0 } else { 1 })
}
```

Inference does not flow *into* a closure literal from a generic parameter type.
A `fn(T) -> U` parameter takes a named function, or a closure whose own
parameters are annotated; a bare `|n| …` leaves `T` unsolved and reports
``expected `fn(int) -> str`, found `fn(<error>) -> str` ``.

```kite
fn apply<T, U>(items: [T], f: fn(T) -> U) -> [U] {
    var out: [U] = []
    for item in items {
        out.push(f(item))
    }
    return out
}

fn label(n: int) -> str {
    return "n\(n)"
}

fn main() {
    for s in apply([1, 2, 3], label) {
        io.print(s)
    }
    for s in apply([1, 2], |n: int| "x\(n)") {
        io.print(s)
    }
}
```

```kite fails
fn apply<T, U>(items: [T], f: fn(T) -> U) -> [U] {
    var out: [U] = []
    for item in items {
        out.push(f(item))
    }
    return out
}

fn main() {
    for s in apply([1, 2, 3], |n| "n\(n)") { //~ E0200
        io.print(s)
    }
}
```

### Generic structs and enums

`Box` is a template, not a type; `Box<int>` is a type. There is no
`Box<int>{ … }` literal spelling — `<` in expression position is a comparison, so
that reads as `Box` standing alone and is `E0200`, "`Box` is a type, not a
value". Arguments are inferred from the field values, or from the binding's
annotation.

```kite
struct Box<T> {
    value: T
}

struct Pair<A, B> {
    first: A
    second: B
}

struct Tree<T> {
    label: T
    children: [Tree<T>]
}

enum Outcome<T, E> {
    Ok(T)
    Err(E)
}

impl<T> Box<T> {
    fn get(self) -> T {
        return self.value
    }

    fn replaced(self, v: T) -> Box<T> {
        return Box{ value: v }
    }

    fn of(v: T) -> Box<T> {
        return Box{ value: v }
    }
}

fn size<T>(tree: Tree<T>) -> int {
    var total = 1
    for child in tree.children {
        total = total + size(child)
    }
    return total
}

fn or_else(r: Outcome<int, str>, fallback: int) -> int {
    return match r {
        Ok(value) => value
        Err(message) => fallback
    }
}

fn main() {
    let n = Box{ value: 42 }
    io.print(n.get())
    io.print(n.replaced(9).get())

    let deep: Box<Box<int>> = Box{ value: Box{ value: 7 } }
    io.print(deep.value.value)

    let p = Pair{ first: 1, second: "one" }
    io.print("\(p.first) is \(p.second)")

    let leaf = Tree{ label: 3, children: [] }
    io.print(size(Tree{ label: 1, children: [leaf, leaf] }))

    // An associated function has no receiver, so the arguments come from the
    // type the result is used as, or from its own arguments.
    let made: Box<bool> = Box.of(true)
    io.print(made.get())

    io.print(or_else(Outcome.Ok(5), -1))
    io.print(or_else(Outcome.Err("no"), -1))
}
```

A bound on a generic type holds everywhere. On the `struct` or `enum` header it
is checked wherever a value is built, so a `Holder<int>` cannot exist; on the
`impl` header it is checked at every call of the block's methods, and it is what
lets their bodies call the bound's methods. Write it on both:

```kite
struct Holder<T: Display> {
    value: T
}

struct M {
    p: int
}

impl Display for M {
    fn show(self) -> str {
        return "M\(self.p)"
    }
}

impl<T: Display> Holder<T> {
    fn tell(self) -> str {
        return self.value.show()
    }
}

fn main() {
    io.print(Holder{ value: M{ p: 5 } }.tell())
}
```

Drop the `: Display` from the `impl` header and `self.value.show()` becomes
``E0205: `T` has no method `show` `` — even though the `struct` header still
carries the bound. Build a `Holder` of something that is not `Display` and the
literal is `E0208`:

```kite fails
struct Holder<T: Display> {
    value: T
}

fn main() {
    let h = Holder{ value: 5 } //~ E0208
    io.print(1)
}
```

A generic type implements a trait only where its arguments meet the bounds of
the `impl` that says so: after `impl<T: Display> Named for Holder<T>`, a
`Holder<int>` is not `Named`, as a bound or as a `dyn`.

A generic name used without arguments, or with the wrong number, is `E0208`:

```kite fails
struct Box<T> {
    value: T
}

fn take(b: Box) -> int { //~ E0208
    return 0
}

fn main() {
    io.print(take(Box{ value: 1 }))
}
```

An `impl` is for every instantiation at once, so its header names the type at
the block's own parameters, in order: `impl<T> Named for Box<T>`. A header for
one instantiation, `impl Named for Box<int>`, or one that names no parameters
for the type's, `impl Named for Box`, is `E0208`; a bound on the block's
parameter says which instantiations it covers:

```kite fails
trait Named {
    fn name(self) -> str
}

struct Box<T> {
    value: T
}

impl Named for Box { //~ E0208
    fn name(self) -> str {
        return "box"
    }
}

fn main() {
    io.print(Box{ value: 1 }.name())
}
```

A unit variant of a generic enum says nothing about the arguments, so the binding
must — and when it does, the bare variant is enough, `let m: Maybe<int> = None`:

```kite fails
enum Maybe<T> {
    None
    Some(T)
}

fn main() {
    let m = Maybe.None //~ E0209
    io.print(1)
}
```

```kite
enum Maybe<T> {
    None
    Some(T)
}

fn main() {
    let m: Maybe<int> = Maybe.None
    io.print(match m {
        None => 0
        Some(n) => n
    })
}
```

An associated function on a generic type with nothing to infer from behaves the
same way — `let s = Stack.empty()` is `E0209`, `let s: Stack<int> = Stack.empty()`
compiles.

**A `Display` bound makes the parameter printable**, and so does a `dyn
Display`: `io.print` and interpolation show either through the trait.

```kite
struct P {
    x: int
}

impl Display for P {
    fn show(self) -> str {
        return "P(\(self.x))"
    }
}

fn tell<T: Display>(x: T) {
    io.print("\(x)")
    io.print(x)
}

fn main() {
    tell(P{ x: 1 })
    let d: dyn Display = P{ x: 2 }
    io.print("\(d)")
}
```

**`==` on a type parameter holds every caller to it.** Equality is defined on
everything but a function, a `dyn Trait` and a `JsValue`, so a generic body may
compare its `T` — and a call choosing one of those three for `T` is `E0201`, as
is a call passing its own parameter on to such a body:

```kite fails
fn same<T>(a: T, b: T) -> bool {
    return a == b
}

fn main() {
    io.print(same(1, 1))
    let f = |x: int| x + 1
    io.print(same(f, f)) //~ E0201
}
```

---

## 8. `@derive`

`@derive(…)` is one of Kite's two attributes (the other is `@host`). It sits in
front of a `struct` or an `enum` and expands to ordinary Kite before resolution —
`kitec --emit hir` shows the result. Exactly four traits derive:

| `@derive(…)` | What it adds | Shape |
|---|---|---|
| `Debug` | `impl Debug for T { fn debug(self) -> str }` | `T{ f: 1, g: "s" }`; `Variant(1, "s")`; `Variant(label: "s")` |
| `Hash` | `impl Hash for T { fn hash(self) -> int }` | FNV-1a fold over the same fields `==` compares, in the same order |
| `Encode` | `impl json.Encode for T { fn encode(self) -> json.Json }` | struct → object keyed by field name; enum → externally tagged |
| `Decode` | an **inherent** `fn decode(doc: json.Json) -> (T, error)` | not a trait method, because it returns the implementing type |

```kite
@derive(Debug, Hash)
struct Book {
    title: str
    year: int
    authors: [str]
    edition: Option<int>
}

@derive(Debug, Hash)
enum Shelf {
    Empty
    Boxed(int)
    Named(label: str, capacity: int)
}

fn main() {
    let b = Book{ title: "MMM", year: 1975, authors: ["Brooks"], edition: nil }
    io.print(b.debug())
    io.print(Shelf.Empty.debug())
    io.print(Shelf.Boxed(3).debug())
    io.print(Shelf.Named(label: "front", capacity: 40).debug())

    let same = Book{ title: "MMM", year: 1975, authors: ["Brooks"], edition: nil }
    io.print(b == same)
    io.print(b.hash() == same.hash())
}
```

prints

```
Book{ title: "MMM", year: 1975, authors: ["Brooks"], edition: nil }
Empty
Boxed(3)
Named(label: "front", capacity: 40)
true
true
```

### `Encode` / `Decode`

Both expand to code written against `std/json`, and the expansion names it
however the module does — `use std/json as j` makes it `j.Encode` — or, in a
module that never imports it, by a spelling of its own. So the derive itself
needs no `use`; what needs one is *your* code calling `json.stringify` or
`json.parse` on the result:

```kite
@derive(Encode, Decode)
struct U {
    n: int
}

fn main() {
    let (back, err) = U.decode(U{ n: 1 }.encode())
    if err != nil {
        return
    }
    io.print(back.n)
}
```

The round trip is an ordinary pair of calls. `decode` is named
on the type because there is no turbofish to write `json.decode<User>(text)`
with, and it returns `(T, error)` — a missing field is an error, never a zero:

```kite
use std/json

@derive(Debug, Encode, Decode)
struct Book {
    title: str
    year: int
}

@derive(Debug, Encode, Decode)
enum Shelf {
    Empty
    Boxed(int)
    Named(label: str, capacity: int)
}

fn main() {
    let text = json.stringify(Book{ title: "MMM", year: 1975 }.encode())
    io.print(text)
    io.print(json.stringify(Shelf.Empty.encode()))
    io.print(json.stringify(Shelf.Boxed(3).encode()))
    io.print(json.stringify(Shelf.Named(label: "f", capacity: 4).encode()))

    let (doc, err) = json.parse(text)
    if err != nil {
        return
    }
    let (back, derr) = Book.decode(doc)
    if derr != nil {
        io.print(derr.message())
        return
    }
    io.print(back.debug())

    let (bad, berr) = json.parse("{\"title\": \"untitled\"}")
    if berr != nil {
        return
    }
    let (broken, ferr) = Book.decode(bad)
    io.print(if ferr == nil { "?" } else { ferr.message() })
}
```

prints

```
{"title":"MMM","year":1975}
"Empty"
{"Boxed":[3]}
{"Named":{"label":"f","capacity":4}}
Book{ title: "MMM", year: 1975 }
Book.year: expected a whole number
```

So the JSON encoding of an enum is: unit variant → its own name as a string;
positional payload → `{"Variant": [...]}`; named payload → `{"Variant": {...}}`.

### What does not derive

`Display` never derives — how a type reads to a person is a design decision:

```kite fails
@derive(Display) //~ E0701
struct U {
    name: str
}

fn main() {
    io.print(1)
}
```

Neither does `Eq` (structural already) nor `Ord` (undefined on aggregates). And
the derived walk must be able to handle every field — a primitive, slice, map,
optional, tuple, or another type deriving the same trait. A field that stops it
is `E0702`, pointing at the field and naming its type:

```kite fails
struct Plain {
    x: int
}

@derive(Debug)
struct Outer {
    inner: Plain //~ E0702
}

fn main() {
    io.print(Outer{ inner: Plain{ x: 1 } }.inner.x)
}
```

Deriving a trait the type also implements by hand is `E0701`
("`U` already implements `Debug`"). An *inherent* method of the same name is not
caught, and silently shadows the derived one.

---

## 9. Where the compiler and SPECIFICATION.md disagree

The compiler is authoritative. On §8, §10 and §11 it now agrees with the spec:
`Self` is the implementing type (§10.1), a trait is a `dyn` only when every
method takes `self`, is not generic and does not mention `Self` (§10.3), the
orphan rule and the ban on extension methods are enforced (`E0406`, §8.2 and
§10.2), a bound on a generic struct or enum holds wherever a value is built
(§11), and a method may declare type parameters of its own.

Two spec omissions worth knowing: `@derive(Encode)`/`@derive(Decode)` silently
require `use std/json` in the deriving file, and `Decode` is emitted as an
inherent associated function rather than a trait implementation, so there is no
`Decode` trait to name in a bound.

The specification never writes a qualified pattern; the compiler accepts one
(`Enum.Variant(…)`) as the same variant the unqualified spelling names, and it is
the way to match a payload variant two enums share (§3).
