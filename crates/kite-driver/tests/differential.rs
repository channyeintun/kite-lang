//! Differential testing across backends.
//!
//! Every program is compiled three ways — to bytecode, to WebAssembly, and to
//! native machine code through Cranelift — and run on all three. The outputs
//! must match.
//!
//! This is the highest-value test in the tree. Three independent
//! implementations that must agree find codegen bugs almost for free, and
//! codegen bugs are the hardest class to find any other way. It is also the
//! reason the bytecode VM was built before the Wasm backend even though Wasm
//! is the point of the project.
//!
//! The Wasm half needs Node. When Node is absent the module is still compiled
//! and validated; only the execution comparison is skipped, and the test says
//! so rather than silently passing. The native half runs under the JIT and
//! needs no linker; the one test that does need `cc` — linking a real
//! executable — skips the same way when there is none.

use kite_driver::{compile, Emit};
use std::process::Command;

/// Programs exercised on both backends. Each must use only what the Wasm
/// backend lowers today: numbers, booleans, strings, functions, control flow.
const PROGRAMS: &[(&str, &str)] = &[
    (
        "phase-one",
        "\
fn add(a: int, b: int) -> int {
    return a + b
}
fn main() {
    let x = add(2, 3)
    if x > 4 {
        io.print(\"big\")
    }
    for i in 0..x {
        io.print(i)
    }
}
",
    ),
    (
        "arithmetic",
        "fn main() {\n  io.print(2 + 3 * 4 - 10 / 2 % 3)\n  io.print(1.5 + 2.5 * 2.0)\n  io.print(-7)\n}\n",
    ),
    (
        "comparison",
        "fn main() {\n  io.print(1 < 2)\n  io.print(2.0 >= 3.0)\n  io.print(true != false)\n  io.print(!false)\n}\n",
    ),
    (
        "bitwise",
        "fn main() {\n  io.print(12 & 10)\n  io.print(12 | 10)\n  io.print(12 ^ 10)\n  io.print(1 << 4)\n  io.print(256 >> 4)\n  io.print(6 & 3 == 2)\n}\n",
    ),
    (
        "loops",
        "fn main() {\n  for i in 0..3 {\n    io.print(i)\n  }\n  for i in 0..=2 {\n    io.print(i)\n  }\n  var n = 0\n  for n < 3 {\n    io.print(n)\n    n += 1\n  }\n}\n",
    ),
    (
        "continue-advances",
        "fn main() {\n  for i in 0..5 {\n    if i == 2 {\n      continue\n    }\n    io.print(i)\n  }\n}\n",
    ),
    (
        "labelled-jumps",
        "fn main() {\n  outer: for i in 0..3 {\n    for j in 0..3 {\n      if j == 1 {\n        continue outer\n      }\n      io.print(i * 10 + j)\n    }\n  }\n}\n",
    ),
    (
        "recursion",
        "fn fact(n: int) -> int {\n  if n <= 1 {\n    return 1\n  }\n  return n * fact(n - 1)\n}\nfn main() {\n  io.print(fact(10))\n}\n",
    ),
    (
        "mutual-recursion",
        "fn is_even(n: int) -> bool {\n  if n == 0 {\n    return true\n  }\n  return is_odd(n - 1)\n}\nfn is_odd(n: int) -> bool {\n  if n == 0 {\n    return false\n  }\n  return is_even(n - 1)\n}\nfn main() {\n  io.print(is_even(10))\n  io.print(is_odd(7))\n}\n",
    ),
    (
        "short-circuit",
        "fn main() {\n  let a = true\n  let b = false\n  io.print(a && b)\n  io.print(a || b)\n}\n",
    ),
    (
        "if-expression",
        "fn main() {\n  let label = if 12 > 10 { \"big\" } else { \"small\" }\n  io.print(label)\n}\n",
    ),
    (
        "nested-calls",
        "fn add(a: int, b: int) -> int {\n  return a + b\n}\nfn main() {\n  io.print(add(add(1, 2), add(3, 4)))\n}\n",
    ),
    (
        "structs",
        "struct Rect {\n  width: int\n  height: int\n  var label: str\n}\n\
         impl Rect {\n  fn area(self) -> int {\n    return self.width * self.height\n  }\n\
         \x20 fn wider(self, by: int) -> Rect {\n    return Rect{ ..self, width: self.width + by }\n  }\n}\n\
         fn main() {\n  let r = Rect{ width: 3, height: 4, label: \"a\" }\n\
         \x20 io.print(r.area())\n  io.print(r.wider(7).area())\n  io.print(r.area())\n}\n",
    ),
    (
        "struct-mutation",
        "struct C {\n  var n: int\n}\n\
         impl C {\n  fn bump(var self) {\n    self.n = self.n + 1\n  }\n}\n\
         fn main() {\n  var c = C{ n: 1 }\n  c.bump()\n  c.bump()\n  io.print(c.n)\n}\n",
    ),
    (
        "nested-structs",
        "struct Inner {\n  n: int\n}\nstruct Outer {\n  inner: Inner\n}\n\
         fn main() {\n  let o = Outer{ inner: Inner{ n: 42 } }\n  io.print(o.inner.n)\n}\n",
    ),
    (
        "enums",
        "enum Shape {\n  Circle(radius: int)\n  Rect(width: int, height: int)\n  Point\n}\n\
         fn describe(s: Shape) -> str {\n  return match s {\n    Circle(r) => \"circle\",\n\
         \x20   Rect(w, h) if w == h => \"square\",\n    Rect(w, h) => \"rect\",\n\
         \x20   Point => \"point\",\n  }\n}\n\
         fn area(s: Shape) -> int {\n  return match s {\n    Circle(r) => 3 * r * r,\n\
         \x20   Rect(w, h) => w * h,\n    Point => 0,\n  }\n}\n\
         fn main() {\n  io.print(describe(Circle(radius: 2)))\n  io.print(area(Circle(radius: 2)))\n\
         \x20 io.print(describe(Rect(width: 3, height: 3)))\n\
         \x20 io.print(describe(Rect(width: 3, height: 4)))\n\
         \x20 io.print(area(Rect(width: 3, height: 4)))\n  io.print(describe(Point))\n}\n",
    ),
    (
        "recursive-enum",
        "enum Tree {\n  Leaf(int)\n  Node(left: Tree, right: Tree)\n}\n\
         fn total(t: Tree) -> int {\n  return match t {\n    Leaf(n) => n,\n\
         \x20   Node(l, r) => total(l) + total(r),\n  }\n}\n\
         fn main() {\n  let t = Node(left: Node(left: Leaf(1), right: Leaf(2)), right: Leaf(3))\n\
         \x20 io.print(total(t))\n}\n",
    ),
    (
        "literal-patterns",
        "fn classify(n: int) -> str {\n  return match n {\n    0 => \"zero\",\n\
         \x20   1 | 2 | 3 => \"small\",\n    4..=9 => \"medium\",\n    _ => \"large\",\n  }\n}\n\
         fn main() {\n  io.print(classify(0))\n  io.print(classify(2))\n\
         \x20 io.print(classify(9))\n  io.print(classify(10))\n}\n",
    ),
    (
        "struct-patterns",
        "struct P {\n  x: int\n  y: int\n}\n\
         fn where_is(p: P) -> str {\n  return match p {\n    P{ x: 0, y: 0 } => \"origin\",\n\
         \x20   P{ x: 0, y } => \"on y\",\n    P{ x, y } => \"elsewhere\",\n  }\n}\n\
         fn main() {\n  io.print(where_is(P{ x: 0, y: 0 }))\n  io.print(where_is(P{ x: 0, y: 5 }))\n\
         \x20 io.print(where_is(P{ x: 1, y: 5 }))\n}\n",
    ),
    (
        "display",
        "struct P {\n  x: int\n  y: int\n}\n\
         impl Display for P {\n  fn show(self) -> str {\n\
         \x20   return \"(\\(self.x), \\(self.y))\"\n  }\n}\n\
         enum S {\n  Dot\n  Circle(int)\n}\n\
         impl Display for S {\n  fn show(self) -> str {\n\
         \x20   return match self {\n      Dot => \"dot\"\n\
         \x20     Circle(r) => \"circle \\(r)\"\n    }\n  }\n}\n\
         fn main() {\n  let p = P{x: 3, y: 4}\n\
         \x20 io.print(p)\n  io.print(\"at \\(p)\")\n\
         \x20 io.print(S.Dot)\n  io.print(S.Circle(9))\n\
         \x20 io.print(\"\\(S.Dot) and \\(P{x: 0, y: 0})\")\n\
         \x20 io.print(join(map([p, P{x: 1, y: 1}], |q: P| q.show()), \" \"))\n\
         \x20 io.print(p.show() == \"(3, 4)\")\n}\n",
    ),
    (
        "error-types",
        "// A concrete type saying what its own failure means. Returning one\n\
         // where an `error` is expected converts it at that point, which is an\n\
         // ordinary call to `message` in the IR — so all three backends lower\n\
         // it without knowing the trait exists.\n\
         enum LoadError {\n  Absent(path: str)\n  Malformed(path: str, detail: str)\n}\n\
         impl Error for LoadError {\n  fn message(self) -> str {\n\
         \x20   return match self {\n\
         \x20     Absent(path)            => \"no file at \\(path)\",\n\
         \x20     Malformed(path, detail) => \"\\(path): \\(detail)\",\n    }\n  }\n}\n\
         struct Refused {\n  who: str\n}\n\
         impl Error for Refused {\n  fn message(self) -> str {\n\
         \x20   return \"\\(self.who) said no\"\n  }\n}\n\
         fn load(path: str) -> (int, error) {\n\
         \x20 if path == \"\" {\n    return _, LoadError.Absent(path: \"<none>\")\n  }\n\
         \x20 if path == \"bad\" {\n\
         \x20   return _, LoadError.Malformed(path: path, detail: \"not JSON\")\n  }\n\
         \x20 return 7, nil\n}\n\
         fn ask(who: str) -> (int, error) {\n\
         \x20 if who == \"nobody\" {\n    return _, Refused{ who: who }\n  }\n\
         \x20 return 1, nil\n}\n\
         fn main() {\n\
         \x20 let (a, aerr) = load(\"\")\n\
         \x20 if aerr != nil {\n    io.print(aerr.message())\n  }\n\
         \x20 let (b, berr) = load(\"bad\")\n\
         \x20 if berr != nil {\n    io.print(berr.message())\n  }\n\
         \x20 let (c, cerr) = load(\"good\")\n\
         \x20 if cerr == nil {\n    io.print(\"ok \\(c)\")\n  }\n\
         \x20 let (d, derr) = ask(\"nobody\")\n\
         \x20 if derr != nil {\n    io.print(derr.message())\n  }\n\
         \x20 let (e, eerr) = ask(\"somebody\")\n\
         \x20 if eerr == nil {\n    io.print(\"allowed \\(e)\")\n  }\n}\n",
    ),
    (
        "events",
        "// Every event comes through one door: a click fills the position, a\n\
         // key press fills the key.\n\
         struct M {\n  n: int\n  from: str\n}\n\
         fn step(m: M, event: int, x: float, y: float, key: str) -> M {\n\
         \x20 if event == 0 {\n    return M{n: m.n + 1, from: \"click at \\(x),\\(y)\"}\n  }\n\
         \x20 if event == 1 {\n\
         \x20   if key == \"r\" {\n      return M{n: 0, from: \"reset\"}\n    }\n\
         \x20   return M{n: m.n + 1, from: \"key \\(key)\"}\n  }\n\
         \x20 return m\n}\n\
         fn main() {\n  var m = M{n: 0, from: \"start\"}\n\
         \x20 m = step(m, 0, 3.0, 4.0, \"\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"+\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"ArrowUp\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"r\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 9, 0.0, 0.0, \"\")\n  io.print(\"\\(m.n) \\(m.from)\")\n}\n",
    ),
    (
        "strings",
        "fn greet(name: str) -> str {\n  return \"hello, \" + name\n}\n\
         fn main() {\n  io.print(greet(\"world\"))\n  io.print(\"a\" + \"b\" + \"c\")\n\
         \x20 io.print(\"x\" == \"x\")\n  io.print(\"x\" == \"y\")\n  io.print(\"x\" != \"y\")\n}\n",
    ),
    (
        "optionals",
        "struct U {\n  name: str\n}\n\
         fn find(id: int) -> Option<U> {\n  if id == 1 {\n    return U{ name: \"ada\" }\n  }\n\
         \x20 return nil\n}\n\
         fn name_of(id: int) -> str {\n  let u = find(id)\n\
         \x20 return if u == nil { \"anon\" } else { u.name }\n}\n\
         fn main() {\n  io.print(name_of(1))\n  io.print(name_of(2))\n\
         \x20 io.print(match find(1) {\n    nil => \"none\",\n    u => u.name,\n  })\n}\n",
    ),
    (
        "optional-primitives",
        "fn maybe(n: int) -> Option<int> {\n  if n > 0 {\n    return n\n  }\n  return nil\n}\n\
         fn main() {\n  let a = maybe(5)\n  io.print(if a == nil { 0 } else { a })\n\
         \x20 let b = maybe(-1)\n  io.print(if b == nil { 0 } else { b })\n}\n",
    ),
    (
        "slices",
        "fn sum(xs: [int]) -> int {\n  var total = 0\n  for x in xs {\n    total = total + x\n  }\n\
         \x20 return total\n}\n\
         fn main() {\n  let xs = [1, 2, 3, 4]\n  io.print(xs.len())\n  io.print(xs[0])\n\
         \x20 io.print(xs[3])\n  io.print(sum(xs))\n}\n",
    ),
    (
        "slice-value-semantics",
        "fn main() {\n  var a = [1, 2]\n  var b = a\n  b.push(3)\n  b[0] = 9\n\
         \x20 io.print(a.len())\n  io.print(a[0])\n  io.print(b.len())\n  io.print(b[0])\n}\n",
    ),
    (
        "slice-get",
        "fn main() {\n  let xs = [10, 20]\n  let a = xs.get(1)\n  let b = xs.get(9)\n\
         \x20 io.print(if a == nil { -1 } else { a })\n\
         \x20 io.print(if b == nil { -1 } else { b })\n}\n",
    ),
    // Every edge of `xs[a..b]` in one program, because the whole question
    // about a range is what it does past the ends and the three backends
    // clamp in three different pieces of code.
    (
        "slice-range",
        "fn show(xs: [int]) -> str {\n  var out = \"\"\n  for x in xs {\n\
         \x20   out = out + \"\\(x),\"\n  }\n  return \"[\" + out + \"]\"\n}\n\
         fn main() {\n  let xs = [1, 2, 3, 4, 5]\n\
         \x20 io.print(show(xs[1..3]))\n\
         \x20 io.print(show(xs[0..xs.len()]))\n\
         \x20 io.print(show(xs[2..100]))\n\
         \x20 io.print(show(xs[-5..2]))\n\
         \x20 io.print(show(xs[4..1]))\n\
         \x20 io.print(show(xs[1..=3]))\n\
         \x20 io.print(show(xs[0..0]))\n\
         \x20 io.print(show(xs[9..9]))\n}\n",
    ),
    // An error carrying its value and its cause: the type recovered by name
    // through three layers of wrapping, which is the whole point of the
    // representation. Every backend keeps the tag in a different place.
    (
        "error-carries-its-value",
        "use std/errors\n\
         pub struct NotFound {\n  pub path: str\n}\n\
         impl Error for NotFound {\n         \x20 fn message(self) -> str {\n    return \"no such file: \\(self.path)\"\n  }\n         }\n\
         pub struct Denied {\n  pub who: str\n}\n\
         impl Error for Denied {\n         \x20 fn message(self) -> str {\n    return \"\\(self.who) may not\"\n  }\n         }\n\
         fn read() -> (int, error) {\n         \x20 return _, NotFound{ path: \"app.toml\" }\n         }\n\
         fn load() -> (int, error) {\n         \x20 let (v, err) = read()\n  check errors.wrap(err, \"loading config\")\n         \x20 return v, nil\n         }\n\
         fn start() -> (int, error) {\n         \x20 let (v, err) = load()\n  check errors.wrap(err, \"starting up\")\n         \x20 return v, nil\n         }\n\
         fn main() {\n         \x20 let (v, err) = start()\n         \x20 if err == nil {\n    io.print(\"ok\")\n    return\n  }\n         \x20 io.print(err.message())\n         \x20 io.print(join(errors.chain(err), \" <- \"))\n         \x20 let r = errors.root(err)\n         \x20 io.print(errors.message_or(r, \"none\"))\n         \x20 io.print(\"\\(NotFound.is(r))\")\n         \x20 io.print(\"\\(Denied.is(r))\")\n         \x20 io.print(\"\\(NotFound.is(err))\")\n         \x20 let nf = NotFound.as(r)\n         \x20 io.print(if nf == nil { \"none\" } else { nf.path })\n         \x20 let d = Denied.as(r)\n         \x20 io.print(if d == nil { \"none\" } else { d.who })\n         }\n",
    ),
    // `check` in a function that answers with a bare `error` — no value slot
    // to fill, so the propagation is the error rather than a pair.
    (
        "check-bare-error-return",
        "fn might(n: int) -> (int, error) {\n\
         \x20 if n < 0 {\n    return _, errors.new(\"negative\")\n  }\n  return n * 2, nil\n}\n\
         fn only(n: int) -> error {\n  let (v, err) = might(n)\n  check err\n\
         \x20 io.print(\"got \\(v)\")\n  return nil\n}\n\
         fn main() {\n  let a = only(3)\n\
         \x20 io.print(if a == nil { \"ok\" } else { a.message() })\n\
         \x20 let b = only(-1)\n\
         \x20 io.print(if b == nil { \"ok\" } else { b.message() })\n}\n",
    ),
    // `for (a, b) in …` over a slice of pairs, which is what `enumerate` and
    // `zip` both answer with.
    (
        "for-pairs-over-slice",
        "fn main() {\n  for (i, name) in enumerate([\"ay\", \"bee\"]) {\n\
         \x20   io.print(\"\\(i). \\(name)\")\n  }\n\
         \x20 for (a, b) in zip([1, 2, 3], [\"x\", \"y\"]) {\n\
         \x20   io.print(\"\\(a)\\(b)\")\n  }\n\
         \x20 for (_, s) in enumerate([\"only\"]) {\n    io.print(s)\n  }\n}\n",
    ),
    // A range over references, where the copy has to keep the element kind in
    // the header or the collector cannot trace what it just made.
    (
        "slice-range-refs",
        "fn main() {\n  let names = [\"ay\", \"bee\", \"cee\", \"dee\"]\n\
         \x20 io.print(join(names[1..3], \"-\"))\n\
         \x20 io.print(join(names[2..99], \"-\"))\n\
         \x20 io.print(join(names[0..1], \"-\"))\n\
         \x20 let s = \"hello world\"\n\
         \x20 io.print(s[0..5])\n  io.print(s[6..100])\n  io.print(s[0..=4])\n}\n",
    ),
    // A range index may leave out either end. The parser fills in `0` and the
    // largest `int`, and relies on every backend clamping a window to the
    // sequence — so the largest `int` as an end, on slices of values, of
    // references and on strings, is the edge to check.
    (
        "open-slice-range",
        "fn show(xs: [int]) -> str {\n  var out = \"\"\n  for x in xs {\n\
         \x20   out = out + \"\\(x),\"\n  }\n  return \"[\" + out + \"]\"\n}\n\
         fn main() {\n  let xs = [1, 2, 3, 4, 5]\n\
         \x20 io.print(show(xs[2..]))\n  io.print(show(xs[..2]))\n  io.print(show(xs[..]))\n\
         \x20 io.print(show(xs[..=1]))\n  io.print(show(xs[9..]))\n  io.print(show(xs[-3..]))\n\
         \x20 io.print(show(xs[..0]))\n\
         \x20 let names = [\"ay\", \"bee\", \"cee\"]\n\
         \x20 io.print(join(names[1..], \"-\"))\n  io.print(join(names[..1], \"-\"))\n\
         \x20 let s = \"hello world\"\n\
         \x20 io.print(s[6..])\n  io.print(s[..5])\n  io.print(s[..])\n  io.print(s[..=0])\n\
         \x20 io.print(s[20..] == \"\")\n}\n",
    ),
    // What the parser decides, run: `&`, `^` and `|` share a level (§5.1), a
    // name after `as` takes no type arguments, `t.0.1` is two indexes,
    // `Option<int>=` splits, a line ending in `>` continues, and a block
    // string with a hole is dedented like one without.
    (
        "parser-decisions",
        "fn main() {\n  let a = 1\n  let b = 2\n  let c = 4\n\
         \x20 io.print(a | b & c)\n  io.print(1 | 6 ^ 3)\n  io.print(6 & 3 | 8)\n\
         \x20 io.print(6 & 3 == 2)\n\
         \x20 let f = 2.5\n  if f as int < 3 {\n    io.print(\"less\")\n  }\n\
         \x20 let t = ((1, 2), 3)\n  io.print(t.0.1)\n  io.print(t.1)\n\
         \x20 let o: Option<int>= nil\n  io.print(o == nil)\n\
         \x20 let big = a >\n    b\n  io.print(big)\n\
         \x20 let n = 7\n\
         \x20 let block = \"\"\"\n      first \\(n)\n        second \\(n + 1)\n      \"\"\"\n\
         \x20 io.print(\"[\\(block)]\")\n}\n",
    ),
    (
        "error-handling",
        "fn divide(a: int, b: int) -> (int, error) {\n  if b == 0 {\n\
         \x20   return _, errors.new(\"division by zero\")\n  }\n  return a / b, nil\n}\n\
         fn ratio(a: int, b: int) -> (int, error) {\n  let (q, err) = divide(a, b)\n\
         \x20 check err\n  let (s, err) = divide(q * 1000, 10)\n  check err\n  return s, nil\n}\n\
         fn report(a: int, b: int) {\n  let (r, err) = ratio(a, b)\n  if err != nil {\n\
         \x20   io.print(\"failed: \" + err.message())\n  } else {\n    io.print(r)\n  }\n}\n\
         fn main() {\n  report(10, 2)\n  report(10, 0)\n}\n",
    ),
    (
        "tuples",
        "fn pair() -> (int, str) {\n  return (7, \"seven\")\n}\n\
         fn main() {\n  io.print(match pair() {\n    (0, s) => \"zero\",\n    (n, s) => s,\n  })\n\
         \x20 let t = (1, (2, 3))\n  io.print(match t {\n    (a, (b, c)) => a + b + c,\n  })\n\
         }\n",
    ),
    (
        "maps",
        "fn main() {\n  let m = {\"a\": 1, \"b\": 2}\n  io.print(m.len())\n\
         \x20 let a = m[\"a\"]\n  io.print(if a == nil { -1 } else { a })\n\
         \x20 let z = m[\"zz\"]\n  io.print(if z == nil { -1 } else { z })\n}\n",
    ),
    (
        "display",
        "struct P {\n  x: int\n  y: int\n}\n\
         impl Display for P {\n  fn show(self) -> str {\n\
         \x20   return \"(\\(self.x), \\(self.y))\"\n  }\n}\n\
         enum S {\n  Dot\n  Circle(int)\n}\n\
         impl Display for S {\n  fn show(self) -> str {\n\
         \x20   return match self {\n      Dot => \"dot\"\n\
         \x20     Circle(r) => \"circle \\(r)\"\n    }\n  }\n}\n\
         fn main() {\n  let p = P{x: 3, y: 4}\n\
         \x20 io.print(p)\n  io.print(\"at \\(p)\")\n\
         \x20 io.print(S.Dot)\n  io.print(S.Circle(9))\n\
         \x20 io.print(\"\\(S.Dot) and \\(P{x: 0, y: 0})\")\n\
         \x20 io.print(join(map([p, P{x: 1, y: 1}], |q: P| q.show()), \" \"))\n\
         \x20 io.print(p.show() == \"(3, 4)\")\n}\n",
    ),
    (
        "error-types",
        "// A concrete type saying what its own failure means. Returning one\n\
         // where an `error` is expected converts it at that point, which is an\n\
         // ordinary call to `message` in the IR — so all three backends lower\n\
         // it without knowing the trait exists.\n\
         enum LoadError {\n  Absent(path: str)\n  Malformed(path: str, detail: str)\n}\n\
         impl Error for LoadError {\n  fn message(self) -> str {\n\
         \x20   return match self {\n\
         \x20     Absent(path)            => \"no file at \\(path)\",\n\
         \x20     Malformed(path, detail) => \"\\(path): \\(detail)\",\n    }\n  }\n}\n\
         struct Refused {\n  who: str\n}\n\
         impl Error for Refused {\n  fn message(self) -> str {\n\
         \x20   return \"\\(self.who) said no\"\n  }\n}\n\
         fn load(path: str) -> (int, error) {\n\
         \x20 if path == \"\" {\n    return _, LoadError.Absent(path: \"<none>\")\n  }\n\
         \x20 if path == \"bad\" {\n\
         \x20   return _, LoadError.Malformed(path: path, detail: \"not JSON\")\n  }\n\
         \x20 return 7, nil\n}\n\
         fn ask(who: str) -> (int, error) {\n\
         \x20 if who == \"nobody\" {\n    return _, Refused{ who: who }\n  }\n\
         \x20 return 1, nil\n}\n\
         fn main() {\n\
         \x20 let (a, aerr) = load(\"\")\n\
         \x20 if aerr != nil {\n    io.print(aerr.message())\n  }\n\
         \x20 let (b, berr) = load(\"bad\")\n\
         \x20 if berr != nil {\n    io.print(berr.message())\n  }\n\
         \x20 let (c, cerr) = load(\"good\")\n\
         \x20 if cerr == nil {\n    io.print(\"ok \\(c)\")\n  }\n\
         \x20 let (d, derr) = ask(\"nobody\")\n\
         \x20 if derr != nil {\n    io.print(derr.message())\n  }\n\
         \x20 let (e, eerr) = ask(\"somebody\")\n\
         \x20 if eerr == nil {\n    io.print(\"allowed \\(e)\")\n  }\n}\n",
    ),
    (
        "events",
        "// Every event comes through one door: a click fills the position, a\n\
         // key press fills the key.\n\
         struct M {\n  n: int\n  from: str\n}\n\
         fn step(m: M, event: int, x: float, y: float, key: str) -> M {\n\
         \x20 if event == 0 {\n    return M{n: m.n + 1, from: \"click at \\(x),\\(y)\"}\n  }\n\
         \x20 if event == 1 {\n\
         \x20   if key == \"r\" {\n      return M{n: 0, from: \"reset\"}\n    }\n\
         \x20   return M{n: m.n + 1, from: \"key \\(key)\"}\n  }\n\
         \x20 return m\n}\n\
         fn main() {\n  var m = M{n: 0, from: \"start\"}\n\
         \x20 m = step(m, 0, 3.0, 4.0, \"\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"+\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"ArrowUp\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 1, 0.0, 0.0, \"r\")\n  io.print(\"\\(m.n) \\(m.from)\")\n\
         \x20 m = step(m, 9, 0.0, 0.0, \"\")\n  io.print(\"\\(m.n) \\(m.from)\")\n}\n",
    ),
    (
        "strings",
        "fn main() {\n  let s = \"  hello world  \"\n\
         \x20 io.print(s.len())\n  io.print(s.trim())\n  io.print(s.trim().len())\n\
         \x20 io.print(s.index_of(\"world\"))\n  io.print(s.index_of(\"nope\"))\n\
         \x20 io.print(s.slice(2, 7))\n  io.print(s.slice(0, 100))\n\
         \x20 io.print(s.slice(5, 2) == \"\")\n  io.print(s.slice(-3, 4))\n\
         \x20 let u = \"héllo日本\"\n  io.print(u.len())\n  io.print(u.slice(1, 3))\n\
         \x20 io.print(u.index_of(\"日\"))\n  io.print(u.index_of(\"é\"))\n}\n",
    ),
    (
        "string-library",
        "fn main() {\n\
         \x20 io.print(contains(\"hello\", \"ell\"))\n  io.print(contains(\"hello\", \"z\"))\n\
         \x20 io.print(starts_with(\"hello\", \"he\"))\n  io.print(starts_with(\"he\", \"hello\"))\n\
         \x20 io.print(ends_with(\"hello\", \"lo\"))\n  io.print(ends_with(\"hello\", \"hello\"))\n\
         \x20 let parts = split(\"a,b,,c\", \",\")\n  io.print(parts.len())\n\
         \x20 io.print(parts[2] == \"\")\n  io.print(join(parts, \"-\"))\n\
         \x20 io.print(replace(\"a.b.c\", \".\", \"/\"))\n\
         \x20 io.print(words(\"  the   quick  brown \").len())\n\
         \x20 io.print(join(words(\" one  two \"), \"+\"))\n\
         \x20 io.print(split(\"nosep\", \",\").len())\n  io.print(split(\"x\", \"\").len())\n}\n",
    ),
    (
        "text-measurement",
        "// Measurement is a host call, so a runtime with no font answers with a\n\
         // nominal advance — the same one on both backends, which is what keeps\n\
         // a layout comparable under test.\n\
         fn main() {\n\
         \x20 io.print(text.width(\"\"))\n  io.print(text.width(\"abc\"))\n\
         \x20 io.print(text.width(\"héllo\"))\n  io.print(text.width(\"日本語\"))\n\
         \x20 let s = \"ab\" + \"cd\"\n  io.print(text.width(s))\n\
         \x20 io.print(text.width(s) > text.width(\"a\"))\n\
         \x20 io.print(text.height())\n  io.print(text.height() > 0.0)\n}\n",
    ),
    (
        "guard-clause-narrowing",
        "// `if x == nil { return }` leaves only the path where `x` is there,\n\
         // so it reads as a `T` for the rest of the block.\n\
         fn unwrap_or(v: Option<int>, fallback: int) -> int {\n\
         \x20 if v == nil {\n    return fallback\n  }\n  return v + 1\n}\n\
         fn describe(s: Option<str>) -> str {\n\
         \x20 if s == nil {\n    return \"none\"\n  }\n  return s + \"!\"\n}\n\
         fn nested(a: Option<int>, b: Option<int>) -> int {\n\
         \x20 if a == nil {\n    return -1\n  }\n\
         \x20 if b == nil {\n    return a\n  }\n  return a + b\n}\n\
         fn scoped(v: Option<int>) -> int {\n\
         \x20 // The narrowing ends with the block that guarded it.\n\
         \x20 for i in 0..1 {\n    if v == nil {\n      return -1\n    }\n\
         \x20   io.print(v)\n  }\n  return 0\n}\n\
         fn main() {\n\
         \x20 io.print(unwrap_or(41, 0))\n  io.print(unwrap_or(nil, 7))\n\
         \x20 io.print(describe(\"hi\"))\n  io.print(describe(nil))\n\
         \x20 io.print(nested(1, 2))\n  io.print(nested(1, nil))\n  io.print(nested(nil, 2))\n\
         \x20 io.print(scoped(5))\n  io.print(scoped(nil))\n}\n",
    ),
    (
        "drawing",
        "// The drawing boundary is two calls wide, and both backends describe\n\
         // each call the same way — which is what lets a layout be compared\n\
         // without a browser.\n\
         fn main() {\n\
         \x20 draw.rect(0.0, 0.0, 640.0, 360.0, 0x14161a)\n\
         \x20 draw.text(12.0, 12.0, \"Kite\", 0xf5f7fa)\n\
         \x20 var y = 40.0\n\
         \x20 for i in 0..3 {\n\
         \x20   draw.rect(0.0, y, 160.0, 24.0, 0x1a1e25)\n\
         \x20   draw.text(8.0, y, \"row \\(i)\", 0xc9d1dc)\n\
         \x20   y = y + 24.0\n  }\n\
         \x20 draw.rect(-1.5, 0.25, 0.0, 1.0e2, 0xffffff)\n}\n",
    ),
    (
        "ambiguous-variant-names",
        "// Two enums with the same variant names. Which one an unqualified\n\
         // pattern means is decided by the scrutinee.\n\
         enum Mode {\n  Slow\n  Fast\n}\n\
         enum Speed {\n  Slow\n  Quick\n}\n\
         fn mode(m: Mode) -> int {\n  return match m {\n    Slow => 1\n    Fast => 2\n  }\n}\n\
         fn speed(s: Speed) -> int {\n  return match s {\n    Slow => 10\n    Quick => 20\n  }\n}\n\
         fn main() {\n  io.print(mode(Mode.Slow))\n  io.print(mode(Mode.Fast))\n\
         \x20 io.print(speed(Speed.Slow))\n  io.print(speed(Speed.Quick))\n}\n",
    ),
    (
        "subsumption-everywhere",
        "struct Holder {\n  var slot: Option<int>\n  tag: Option<str>\n}\n\
         fn main() {\n\
         \x20 // A `T` written where an `Option<T>` is wanted wraps, in a field,\n\
         \x20 // in an assignment, and in a field assignment alike.\n\
         \x20 var h = Holder{slot: 1, tag: \"x\"}\n\
         \x20 io.print(or_else(h.slot, -1))\n  io.print(or_else(h.tag, \"none\"))\n\
         \x20 h.slot = 9\n  io.print(or_else(h.slot, -1))\n\
         \x20 var v: Option<int> = nil\n  io.print(or_else(v, -1))\n\
         \x20 v = 7\n  io.print(or_else(v, -1))\n}\n",
    ),
    (
        "prelude",
        "fn main() {\n  let xs = [5, 1, 9, 3, 7]\n\
         \x20 io.print(sum(xs))\n  io.print(count(xs, |n: int| n > 4))\n\
         \x20 io.print(filter(xs, |n: int| n > 4).len())\n\
         \x20 io.print(any(xs, |n: int| n == 9))\n  io.print(all(xs, |n: int| n > 0))\n\
         \x20 io.print(map(xs, |n: int| n * 2).len())\n\
         \x20 io.print(fold(xs, 0, |a: int, n: int| a + n))\n\
         \x20 io.print(fold(map(xs, |n: int| \"x\"), \"\", |a: str, s: str| a + s))\n\
         \x20 io.print(abs(-12))\n  io.print(min(3, 8))\n  io.print(max(3, 8))\n\
         \x20 io.print(clamp(99, 0, 10))\n\
         \x20 io.print(approx_eq(0.1 + 0.2, 0.3, 0.0001))\n  io.print(divides(9, 3))\n\
         \x20 io.print(or_else(first(xs), -1))\n\
         \x20 let empty: [int] = []\n  io.print(or_else(first(empty), -1))\n\
         \x20 io.print(or_else(last(xs), -1))\n\
         \x20 io.print(reversed(xs)[0])\n  io.print(concat(xs, xs).len())\n\
         \x20 io.print(take(xs, 2).len())\n  io.print(drop(xs, 2).len())\n\
         \x20 io.print(or_else(find(xs, |n: int| n > 6), -1))\n\
         \x20 io.print(is_some(find(xs, |n: int| n > 100)))\n}\n",
    ),
    (
        "prelude-shadowing",
        "// A program's own definition wins over the prelude's.\n\
         fn sum(items: [int]) -> int {\n  return 999\n}\n\
         fn main() {\n  io.print(sum([1, 2, 3]))\n  io.print(abs(-4))\n}\n",
    ),
    (
        "generic-methods",
        "struct Stack<T> {\n  items: [T]\n}\n\
         impl<T> Stack<T> {\n\
         \x20 fn of(v: T) -> Stack<T> {\n    return Stack{items: [v]}\n  }\n\
         \x20 fn len(self) -> int {\n    return self.items.len()\n  }\n\
         \x20 fn peek(self) -> Option<T> {\n    if self.items.len() == 0 {\n      return nil\n    }\n\
         \x20   return self.items[self.items.len() - 1]\n  }\n\
         \x20 fn pushed(self, v: T) -> Stack<T> {\n    var next = self.items\n\
         \x20   next.push(v)\n    return Stack{items: next}\n  }\n}\n\
         enum Slot<T> {\n  Empty\n  Full(T)\n}\n\
         impl<T> Slot<T> {\n  fn or(self, fallback: T) -> T {\n\
         \x20   return match self {\n      Empty => fallback\n      Full(v) => v\n    }\n  }\n}\n\
         fn main() {\n\
         \x20 let s = Stack{items: [1, 2, 3]}\n  io.print(s.len())\n\
         \x20 let p = s.peek()\n  io.print(if p == nil { -1 } else { p })\n\
         \x20 io.print(s.pushed(9).len())\n  io.print(s.len())\n\
         \x20 let w = Stack{items: [\"a\"]}\n  io.print(w.pushed(\"b\").len())\n\
         \x20 let q = w.pushed(\"b\").peek()\n  io.print(if q == nil { \"none\" } else { q })\n\
         \x20 let one: Stack<bool> = Stack.of(true)\n  io.print(one.len())\n\
         \x20 let full: Slot<int> = Slot.Full(5)\n  let empty: Slot<int> = Slot.Empty\n\
         \x20 io.print(full.or(-1))\n  io.print(empty.or(-1))\n\
         \x20 let text: Slot<str> = Slot.Full(\"yes\")\n  io.print(text.or(\"no\"))\n}\n",
    ),
    (
        "generic-types",
        "struct Box<T> {\n  value: T\n}\n\
         struct Pair<A, B> {\n  first: A\n  second: B\n}\n\
         struct Tree<T> {\n  label: T\n  children: [Tree<T>]\n}\n\
         enum Res<T, E> {\n  Ok(T)\n  Err(E)\n}\n\
         fn size<T>(t: Tree<T>) -> int {\n  var n = 1\n\
         \x20 for c in t.children {\n    n = n + size(c)\n  }\n  return n\n}\n\
         fn or_else(r: Res<int, str>, fallback: int) -> int {\n\
         \x20 return match r {\n    Ok(v) => v\n    Err(m) => fallback\n  }\n}\n\
         fn main() {\n\
         \x20 io.print(Box{value: 42}.value)\n  io.print(Box{value: \"text\"}.value)\n\
         \x20 let deep: Box<Box<int>> = Box{value: Box{value: 7}}\n\
         \x20 io.print(deep.value.value)\n\
         \x20 let p = Pair{first: 1, second: \"one\"}\n\
         \x20 io.print(\"\\(p.first) is \\(p.second)\")\n\
         \x20 let leaf = Tree{label: 3, children: []}\n\
         \x20 let root = Tree{label: 1, children: [leaf, Tree{label: 2, children: [leaf]}]}\n\
         \x20 io.print(size(root))\n\
         \x20 io.print(root.children[1].children[0].label)\n\
         \x20 io.print(or_else(Res.Ok(5), -1))\n  io.print(or_else(Res.Err(\"no\"), -1))\n\
         \x20 let a: Box<int> = Box{value: 1}\n  let b: Box<int> = Box{value: 1}\n\
         \x20 io.print(a == b)\n  io.print(a == Box{value: 2})\n}\n",
    ),
    (
        "closures",
        "fn apply(f: fn(int) -> int, x: int) -> int {\n  return f(x)\n}\n\
         fn twice(f: fn(int) -> int, x: int) -> int {\n  return f(f(x))\n}\n\
         fn make_adder(n: int) -> fn(int) -> int {\n  return |x: int| x + n\n}\n\
         fn main() {\n\
         \x20 let double = |x: int| x * 2\n  io.print(apply(double, 21))\n\
         \x20 io.print(twice(double, 3))\n\
         \x20 let base = 100\n  io.print(apply(|x: int| x + base, 5))\n\
         \x20 let add5 = make_adder(5)\n  let add9 = make_adder(9)\n\
         \x20 io.print(apply(add5, 1))\n  io.print(apply(add9, 1))\n\
         \x20 io.print(apply(add5, 1))\n\
         \x20 let classify = |n: int| -> str {\n    if n < 0 {\n      return \"neg\"\n    }\n\
         \x20   if n == 0 {\n      return \"zero\"\n    }\n    return \"pos\"\n  }\n\
         \x20 io.print(classify(-3))\n  io.print(classify(0))\n  io.print(classify(9))\n\
         \x20 let outer = |x: int| -> int {\n    let inner = |y: int| y + base\n\
         \x20   return apply(inner, x)\n  }\n  io.print(apply(outer, 7))\n}\n",
    ),
    (
        "closures-in-generics",
        "fn transform<T>(xs: [T], f: fn(T) -> T) -> [T] {\n  var out: [T] = []\n\
         \x20 for x in xs {\n    out.push(f(x))\n  }\n  return out\n}\n\
         fn describe<T>(x: T, show: fn(T) -> str) -> str {\n  return \"v: \" + show(x)\n}\n\
         fn main() {\n\
         \x20 let d = transform([1, 2, 3], |n: int| n * 2)\n\
         \x20 io.print(d.len())\n  io.print(d[0])\n  io.print(d[2])\n\
         \x20 let s = transform([\"a\", \"b\"], |x: str| x + \"!\")\n\
         \x20 io.print(s[0])\n  io.print(s[1])\n\
         \x20 io.print(describe(7, |n: int| \"\\(n)\"))\n\
         \x20 io.print(describe(true, |b: bool| if b { \"yes\" } else { \"no\" }))\n}\n",
    ),
    (
        "generics",
        "fn first<T>(xs: [T]) -> Option<T> {\n  if xs.len() == 0 {\n    return nil\n  }\n\
         \x20 return xs[0]\n}\n\
         fn pair<A, B>(a: A, b: B) -> (A, B) {\n  return (a, b)\n}\n\
         fn count<T>(xs: [T]) -> int {\n  var n = 0\n  for x in xs {\n    n += 1\n  }\n\
         \x20 return n\n}\n\
         fn main() {\n\
         \x20 let a = first([10, 20, 30])\n  io.print(if a == nil { -1 } else { a })\n\
         \x20 let b = first([\"x\", \"y\"])\n  io.print(if b == nil { \"none\" } else { b })\n\
         \x20 let e: [int] = []\n  let c = first(e)\n\
         \x20 io.print(if c == nil { -1 } else { c })\n\
         \x20 match pair(1, \"one\") {\n    (x, y) => io.print(\"\\(x) is \\(y)\")\n  }\n\
         \x20 match pair(true, 2.5) {\n    (x, y) => io.print(\"\\(x) and \\(y)\")\n  }\n\
         \x20 io.print(count([1, 2, 3]))\n  io.print(count([\"a\", \"b\"]))\n\
         \x20 io.print(count([[1], [2], [3], [4]]))\n}\n",
    ),
    (
        "generic-bounds",
        "trait Shape {\n  fn area(self) -> int\n}\n\
         struct Sq {\n  s: int\n}\nstruct Tri {\n  b: int\n  h: int\n}\n\
         impl Shape for Sq {\n  fn area(self) -> int {\n    return self.s * self.s\n  }\n}\n\
         impl Shape for Tri {\n  fn area(self) -> int {\n    return self.b * self.h / 2\n  }\n}\n\
         fn total<T: Shape>(xs: [T]) -> int {\n  var sum = 0\n\
         \x20 for x in xs {\n    sum = sum + x.area()\n  }\n  return sum\n}\n\
         fn biggest<T: Shape>(a: T, b: T) -> int {\n\
         \x20 return if a.area() > b.area() { a.area() } else { b.area() }\n}\n\
         fn main() {\n\
         \x20 io.print(total([Sq{s: 2}, Sq{s: 3}]))\n\
         \x20 io.print(total([Tri{b: 4, h: 6}, Tri{b: 2, h: 2}]))\n\
         \x20 io.print(biggest(Sq{s: 5}, Sq{s: 4}))\n\
         \x20 io.print(biggest(Tri{b: 1, h: 2}, Tri{b: 10, h: 10}))\n}\n",
    ),
    (
        "generics-nested",
        "fn ident<T>(x: T) -> T {\n  return x\n}\n\
         fn twice<T>(x: T) -> [T] {\n  return [ident(x), ident(x)]\n}\n\
         fn depth<T>(xs: [T]) -> int {\n  return xs.len()\n}\n\
         fn main() {\n\
         \x20 io.print(ident(7))\n  io.print(ident(\"s\"))\n  io.print(ident(true))\n\
         \x20 io.print(depth(twice(1)))\n  io.print(depth(twice(\"a\")))\n\
         \x20 io.print(depth(twice(twice(1))))\n}\n",
    ),
    (
        "conversions-at-every-site",
        r#"use std/errors

// A value converted where it is accepted: wrapped into an optional, made
// into a trait object, or turned into an `error` — at every site that takes
// one, including the ones that used to check it without converting it.
trait Show {
    fn show(self) -> str
}

struct P {
    a: int
}

impl Show for P {
    fn show(self) -> str {
        return "P\(self.a)"
    }
}

struct Bad {
    m: str
}

impl Error for Bad {
    fn message(self) -> str {
        return "bad \(self.m)"
    }
}

enum Slot {
    Held(Option<int>)
    Failed(error)
    Empty
}

fn get<T>(x: Option<T>, d: T) -> T {
    if x == nil {
        return d
    }
    return x
}

fn opt(v: Option<int>) -> str {
    if v == nil {
        return "nil"
    }
    return "\(v + 1)"
}

fn describe(s: Slot) -> str {
    return match s {
        Held(v) => opt(v),
        Failed(e) => if e == nil { "no error" } else { e.message() },
        Empty => "empty",
    }
}

fn main() {
    let xs: [Option<int>] = [1, nil, 3]
    io.print("\(opt(xs[0])) \(opt(xs[1])) \(opt(xs[2]))")
    var ys: [Option<int>] = []
    ys.push(5)
    ys.push(nil)
    io.print("\(opt(ys[0])) \(opt(ys[1]))")
    var zs: [Option<int>] = [nil]
    zs[0] = 7
    io.print(opt(zs[0]))
    io.print(describe(Slot.Held(9)))
    io.print(describe(Slot.Failed(Bad{ m: "slot" })))
    io.print(describe(Slot.Empty))
    io.print("\(get(7, 3)) \(get(P{ a: 4 }, P{ a: 5 }).a)")
    var ds: [dyn Show] = [P{ a: 1 }]
    ds.push(P{ a: 2 })
    ds[0] = P{ a: 3 }
    io.print("\(ds[0].show()) \(ds[1].show())")
    var es: [error] = [Bad{ m: "first" }]
    es.push(Bad{ m: "second" })
    let e = es[1]
    if e != nil {
        io.print(e.message())
    }
    let o: Option<int> = nil
    let pick = if xs.len() > 2 { o } else { 4 }
    let back = if xs.len() > 9 { 4 } else { o }
    io.print("\(opt(pick)) \(opt(back))")
    let wrapped = errors.because("outer", Bad{ m: "cause" })
    io.print(join(errors.chain(wrapped), " <- "))
}
"#,
    ),
    (
        "self-and-method-generics",
        r#"// `Self` in a trait, and methods with type parameters of their own — on an
// `impl`, on a trait reached through a bound, and on an associated function.
trait Cmp {
    fn same(self, other: Self) -> bool
    fn pick(self, other: Self) -> Self
}

struct A {
    x: int
}

struct B {
    s: str
}

impl Cmp for A {
    fn same(self, other: A) -> bool {
        return self.x == other.x
    }
    fn pick(self, other: A) -> A {
        return if self.x > other.x { self } else { other }
    }
}

impl Cmp for B {
    fn same(self, other: Self) -> bool {
        return self.s == other.s
    }
    fn pick(self, other: Self) -> Self {
        return self
    }
}

fn all_same<T: Cmp>(xs: [T]) -> bool {
    for x in xs {
        if !x.same(xs[0]) {
            return false
        }
    }
    return true
}

fn best<T: Cmp>(a: T, b: T) -> T {
    return a.pick(b)
}

trait Mapper {
    fn map_to<U>(self, f: fn(int) -> U) -> [U]
}

struct Nums {
    ns: [int]
}

impl Mapper for Nums {
    fn map_to<U>(self, f: fn(int) -> U) -> [U] {
        var out: [U] = []
        for n in self.ns {
            out.push(f(n))
        }
        return out
    }
}

fn twice<M: Mapper>(m: M) -> [str] {
    return m.map_to(|n: int| "\(n)\(n)")
}

struct Box<T> {
    v: T
}

impl<T> Box<T> {
    fn map<U>(self, f: fn(T) -> U) -> Box<U> {
        return Box{ v: f(self.v) }
    }
    fn pair<U>(self, other: U) -> (T, U) {
        return (self.v, other)
    }
    fn of<U>(v: T, u: U) -> Box<(T, U)> {
        return Box{ v: (v, u) }
    }
}

fn main() {
    io.print(all_same([A{ x: 1 }, A{ x: 1 }]))
    io.print(all_same([B{ s: "a" }, B{ s: "b" }]))
    io.print(best(A{ x: 3 }, A{ x: 9 }).x)
    io.print(best(B{ s: "l" }, B{ s: "r" }).s)
    io.print(A{ x: 2 }.same(A{ x: 2 }))
    let t = twice(Nums{ ns: [1, 2] })
    io.print(t[0] + t[1])
    let b = Box{ v: 5 }
    let c = b.map(|x: int| -> str { return "n\(x)" })
    io.print(c.v)
    let d = c.map(|s: str| s.len())
    io.print(d.v)
    let (x, y) = b.pair("p")
    io.print("\(x) \(y)")
    let e = Box.of(1, true)
    let (i, f) = e.v
    io.print("\(i) \(f)")
}
"#,
    ),
    (
        "bounds-through-generics",
        r#"// A bounded parameter meets the same bound; a generic type's `impl` is
// reached through a bound and through a `dyn`; `Display` shows a bounded
// parameter and a trait object; and an optional wrapped in a generic body
// where its parameter is already optional is not wrapped twice.
use std/task

trait Named {
    fn name(self) -> str
}

struct Box<T> {
    v: T
}

impl<T> Named for Box<T> {
    fn name(self) -> str {
        return "box"
    }
}

struct Plain {
    n: int
}

impl Named for Plain {
    fn name(self) -> str {
        return "plain \(self.n)"
    }
}

impl Display for Plain {
    fn show(self) -> str {
        return "P\(self.n)"
    }
}

fn inner<T: Named>(x: T) -> str {
    return x.name()
}

fn outer<T: Named>(x: T) -> str {
    return inner(x)
}

fn show(x: dyn Named) -> str {
    return x.name()
}

fn say<T: Display>(x: T) -> str {
    return "<\(x)>"
}

fn wrap<T>(x: T) -> Option<T> {
    return x
}

async fn count<T: Share>(xs: [T]) -> int {
    let r = await task.parallel(xs, |x: T| -> int { return 1 })
    return r.len()
}

async fn main() {
    io.print(outer(Plain{ n: 1 }))
    io.print(outer(Box{ v: 1 }))
    io.print(show(Box{ v: "s" }))
    io.print(show(Plain{ n: 2 }))
    io.print(Box{ v: true }.name())
    io.print(say(Plain{ n: 3 }))
    let d: dyn Display = Plain{ n: 4 }
    io.print("\(d)")
    io.print(d)
    let a: Option<int> = 5
    let f = wrap(a)
    io.print(if f == nil { -1 } else { f + 1 })
    let none: Option<int> = nil
    let g = wrap(none)
    io.print(if g == nil { -1 } else { g + 1 })
    let n = await count([1, 2, 3])
    io.print(n)
}
"#,
    ),
    (
        "nested-exhaustiveness",
        r#"// Matches exhaustive only through their nested patterns: a variant covered
// by its payloads together, a tuple and a struct covered column by column,
// and an optional enum covered by `nil` and each of its variants.
enum Light {
    On(bool)
    Off
}

enum E {
    A
    B
}

struct S {
    a: bool
    b: bool
}

fn light(l: Light) -> str {
    return match l {
        On(true) => "bright",
        On(false) => "dim",
        Off => "off",
    }
}

fn pair(t: (int, bool)) -> int {
    return match t {
        (0, _) => 1,
        (_, true) => 2,
        (n, false) if n > 100 => 3,
        (_, false) => 4,
    }
}

fn both(s: S) -> int {
    return match s {
        S { a: true, b: true } => 1,
        S { a: false, b } => 2,
        S { a: true, b: false } => 3,
    }
}

fn maybe(o: Option<E>) -> str {
    return match o {
        nil => "none",
        A => "a",
        B => "b",
    }
}

fn main() {
    io.print(light(Light.On(true)) + light(Light.On(false)) + light(Light.Off))
    io.print(pair((0, true)) + pair((5, true)) + pair((500, false)) + pair((5, false)))
    io.print(both(S{ a: true, b: true }) + both(S{ a: false, b: true }) + both(S{ a: true, b: false }))
    let none: Option<E> = nil
    io.print(maybe(none) + maybe(E.A) + maybe(E.B))
}
"#,
    ),
    (
        "payload-patterns-on-optionals",
        r#"// A pattern for a value, written against an optional, matches a present
// value it matches: a literal, a variant, a struct. Testing the optional
// itself read a tag or compared an `int` off a value that might not be
// there, and each backend answered differently.
enum E {
    A
    B
}

struct P {
    x: int
}

fn number(o: Option<int>) -> str {
    return match o {
        nil => "none",
        1 => "one",
        _ => "other",
    }
}

fn word(o: Option<str>) -> str {
    return match o {
        "a" => "A",
        _ => "?",
    }
}

fn which(o: Option<E>) -> str {
    return match o {
        nil => "none",
        A => "a",
        B => "b",
    }
}

fn point(o: Option<P>) -> str {
    return match o {
        P { x: 0 } => "origin",
        P { x } => "at \(x)",
        nil => "nowhere",
    }
}

fn main() {
    let one: Option<int> = 1
    let two: Option<int> = 2
    let no: Option<int> = nil
    io.print(number(one) + " " + number(two) + " " + number(no))
    let a: Option<str> = "a"
    let n: Option<str> = nil
    io.print(word(a) + word(n))
    let none: Option<E> = nil
    io.print(which(none) + which(E.A) + which(E.B))
    let origin: Option<P> = P{ x: 0 }
    let far: Option<P> = P{ x: 7 }
    let gone: Option<P> = nil
    io.print(point(origin) + ", " + point(far) + ", " + point(gone))
}
"#,
    ),
    (
        "optional-equals-its-value",
        r#"// Equality is structural for all types, and a `T` stands wherever an
// `Option<T>` does: `found == 5` asks whether `found` is present and five.
struct P {
    x: int
}

fn find(xs: [int], want: int) -> Option<int> {
    for x in xs {
        if x == want {
            return x
        }
    }
    return nil
}

fn main() {
    let found = find([1, 5, 9], 5)
    let lost = find([1, 5, 9], 4)
    io.print("\(found == 5) \(5 == found) \(found != 6) \(lost == 5) \(lost != 5)")
    let p: Option<P> = P{ x: 1 }
    io.print("\(p == P{ x: 1 }) \(P{ x: 2 } == p)")
    let s: Option<str> = "k"
    io.print("\(s == "k") \(s == "j")")
}
"#,
    ),
    (
        "interpolation",
        "fn main() {\n  let name = \"world\"\n  let n = 42\n  let pi = 2.5\n  let ok = true\n\
         \x20 io.print(\"hello, \\(name)!\")\n\
         \x20 io.print(\"n=\\(n) pi=\\(pi) ok=\\(ok)\")\n\
         \x20 io.print(\"math: \\(n * 2 + 1)\")\n\
         \x20 io.print(\"branch: \\(if n > 10 { \"big\" } else { \"small\" })\")\n\
         \x20 io.print(\"adjacent: \\(name)\\(n)\")\n\
         \x20 io.print(\"whole: \\(3.0)\")\n  io.print(\"none at all\")\n}\n",
    ),
    (
        "interpolation-matches-print",
        "fn main() {\n  var i = -3\n  for i < 3 {\n    io.print(i)\n\
         \x20   io.print(\"\\(i)\")\n    i += 1\n  }\n\
         \x20 io.print(1.5)\n  io.print(\"\\(1.5)\")\n\
         \x20 io.print(4.0)\n  io.print(\"\\(4.0)\")\n\
         \x20 io.print(true)\n  io.print(\"\\(true)\")\n}\n",
    ),
    (
        "structural-equality",
        "struct Point {\n  x: int\n  y: int\n}\n\
         struct Line {\n  a: Point\n  b: Point\n  label: str\n}\n\
         enum Shape {\n  Dot\n  Seg(Point, Point)\n  Named(str)\n}\n\
         fn main() {\n\
         \x20 let p = Point{x: 1, y: 2}\n  let q = Point{x: 1, y: 2}\n\
         \x20 let r = Point{x: 9, y: 2}\n\
         \x20 io.print(p == q)\n  io.print(p == r)\n  io.print(p != r)\n\
         \x20 io.print(Line{a: p, b: r, label: \"one\"} == Line{a: q, b: r, label: \"one\"})\n\
         \x20 io.print(Line{a: p, b: r, label: \"one\"} == Line{a: q, b: r, label: \"two\"})\n\
         \x20 io.print([1, 2, 3] == [1, 2, 3])\n  io.print([1, 2, 3] == [1, 2, 4])\n\
         \x20 io.print([1, 2, 3] == [1, 2])\n\
         \x20 io.print((1, \"a\", true) == (1, \"a\", true))\n\
         \x20 io.print((1, \"a\", true) == (1, \"b\", true))\n\
         \x20 io.print(Shape.Seg(p, r) == Shape.Seg(q, r))\n\
         \x20 io.print(Shape.Seg(p, r) == Shape.Named(\"x\"))\n\
         \x20 io.print(Shape.Dot == Shape.Dot)\n\
         \x20 io.print(Shape.Named(\"x\") == Shape.Named(\"x\"))\n\
         \x20 let o1: Option<Point> = p\n  let o2: Option<Point> = q\n\
         \x20 let o3: Option<Point> = nil\n\
         \x20 io.print(o1 == o2)\n  io.print(o1 == o3)\n\
         \x20 io.print([p, q] == [q, p])\n  io.print([p, q] == [q, r])\n}\n",
    ),
    (
        "trait-objects",
        "trait Shape {\n  fn area(self) -> int\n  fn sides(self) -> int\n}\n\
         struct Circle {\n  r: int\n}\nstruct Square {\n  s: int\n}\n\
         impl Shape for Circle {\n  fn area(self) -> int {\n    return self.r * self.r * 3\n  }\n\
         \x20 fn sides(self) -> int {\n    return 0\n  }\n}\n\
         impl Shape for Square {\n  fn area(self) -> int {\n    return self.s * self.s\n  }\n\
         \x20 fn sides(self) -> int {\n    return 4\n  }\n}\n\
         fn describe(v: dyn Shape) {\n  io.print(v.area())\n  io.print(v.sides())\n}\n\
         fn main() {\n  describe(Circle{r: 2})\n  describe(Square{s: 3})\n\
         \x20 let xs: [dyn Shape] = [Circle{r: 1}, Square{s: 2}, Circle{r: 3}]\n\
         \x20 var sum = 0\n  for x in xs {\n    sum = sum + x.area()\n  }\n  io.print(sum)\n}\n",
    ),
    (
        "trait-objects-with-enums",
        "trait Named {\n  fn tag(self) -> int\n}\n\
         enum Colour {\n  Red\n  Green(int)\n}\nstruct Point {\n  x: int\n}\n\
         impl Named for Colour {\n  fn tag(self) -> int {\n    return match self {\n\
         \x20     Red => 1\n      Green(n) => n\n    }\n  }\n}\n\
         impl Named for Point {\n  fn tag(self) -> int {\n    return self.x\n  }\n}\n\
         fn main() {\n  let xs: [dyn Named] = [Colour.Red, Colour.Green(7), Point{x: 100}]\n\
         \x20 for x in xs {\n    io.print(x.tag())\n  }\n}\n",
    ),
    (
        "trait-default-methods-dispatch",
        "trait Greet {\n  fn name(self) -> str\n\
         \x20 fn hello(self) -> str {\n    return \"hi \" + self.name()\n  }\n}\n\
         struct A {\n  n: int\n}\nstruct B {\n  n: int\n}\n\
         impl Greet for A {\n  fn name(self) -> str {\n    return \"a\"\n  }\n}\n\
         impl Greet for B {\n  fn name(self) -> str {\n    return \"b\"\n  }\n\
         \x20 fn hello(self) -> str {\n    return \"yo b\"\n  }\n}\n\
         fn main() {\n  let xs: [dyn Greet] = [A{n: 1}, B{n: 2}]\n\
         \x20 for x in xs {\n    io.print(x.hello())\n  }\n}\n",
    ),
    (
        "map-writes",
        "fn main() {\n  var m = {\"a\": 1}\n  m[\"b\"] = 2\n  io.print(m.len())\n\
         \x20 m[\"a\"] = 9\n  io.print(m.len())\n  let a = m[\"a\"]\n\
         \x20 io.print(if a == nil { -1 } else { a })\n\
         \x20 var c = m\n  c[\"a\"] = 100\n  let orig = m[\"a\"]\n\
         \x20 io.print(if orig == nil { -1 } else { orig })\n}\n",
    ),
    (
        "tasks-interleave",
        "async fn work(n: int) -> int {\n  io.print(\"start \\(n)\")\n  task.yield()\n\
         \x20 io.print(\"middle \\(n)\")\n  task.yield()\n  io.print(\"end \\(n)\")\n\
         \x20 return n * 10\n}\n\
         async fn main() {\n  let a = work(1)\n  let b = work(2)\n\
         \x20 io.print(\"results \\(await a) \\(await b)\")\n}\n",
    ),
    (
        "tasks-sleep",
        "use std/task\n\
         async fn fetch(name: str, ms: int) -> str {\n\
         \x20 await task.sleep(ms)\n\
         \x20 io.print(\"\\(name) at \\(time.now())\")\n  return name\n}\n\
         async fn main() {\n\
         \x20 let a = fetch(\"a\", 100)\n  let b = fetch(\"b\", 50)\n  let c = fetch(\"c\", 30)\n\
         \x20 let (x, y) = await task.both(a, b)\n\
         \x20 io.print(\"\\(x) \\(y) \\(await c)\")\n\
         \x20 io.print(\"elapsed \\(time.now())\")\n}\n",
    ),
    (
        "tasks-combinators",
        "use std/task\n\
         async fn slow(name: str, ms: int) -> str {\n  await task.sleep(ms)\n  return name\n}\n\
         async fn main() {\n\
         \x20 let all = await task.all([slow(\"a\", 20), slow(\"b\", 10)])\n\
         \x20 io.print(join(all, \",\"))\n\
         \x20 let first = await task.race([slow(\"fast\", 5), slow(\"slow\", 500)])\n\
         \x20 io.print(first)\n\
         \x20 let late = await task.timeout(slow(\"late\", 900), 50)\n\
         \x20 io.print(if late == nil { \"timed out\" } else { late })\n\
         \x20 let inTime = await task.timeout(slow(\"quick\", 5), 50)\n\
         \x20 io.print(if inTime == nil { \"timed out\" } else { inTime })\n}\n",
    ),
    (
        "tasks-in-a-loop",
        "async fn add(a: int, b: int) -> int {\n  task.yield()\n  return a + b\n}\n\
         async fn main() {\n  var total = 0\n  var pending: [Task<int>] = []\n\
         \x20 for i in 0..5 {\n    pending.push(add(i, i))\n  }\n\
         \x20 for t in pending {\n    total = total + await t\n  }\n\
         \x20 io.print(total)\n}\n",
    ),
    (
        // Identity is the one thing three very different representations —
        // WasmGC's `ref.eq`, a native pointer compare, and `Rc::ptr_eq` —
        // could each answer plausibly and differently. The last two lines are
        // the frame-loop rule: a model handed back unchanged is the same cell,
        // and a rebuilt one is not, however equal its fields.
        "ptr-same",
        "struct Model {\n  count: int\n}\n\
         enum Msg {\n  Tick\n  Set(int)\n}\n\
         fn step(m: Model, grow: bool) -> Model {\n\
         \x20 if grow {\n    return Model{ count: m.count + 1 }\n  }\n  return m\n}\n\
         fn main() {\n\
         \x20 let a = Model{ count: 1 }\n  let b = Model{ count: 1 }\n\
         \x20 io.print(a == b)\n  io.print(ptr.same(a, b))\n\
         \x20 let c = a\n  io.print(ptr.same(a, c))\n\
         \x20 let m = Msg.Set(3)\n\
         \x20 io.print(ptr.same(m, m))\n  io.print(ptr.same(m, Msg.Set(3)))\n\
         \x20 let t = {\"a\": 1}\n\
         \x20 io.print(ptr.same(t, t))\n  io.print(ptr.same(t, {\"a\": 1}))\n\
         \x20 io.print(ptr.same(a, step(a, false)))\n\
         \x20 io.print(ptr.same(a, step(a, true)))\n}\n",
    ),
    (
        "tuple-bindings",
        "fn pair() -> (int, str) {\n  return (7, \"seven\")\n}\n\
         fn main() {\n  let (n, name) = pair()\n  io.print(n)\n  io.print(name)\n\
         \x20 let (a, _) = pair()\n  io.print(a)\n}\n",
    ),
    (
        "defer",
        "fn close(what: str) {\n  io.print(\"closed \\(what)\")\n}\n\
         fn work(fail: bool) -> (int, error) {\n  io.print(\"open a\")\n\
         \x20 defer close(\"a\")\n\
         \x20 if fail {\n    return _, errors.new(\"no\")\n  }\n\
         \x20 io.print(\"open b\")\n  defer close(\"b\")\n  return 1, nil\n}\n\
         fn main() {\n  let (n, err) = work(false)\n  if err != nil {\n    io.print(\"?\")\n    return\n  }\n\
         \x20 io.print(n)\n  let (m, e) = work(true)\n\
         \x20 io.print(if e == nil { \"?\" } else { e.message() })\n}\n",
    ),
    (
        "require",
        "fn half(n: int) -> int {\n  require(n % 2 == 0, \"needs an even number\")\n\
         \x20 return n / 2\n}\n\
         fn main() {\n  io.print(half(10))\n  assert(half(4) == 2, \"four halves to two\")\n\
         \x20 io.print(\"claims held\")\n}\n",
    ),
    (
        "map-iteration",
        "fn main() {\n  var m = {\"a\": 1, \"b\": 2}\n  m[\"c\"] = 3\n\
         \x20 for (k, v) in m {\n    io.print(\"\\(k) -> \\(v)\")\n  }\n\
         \x20 io.print(join(m.keys(), \",\"))\n  io.print(sum(m.values()))\n\
         \x20 var total = 0\n  for (_, v) in m {\n    total = total + v\n  }\n\
         \x20 io.print(total)\n}\n",
    ),
    (
        "functions-as-values",
        "fn double(n: int) -> int {\n  return n * 2\n}\n\
         struct Op {\n  name: str\n  apply: fn(int) -> int\n}\n\
         fn main() {\n  let op = Op{name: \"double\", apply: double}\n\
         \x20 io.print(\"\\(op.name) \\(op.apply(21))\")\n\
         \x20 io.print(map([1, 2, 3], double) == [2, 4, 6])\n}\n",
    ),
    (
        "evaluation-order",
        "fn step(n: int) -> int {\n  io.print(n)\n  return n\n}\nfn main() {\n  let x = step(1) + step(2)\n  io.print(x)\n}\n",
    ),
    // A derived body is generated Kite and nothing downstream knows it was
    // generated, so this is here for the same reason everything else is: if
    // the two backends disagreed about it they would be disagreeing about
    // ordinary code, and this is where that is found.
    (
        "derived-debug-and-hash",
        "@derive(Debug, Hash)\n\
         enum Shape {\n  Dot\n  Rect(int, int)\n  Named(label: str, size: float)\n}\n\
         @derive(Debug, Hash)\n\
         struct Scene {\n  title: str\n  shapes: [Shape]\n  counts: {str: int}\n  note: Option<str>\n}\n\
         fn main() {\n\
         \x20 let s = Scene{\n\
         \x20   title: \"one\",\n\
         \x20   shapes: [Shape.Dot, Shape.Rect(2, 3), Shape.Named(label: \"n\", size: 1.5)],\n\
         \x20   counts: {\"k\": 2},\n\
         \x20   note: nil,\n\
         \x20 }\n\
         \x20 io.print(s.debug())\n\
         \x20 io.print(s.hash())\n\
         \x20 io.print(Scene{ title: \"one\", shapes: s.shapes, counts: s.counts, note: \"x\" }.debug())\n\
         \x20 io.print(Shape.Rect(2, 3).hash() == Shape.Rect(2, 3).hash())\n\
         \x20 io.print(Shape.Rect(2, 3).hash() == Shape.Rect(3, 2).hash())\n}\n",
    ),
    (
        "derived-json-round-trip",
        "use std/json\n\
         @derive(Encode, Decode, Debug)\n\
         enum Colour {\n  Red\n  Rgb(int, int, int)\n}\n\
         @derive(Encode, Decode, Debug)\n\
         struct Palette {\n  name: str\n  colours: [Colour]\n  ratio: float\n  tag: Option<str>\n}\n\
         fn round(p: Palette) -> (Palette, error) {\n\
         \x20 let text = json.stringify(p.encode())\n\
         \x20 io.print(text)\n\
         \x20 let (doc, err) = json.parse(text)\n\
         \x20 check err\n\
         \x20 return Palette.decode(doc)\n}\n\
         fn main() {\n\
         \x20 let p = Palette{ name: \"warm\", colours: [Colour.Red, Colour.Rgb(1, 2, 3)], ratio: 0.5, tag: nil }\n\
         \x20 let (back, err) = round(p)\n\
         \x20 if err != nil {\n    io.print(err.message())\n    return\n  }\n\
         \x20 io.print(back.debug())\n\
         \x20 let (bad, berr) = Palette.decode(json.Json.Null)\n\
         \x20 io.print(if berr == nil { \"?\" } else { berr.message() })\n}\n",
    ),
    (
        "code-at-reads-a-character",
        "fn main() {\n  let s = \"h\\u{e9}llo\"\n  io.print(s.code_at(0))\n  io.print(s.code_at(1))\n\
         \x20 io.print(s.code_at(99))\n  io.print(hash_str(\"abc\") == hash_str(\"abc\"))\n\
         \x20 io.print(hash_str(\"abc\") == hash_str(\"abd\"))\n}\n",
    ),
];

/// Programs pinning down what the middle of the compiler — HIR, MIR and the
/// bytecode it becomes — promises every backend. Each one is here because the
/// three once disagreed about it, or all three agreed on the wrong answer.
const MIDDLE_END: &[(&str, &str)] = &[
    // The first struct a program declares once had type tag zero, which is
    // also the tag of an error carrying nothing and of a nil error. So a plain
    // `errors.new` claimed to be a `NotFound`, and `NotFound.as` of it handed
    // back a value that was never there — a segfault natively, an illegal
    // cast on Wasm.
    (
        "error-tags-are-never-zero",
        "\
struct NotFound {
  id: int
}

impl Error for NotFound {
  fn message(self) -> str { return \"not found \\(self.id)\" }
}

enum Busy {
  Retry(after: int)
}

impl Error for Busy {
  fn message(self) -> str { return \"busy\" }
}

fn main() {
  let plain = errors.new(\"disk on fire\")
  io.print(NotFound.is(plain))
  io.print(NotFound.as(plain) == nil)
  io.print(Busy.is(plain))
  let none: error = nil
  io.print(NotFound.is(none))
  io.print(NotFound.as(none) == nil)
  io.print(Busy.is(none))
  let nf: error = NotFound{ id: 3 }
  io.print(NotFound.is(nf))
  io.print(Busy.is(nf))
  let got = NotFound.as(nf)
  if got != nil {
    io.print(got.id)
  }
  let busy: error = Busy.Retry(after: 5)
  io.print(Busy.is(busy))
  io.print(NotFound.is(busy))
}
",
    ),
    // `==` on a recursive type recursed through the type itself until the
    // checker's stack ran out, and `==` on a map was refused outright although
    // section 5.2 defines it. Maps compare entry by entry in insertion order.
    (
        "equality-on-recursive-types-and-maps",
        "\
enum List {
  Cons(h: int, t: List)
  Empty
}

struct Node {
  label: int
  children: [Node]
}

enum Json {
  Null
  Num(float)
  Text(str)
  Arr([Json])
  Obj({str: Json})
}

struct Inventory {
  name: str
  stock: {str: int}
}

fn main() {
  let a = Cons(1, Cons(2, Empty))
  io.print(a == Cons(1, Cons(2, Empty)))
  io.print(a == Cons(1, Empty))
  let n = Node{ label: 1, children: [Node{ label: 2, children: [] }] }
  io.print(n == Node{ label: 1, children: [Node{ label: 2, children: [] }] })
  io.print(n != Node{ label: 1, children: [] })
  let doc = Json.Obj({\"k\": Json.Arr([Json.Num(1.5), Json.Null]), \"t\": Json.Text(\"x\")})
  io.print(doc == Json.Obj({\"k\": Json.Arr([Json.Num(1.5), Json.Null]), \"t\": Json.Text(\"x\")}))
  io.print(doc == Json.Obj({\"k\": Json.Arr([Json.Num(1.5)]), \"t\": Json.Text(\"x\")}))
  let x = {\"a\": 1, \"b\": 2}
  io.print(x == {\"a\": 1, \"b\": 2})
  io.print(x == {\"b\": 2, \"a\": 1})
  io.print(x == {\"a\": 1, \"b\": 3})
  io.print(x != {\"a\": 1})
  let i = Inventory{ name: \"shop\", stock: x }
  io.print(i == Inventory{ name: \"shop\", stock: {\"a\": 1, \"b\": 2} })
  io.print(i == Inventory{ name: \"shop\", stock: {\"b\": 2, \"a\": 1} })
  let nested = {\"k\": [1, 2]}
  io.print(nested == {\"k\": [1, 2]})
  io.print(nested == {\"k\": [1]})
}
",
    ),
    // A literal, variant, struct or tuple pattern against an optional was
    // compared with the optional itself — `Option<int>` against `4`. The VM's
    // untyped registers hid it; natively it matched nothing, and Wasm refused
    // the module.
    (
        "match-on-an-optional-tests-its-payload",
        "\
enum Shape {
  Circle(r: int)
  Square(s: int)
}

struct P {
  x: int
  y: int
}

fn shape(o: Option<Shape>) -> str {
  return match o {
    nil => \"none\",
    Circle(r) => \"circle \\(r)\",
    Square(s) => \"square \\(s)\",
    _ => \"other\",
  }
}

fn point(o: Option<P>) -> str {
  return match o {
    nil => \"nowhere\",
    P{ x: 0, y } => \"on y at \\(y)\",
    P{ x, y } => \"at \\(x),\\(y)\",
    _ => \"other\",
  }
}

fn flag(o: Option<bool>) -> str {
  return match o {
    nil => \"unset\",
    true => \"on\",
    false => \"off\",
    _ => \"other\",
  }
}

fn count(o: Option<int>) -> str {
  return match o {
    nil => \"none\",
    4 => \"four\",
    1..=3 => \"few\",
    n => \"many \\(n)\",
  }
}

fn word(o: Option<str>) -> str {
  return match o {
    nil => \"none\",
    \"q\" => \"queue\",
    s => s,
  }
}

fn tagged(t: (Option<int>, str)) -> str {
  return match t {
    (nil, s) => \"nil \\(s)\",
    (4, \"x\") => \"four x\",
    (n, s) => s,
  }
}

fn main() {
  io.print(shape(nil))
  io.print(shape(Circle(r: 2)))
  io.print(shape(Square(s: 3)))
  io.print(point(nil))
  io.print(point(P{ x: 0, y: 4 }))
  io.print(point(P{ x: 1, y: 4 }))
  io.print(flag(nil))
  io.print(flag(true))
  io.print(flag(false))
  io.print(count(nil))
  io.print(count(4))
  io.print(count(2))
  io.print(count(9))
  io.print(word(nil))
  io.print(word(\"q\"))
  io.print(word(\"z\"))
  io.print(tagged((nil, \"a\")))
  io.print(tagged((4, \"x\")))
  io.print(tagged((4, \"y\")))
}
",
    ),
    // `for i in a..=max` incremented past the bound before testing it, which
    // overflows at `int`'s maximum: a trap on the last step.
    (
        "an-inclusive-range-ends-at-the-maximum",
        "\
fn id(x: int) -> int { return x }

fn main() {
  let hi = id(9223372036854775807)
  var n = 0
  for i in (hi - 2)..=hi {
    n = n + 1
  }
  io.print(n)
  var last = 0
  for i in (hi - 1)..=hi {
    if i == hi - 1 {
      continue
    }
    last = i
  }
  io.print(last == hi)
  for i in 3..=3 {
    io.print(i)
  }
  for i in 4..=3 {
    io.print(i)
  }
}
",
    ),
    // `int`'s minimum written as a literal, a remainder by -1, and shifts at
    // the edge of their range. `min % -1` trapped on two backends and was 0 on
    // the third; it is 0.
    (
        "integer-edges",
        "\
fn id(x: int) -> int { return x }

let LOW = -9223372036854775808

fn main() {
  let min = -9223372036854775808
  io.print(min)
  io.print(LOW == min)
  io.print(min % id(-1))
  io.print(id(7) % id(-1))
  io.print(id(-7) % id(2))
  io.print(id(1) << id(63))
  io.print(id(-8) >> id(1))
  io.print(-id(5))
  let v = match id(min) {
    -9223372036854775808 => \"min\",
    _ => \"other\",
  }
  io.print(v)
}
",
    ),
    // An or-pattern bound its names through its first alternative whichever
    // one matched, so a `Square` here read its payload as a `Circle`'s.
    (
        "or-pattern-binds-through-the-alternative-that-matched",
        "\
enum Shape {
  Circle(r: int)
  Square(colour: int, side: int)
  Dot
}

fn size(s: Shape) -> int {
  return match s {
    Circle(n) | Square(_, n) => n,
    Dot => 0,
  }
}

fn describe(o: Option<Shape>) -> str {
  return match o {
    nil => \"none\",
    Circle(n) | Square(_, n) if n > 5 => \"big \\(n)\",
    Circle(n) | Square(_, n) => \"small \\(n)\",
    _ => \"dot\",
  }
}

fn main() {
  io.print(size(Square(99, 7)))
  io.print(size(Circle(3)))
  io.print(size(Dot))
  io.print(describe(Square(1, 9)))
  io.print(describe(Circle(2)))
  io.print(describe(nil))
}
",
    ),
    // Floats at every edge of how they are written. The VM and the native
    // runtime used Rust's `{}` and the Wasm glue JavaScript's `String`, so
    // this printed `inf` or `Infinity`, `-0.0` or `0.0`, `0.0000001` or
    // `1e-7`, and a 327-character decimal or `5e-324`, by backend. A float
    // interpolated into a constant was refused for the same reason.
    (
        "floats-are-written-alike",
        "\
fn f(x: float) -> float { return x }

let BIG = \"big \\(1e21) tiny \\(1.5e-7) whole \\(2.0)\"

fn main() {
  io.print(f(0.1) + f(0.2))
  io.print(f(1.0) / f(3.0))
  io.print(f(1e21))
  io.print(f(1e20))
  io.print(f(123456789012345680000.0))
  io.print(f(1e-7))
  io.print(f(1.5e-7))
  io.print(f(0.000001))
  io.print(f(1.5e300) * f(1e10))
  io.print(-f(1.5e300) * f(1e10))
  io.print(f(0.0) / f(0.0))
  io.print(f(-0.0))
  io.print(f(0.0))
  io.print(f(2.0))
  io.print(f(-2.5))
  io.print(f(5e-324))
  io.print(f(1.7976931348623157e308))
  io.print(f(-1e21))
  io.print(f(1e16))
  io.print(f(1e15) + f(0.5))
  io.print(9007199254740993 as float)
  io.print(9223372036854775807 as float)
  io.print(\"\\(f(1e21)) \\(f(-0.0)) \\(f(0.1)) \\(f(1.0) / f(0.0)) \\(f(2.5e-7))\")
  io.print(BIG)
}
",
    ),
    // A recursion a thousand frames deep, of a function holding a dozen
    // values across its call, runs on every target — a WebAssembly host's
    // stack included, which is the shallowest of the three and ends a few
    // thousand frames of this down. Past each target's limit is a trap; see
    // `a-recursion-past-the-limit-traps`.
    (
        "a-thousand-frames-deep-runs-everywhere",
        "\
struct P {
  x: int
  y: int
  name: str
}

fn wide(n: int, acc: P) -> int {
  if n == 0 {
    return acc.x
  }
  let a = n * 2
  let b = a + 3
  let c = b * a
  let d = \"\\(c)\"
  let e = P{ x: acc.x + 1, y: b, name: d }
  let f = [a, b, c]
  let g = f.len() + e.y
  let r = wide(n - 1, e)
  return r + g - g + f[0] - a + d.len() - d.len()
}

fn even(n: int) -> bool {
  if n == 0 {
    return true
  }
  return odd(n - 1)
}

fn odd(n: int) -> bool {
  if n == 0 {
    return false
  }
  return even(n - 1)
}

fn main() {
  io.print(wide(1000, P{ x: 0, y: 0, name: \"\" }))
  io.print(even(1000))
}
",
    ),
    // A float exactly halfway between two shortest decimals. ECMAScript picks
    // the even one, and Rust's `{:e}` the upper, so the VM and the native
    // runtime printed `…624.3` where Wasm printed `…624.2` — at run time, in
    // a constant folded at compile time (which Wasm then disagreed with
    // itself about), and in a derived hash, which hashes the text.
    (
        "float-ties-go-to-the-even-digit",
        "\
fn f(x: float) -> float { return x }

let TIE = \"\\(1125899906842624.25)\"

@derive(Hash)
struct F {
  x: float
}

fn main() {
  let n = 237061009
  io.print(n as float / 8192.0)
  io.print(f(1125899906842624.25))
  io.print(f(1125899906842625.25))
  io.print(f(1125899906842624.75))
  io.print(f(577411599005501.25))
  io.print(-f(577411599005501.25))
  io.print(TIE)
  io.print(TIE == \"\\(f(1125899906842624.25))\")
  io.print(F{ x: 1125899906842624.25 }.hash() == F{ x: f(1125899906842624.2) }.hash())
  var odd = 0
  for i in 237060000..237060400 {
    let s = \"\\(i as float / 8192.0)\"
    let last = s.slice(s.len() - 1, s.len())
    if last == \"3\" || last == \"7\" {
      odd = odd + 1
    }
  }
  io.print(odd)
}
",
    ),
];

/// Programs pinning down the WebAssembly target against the other two. Each
/// one is here because Wasm once refused it, trapped on it, or answered
/// differently.
const WASM_TARGET: &[(&str, &str)] = &[
    // A match arm binding what a `nil` arm leaves behind holds the payload,
    // so the binding unwraps. At `T = Option<int>` the subject `Option<T>` is
    // `Option<int>` and so is the binding, and the unwrap from a type to
    // itself made a module the validator refused (E0900).
    (
        "a-generic-match-binding-at-an-optional",
        r#"fn wrapit<T>(x: T) -> Option<T> {
  return x
}

fn or_else<T>(x: T, d: T) -> T {
  return match wrapit(x) {
    nil => d,
    v => v,
  }
}

fn pick<T>(x: Option<T>, d: T) -> T {
  return match x {
    nil => d,
    v => v,
  }
}

fn first_or<T>(xs: [T], d: T) -> T {
  return match xs.get(0) {
    nil => d,
    v => v,
  }
}

fn main() {
  let a: Option<int> = 5
  let n: Option<int> = nil
  let r = or_else(a, n)
  if r != nil {
    io.print(r)
  }
  io.print(or_else(n, a) == a)
  let b: Option<int> = 3
  let p: Option<int> = pick(b, n)
  io.print(p == nil)
  let q: Option<int> = pick(n, b)
  io.print(q == b)
  let s: Option<str> = "hi"
  let none: Option<str> = nil
  let t: Option<str> = pick(s, none)
  io.print(t == "hi")
  let u: Option<str> = pick(none, s)
  io.print(u == nil)
  let xs: [Option<int>] = [7, nil]
  let first = first_or(xs, n)
  if first != nil {
    io.print(first)
  }
  io.print(first_or([1, 2], 9))
  io.print(or_else(4, 9))
}
"#,
    ),
    // A map key was compared with `i32.eq` unless it was a number or a string,
    // so a struct, enum, tuple, optional or slice key produced a module the
    // validator refused (E0900). The removal from a map built elsewhere is here
    // because it was the one key comparison the string runtime's scan missed.
    (
        "aggregate-map-keys",
        r#"struct P {
  x: int
  name: str
}

enum C {
  Red
  Rgb(int, int, int)
}

fn drop_a(m: {str: int}) -> {str: int} {
  var n = m
  n.remove("a")
  return n
}

fn ori(o: Option<int>, d: int) -> int {
  return match o {
    nil => d,
    v => v,
  }
}

fn ors(o: Option<str>, d: str) -> str {
  return match o {
    nil => d,
    v => v,
  }
}

fn orb(o: Option<bool>, d: bool) -> bool {
  return match o {
    nil => d,
    v => v,
  }
}

fn main() {
  var ps: {P: int} = {P{ x: 1, name: "one" }: 1}
  ps[P{ x: 1, name: "one" }] = 10
  ps[P{ x: 2, name: "two" }] = 20
  io.print(ps.len())
  io.print(ori(ps[P{ x: 1, name: "one" }], -1))
  io.print(ori(ps[P{ x: 1, name: "uno" }], -1))
  ps.remove(P{ x: 1, name: "one" })
  io.print(ps.len())

  var cs: {C: str} = {}
  cs[C.Red] = "red"
  cs[C.Rgb(1, 2, 3)] = "grey"
  cs[C.Rgb(1, 2, 3)] = "gray"
  cs[C.Rgb(3, 2, 1)] = "other"
  io.print(cs.len())
  io.print(ors(cs[C.Rgb(1, 2, 3)], "?"))

  var ts: {(int, str): bool} = {}
  ts[(1, "a")] = true
  ts[(1, "a")] = false
  ts[(1, "b")] = true
  io.print(ts.len())
  io.print(orb(ts[(1, "a")], true))

  var os: {Option<int>: int} = {}
  os[nil] = 1
  os[nil] = 2
  os[5] = 3
  io.print(os.len())
  io.print(ori(os[nil], 0))

  var ss: {[int]: int} = {}
  ss[[1, 2]] = 1
  ss[[1, 2]] = 2
  ss[[2, 1]] = 3
  io.print(ss.len())
  io.print(ori(ss[[1, 2]], 0))

  var nested: {{str: int}: str} = {}
  nested[{"a": 1}] = "x"
  nested[{"a": 1}] = "y"
  io.print(nested.len())

  let m = drop_a({"a": 1, "b": 2})
  io.print(m.len())
  let q: {P: int} = {P{ x: 1, name: "one" }: 1}
  let r: {P: int} = {P{ x: 1, name: "one" }: 1}
  io.print(q == r)
}
"#,
    ),
    // `Option<Option<T>>` is `Option<T>`, so a lookup in a map of optional
    // values, or `.get()` on a slice of them, answers the stored optional
    // itself. Wasm looked for a box around an optional, found none and trapped.
    // A slice is a header over a buffer that may be longer than it, written
    // in place when nothing else can see it (`slices` in the Wasm backend).
    // Every way a slice's reference can be kept — another local, a call, a
    // field, a map, an optional, a closure, a slice of slices, a loop over it —
    // is here, with both copies changed afterwards: the case that breaks is a
    // buffer shared by two names, each pushing into the other's next slot.
    (
        "slices-are-values",
        r#"struct Holder {
  items: [int]
}

fn show(xs: [int]) -> str {
  var s = "["
  for x in xs {
    s = s + " \(x)"
  }
  return s + " ]"
}

fn grow(xs: [int]) -> [int] {
  var ys = xs
  ys.push(99)
  return ys
}

fn stamp(xs: [int], v: int) -> [int] {
  var ys = xs
  ys[0] = v
  return ys
}

fn counted(n: int) -> [int] {
  var xs: [int] = []
  for i in 0..n {
    xs.push(i)
  }
  return xs
}

fn main() {
  // A snapshot is a value: what happens to the original afterwards is not
  // seen through it, even when the storage had room to spare.
  var xs = counted(10)
  let a = xs
  xs.push(10)
  xs[0] = 100
  io.print(show(a))
  io.print(show(xs))

  // Two names for one buffer with capacity left, both pushing: neither may
  // write into the other's next slot.
  var p: [int] = [1, 2, 3]
  p.push(4)
  var q = p
  p.push(5)
  q.push(6)
  q[0] = -1
  io.print(show(p))
  io.print(show(q))

  // Through a call, both ways.
  let g = grow(p)
  let s = stamp(p, 7)
  io.print(show(p))
  io.print(show(g))
  io.print(show(s))

  // Into a struct, a map, an optional and a closure, then changed.
  var h = Holder{ items: xs }
  var m = {"k": xs}
  let o: Option<[int]> = xs
  let held = xs
  let f = || held.len()
  xs.push(11)
  xs[1] = -5
  io.print(show(h.items))
  match m["k"] {
    nil => io.print("nil"),
    v => io.print(show(v)),
  }
  io.print(f())
  io.print(xs.len())
  match o {
    nil => io.print("nil"),
    v => io.print(v.len()),
  }
  var it = h.items
  it.push(12)
  io.print(h.items.len())
  io.print(it.len())

  // The classic: one row reused while it is collected.
  var rows: [[int]] = []
  var row: [int] = []
  for i in 0..4 {
    row.push(i)
    rows.push(row)
  }
  for r in rows {
    io.print(show(r))
  }
  var first = rows[0]
  first.push(50)
  rows[1] = first
  io.print(show(rows[0]))
  io.print(show(rows[1]))

  // Iterating a slice while pushing onto it sees the slice as it was.
  var walk: [int] = [1, 2]
  for w in walk {
    walk.push(w * 10)
  }
  io.print(show(walk))

  // A window is a copy.
  var base = counted(6)
  var win = base[1..4]
  win[0] = 42
  win.push(43)
  base[2] = 24
  io.print(show(base))
  io.print(show(win))

  // Self-assignment, and a slice rebuilt in a loop from its own copy.
  var self_ = [5]
  self_ = self_
  self_.push(6)
  var acc: [int] = []
  for i in 0..5 {
    let before = acc
    acc.push(i)
    if before.len() + 1 != acc.len() {
      io.print("bad")
    }
  }
  io.print(show(acc))

  // Capacity is not length: equality, `get` and ranges see only the slice.
  var spare: [int] = []
  spare.push(1)
  io.print(spare == [1])
  io.print(spare.get(1) == nil)
  io.print(show(spare[0..3]))
  var keys: {[int]: str} = {}
  keys[spare] = "one"
  match keys[[1]] {
    nil => io.print("missing"),
    v => io.print(v),
  }
}
"#,
    ),
    // Wasm's generated `==` called itself for each component, so two lists a
    // few hundred thousand cells long exhausted the engine's stack with a
    // `RangeError` where the other two answer. It walks a worklist now.
    (
        "deep-values-compare-without-recursing",
        r#"enum List {
  Empty
  Cons(int, List)
}

struct Node {
  label: str
  next: Option<Node>
}

fn build(n: int, last: int) -> List {
  var l = List.Cons(last, List.Empty)
  for i in 0..n {
    l = List.Cons(i, l)
  }
  return l
}

fn chain(n: int) -> Option<Node> {
  var head: Option<Node> = nil
  for i in 0..n {
    head = Node{ label: "n\(i % 3)", next: head }
  }
  return head
}

fn main() {
  let a = build(200000, 0)
  let b = build(200000, 0)
  let c = build(200000, 1)
  io.print(a == b)
  io.print(a == c)
  io.print(a != c)
  io.print(chain(100000) == chain(100000))
  let nested = [[build(3, 0)], [build(2, 0), List.Empty]]
  io.print(nested == [[build(3, 0)], [build(2, 0), List.Empty]])
  io.print(nested == [[build(3, 0)], [build(2, 1), List.Empty]])
  io.print((1, [1.5, 2.5], "x") == (1, [1.5, 2.5], "x"))
  io.print({"k": [build(1, 0)]} == {"k": [build(1, 0)]})
}
"#,
    ),
    // A map literal with a key given twice is one entry, at the first key's
    // position with the last value — the VM's rule. Wasm built its arrays
    // as written and had two.
    (
        "a-map-literal-keeps-one-entry-per-key",
        r#"fn main() {
    let k = "a"
    let m = {k: 1, "b": 2, "a": 3}
    io.print(m.len())
    io.print(m.keys().len())
    for key in m.keys() {
        io.print(key)
    }
    let n = {1: "x", 1: "y"}
    io.print(n.len())
}
"#,
    ),
    (
        "optional-values-in-maps-and-slices",
        r#"fn main() {
    var m: {str: Option<int>} = {"a": 5, "b": nil}
    m["c"] = 7
    m["d"] = nil
    let a = m["a"]
    let b = m["b"]
    let z = m["z"]
    io.print(a == nil)
    io.print(b == nil)
    io.print(z == nil)
    let c = m["c"]
    if c != nil {
        io.print(c)
    }
    io.print(m.len())
    var xs: [Option<str>] = ["x", nil]
    xs.push(nil)
    io.print(xs.get(0) == nil)
    io.print(xs.get(1) == nil)
    io.print(xs.get(9) == nil)
    match xs.get(0) {
        nil => io.print("none"),
        s => io.print(s),
    }
    for k in m.keys() {
        io.print(k)
    }
}
"#,
    ),
];

/// Programs pinning down the native target against the other two. Each one is
/// here because the native backend once got it wrong or took too long.
const NATIVE_TARGET: &[(&str, &str)] = &[
    // Every native `push` and `xs[i] = v` copied the whole slice, so a loop of
    // pushes was quadratic. A slice has room to spare now, and is written in
    // place when the local it is in is the only thing that can reach it —
    // `slices` in the native backend. `slices-are-values` above covers the
    // ways a reference can be kept; this covers the ways a slice can reach a
    // function without being its own — a parameter, a generic, a field read
    // back, a capture, an optional, a task's frame across a suspension — and
    // each element kind, with enough pushes to reallocate many times.
    (
        "slices-grow-in-place-natively",
        r#"struct Holder {
    var items: [int]
}

fn show(xs: [int]) -> str {
    var s = "["
    for x in xs {
        s = s + " \(x)"
    }
    return s + " ]"
}

fn total(xs: [int]) -> int {
    var t = 0
    for x in xs {
        t = t + x
    }
    return t
}

// A parameter's slice is the caller's too: the first push copies, and the
// ones after it go straight into the copy.
fn extended(var xs: [int], n: int) -> [int] {
    for i in 0..n {
        xs.push(i)
    }
    return xs
}

fn appended<T>(xs: [T], v: T) -> [T] {
    var ys = xs
    ys.push(v)
    return ys
}

// Every local is spilled into the task's frame at a suspension and read back
// after it, so after `yield` the slice is one read out of the frame, which the
// first push must copy rather than write into.
async fn gather(n: int) -> [int] {
    var xs: [int] = []
    for i in 0..n {
        xs.push(i)
        let seen = xs
        task.yield()
        xs.push(seen.len() * 100)
    }
    return xs
}

async fn main() {
    // Built by pushing, through many doublings, then written in place.
    var xs: [int] = []
    for i in 0..1000 {
        xs.push(i)
    }
    for i in 0..1000 {
        xs[i] = xs[i] * 2
    }
    io.print(xs.len())
    io.print(total(xs))

    // A snapshot every time round: each push copies, and each snapshot keeps
    // the length and the contents it had.
    var snaps: [[int]] = []
    var grow: [int] = []
    for i in 0..300 {
        snaps.push(grow)
        grow.push(i)
    }
    var lengths = 0
    var wrong = 0
    for s in snaps {
        lengths = lengths + s.len()
        for j in 0..s.len() {
            if s[j] != j {
                wrong = wrong + 1
            }
        }
    }
    io.print(lengths)
    io.print(wrong)
    io.print(show(snaps[5]))
    io.print(grow.len())

    // Through a parameter, leaving the caller's slice alone.
    let base = [1, 2, 3]
    let more = extended(base, 50)
    io.print(base.len())
    io.print(more.len())
    io.print(total(more))

    // Generic, over a slice of slices: the rows are shared between the two,
    // and a row written through one is not seen through the other.
    let rows = [[1], [2, 3]]
    var more_rows = appended(rows, [4, 5, 6])
    var r0 = more_rows[0]
    r0.push(10)
    more_rows[0] = r0
    io.print(rows[0].len())
    io.print(more_rows[0].len())
    io.print(more_rows.len())

    // A `var` field grown through a local and stored back each time, and a
    // copy of it taken between.
    var h = Holder{ items: [] }
    for i in 0..20 {
        var it = h.items
        it.push(i)
        h.items = it
    }
    let held = h.items
    var it = h.items
    it[0] = 99
    h.items = it
    io.print(held[0])
    io.print(h.items[0])

    // A captured slice is the closure's, and every call starts from it.
    let fixed = xs
    let f = || -> int {
        var local = fixed
        local.push(1)
        return local.len()
    }
    io.print(f())
    io.print(f())

    // Floats and booleans, written in place.
    var fs: [float] = []
    var bs: [bool] = []
    for i in 0..10 {
        fs.push(0.5 * (i as float))
        bs.push(i % 3 == 0)
    }
    fs[9] = -1.0
    bs[0] = false
    for x in fs {
        io.print(x)
    }
    var trues = 0
    for x in bs {
        if x {
            trues = trues + 1
        }
    }
    io.print(trues)

    // An optional's payload is a copy once it is written.
    let o: Option<[int]> = xs
    match o {
        nil => io.print("nil"),
        v => {
            var w = v
            w.push(1)
            io.print(w.len())
        },
    }
    io.print(xs.len())

    // A grid, one row at a time.
    var grid: [[int]] = []
    for i in 0..4 {
        var row: [int] = []
        for j in 0..4 {
            row.push(i * j)
        }
        grid.push(row)
    }
    for i in 0..4 {
        var row = grid[i]
        row[i] = -1
        grid[i] = row
    }
    for row in grid {
        io.print(show(row))
    }

    let g = await gather(4)
    io.print(show(g))
}
"#,
    ),
];

/// Programs the type checker once refused, or accepted and then typed in a
/// way the backends could not agree about.
const TYPE_CHECKER: &[(&str, &str)] = &[
    // A `T: Error` is an `Error`, so it stands where an `error` is wanted, as
    // a concrete type implementing `Error` does (§7.2, §11). It was E0200
    // "expected `error`, found `T`". The message is found through the bound,
    // and the tag `is` and `as` read is the concrete type's once the
    // function is specialised.
    (
        "a-bounded-error-parameter-is-an-error",
        r#"struct MyErr {
    m: str
}

impl Error for MyErr {
    fn message(self) -> str {
        return self.m
    }
}

struct Other {
    code: int
}

impl Error for Other {
    fn message(self) -> str {
        return "other \(self.code)"
    }
}

fn to_err<T: Error>(x: T) -> error {
    return x
}

fn fail<T: Error>(x: T) -> (int, error) {
    return 0, x
}

fn describe(e: error) -> str {
    if e != nil {
        return e.message()
    }
    return "none"
}

fn pass<T: Error>(x: T) -> str {
    return describe(x)
}

fn wrap<T: Error>(x: T) -> error {
    let e: error = x
    return e
}

fn main() {
    let a = to_err(MyErr{ m: "mine" })
    io.print(describe(a))
    io.print(MyErr.is(a))
    io.print(Other.is(a))
    let b = wrap(Other{ code: 7 })
    io.print(describe(b))
    io.print(Other.is(b))
    let o = Other.as(b)
    if o != nil {
        io.print(o.code)
    }
    let (v, err) = fail(MyErr{ m: "pair" })
    if err != nil {
        io.print(describe(err))
        io.print(MyErr.is(err))
    } else {
        io.print(v)
    }
    io.print(pass(Other{ code: 3 }))
}
"#,
    ),
    // Calling an `async fn` yields its task, and a method or an associated
    // function is no exception (§12.1). Only free functions were wrapped, so
    // `await p.later()` was "only a task can be awaited" and `p.later() + 1`
    // added to a task: a trap on the VM, a pointer printed natively, an
    // invalid module on Wasm. A generic `async fn` called where a `Task<T>` is
    // expected made `T` the whole task.
    (
        "async-methods-yield-their-tasks",
        r#"use std/task

struct P {
    a: int
}

impl P {
    async fn later(self) -> int {
        await task.sleep(1)
        return self.a
    }

    async fn make(a: int) -> int {
        await task.sleep(1)
        return a * 2
    }

    async fn pair(self) -> (int, error) {
        await task.sleep(1)
        return self.a, nil
    }
}

struct Box<T> {
    v: T
}

impl<T> Box<T> {
    async fn get(self) -> T {
        await task.sleep(1)
        return self.v
    }

    async fn wrap_later<U>(self, u: U) -> Box<U> {
        await task.sleep(1)
        return Box{ v: u }
    }
}

async fn job<T>(x: T) -> T {
    await task.sleep(1)
    return x
}

async fn main() {
    let p = P{ a: 3 }
    let x = await p.later()
    let y = await P.make(4)
    io.print(x + y)
    let t1 = p.later()
    let t2 = P.make(5)
    let both = await task.all([t1, t2])
    io.print(both[0] + both[1])
    let (v, err) = await p.pair()
    if err != nil {
        return
    }
    io.print(v)
    let b = Box{ v: "boxed" }
    io.print(await b.get())
    let bb = await b.wrap_later(7)
    io.print(bb.v)
    let t: Task<int> = job(1)
    io.print(await t)
    let ts: [Task<str>] = [job("a"), job("b")]
    let words = await task.all(ts)
    io.print(words[0] + words[1])
    let tb: Task<str> = b.get()
    io.print(await tb)
}
"#,
    ),
    // A trait method's call through a bound or a `dyn` yields what a direct
    // call does: a fallible one's pair, which could not be destructured, and
    // an `async` one's task, which the Wasm dispatcher was typed without.
    (
        "trait-methods-yield-what-their-calls-yield",
        r#"use std/task

trait Source {
    async fn get(self) -> int
    fn probe(self) -> (int, error)
    async fn fetch(self) -> (str, error)
    async fn twice(self) -> int {
        let a = await self.get()
        return a * 2
    }
}

struct P {
    a: int
}

impl Source for P {
    async fn get(self) -> int {
        await task.sleep(1)
        return self.a
    }

    fn probe(self) -> (int, error) {
        if self.a > 5 {
            return 0, errors.new("big")
        }
        return self.a, nil
    }

    async fn fetch(self) -> (str, error) {
        await task.sleep(1)
        if self.a > 3 {
            return "", errors.new("far")
        }
        return "near", nil
    }
}

async fn via<S: Source>(s: S) -> str {
    let (v, err) = s.probe()
    if err != nil {
        return err.message()
    }
    let (w, ferr) = await s.fetch()
    if ferr != nil {
        return ferr.message()
    }
    let g = await s.get()
    let t = await s.twice()
    return "\(v) \(w) \(g) \(t)"
}

async fn via_dyn(s: dyn Source) -> str {
    let (v, err) = s.probe()
    if err != nil {
        return err.message()
    }
    let (w, ferr) = await s.fetch()
    if ferr != nil {
        return ferr.message()
    }
    let g = await s.get()
    let t = await s.twice()
    return "\(v) \(w) \(g) \(t)"
}

async fn main() {
    io.print(await via(P{ a: 3 }))
    io.print(await via(P{ a: 9 }))
    io.print(await via_dyn(P{ a: 4 }))
    io.print(await via_dyn(P{ a: 8 }))
    let p = P{ a: 2 }
    let started = [p.get(), p.twice()]
    let got = await task.all(started)
    io.print(got[0] + got[1])
}
"#,
    ),
    // `==` on two values of a type that mentions a parameter compares them
    // structurally, as `==` on two `T` does, and was refused unless the type was
    // a bare `T`. A map's key and value types count too.
    (
        "compared-inside-a-generic-type",
        r#"struct Box<T> {
    v: T
}

enum Pick<T> {
    One(T)
    Neither
}

fn opt_eq<T>(a: Option<T>, b: Option<T>) -> bool {
    return a == b
}

fn slice_eq<T>(a: [T], b: [T]) -> bool {
    return a == b
}

fn tuple_ne<T>(a: (T, T), b: (T, T)) -> bool {
    return a != b
}

fn box_eq<T>(a: Box<T>, b: Box<T>) -> bool {
    return a == b
}

fn pick_eq<T>(a: Pick<T>, b: Pick<T>) -> bool {
    return a == b
}

fn map_eq<K, V>(a: { K: V }, b: { K: V }) -> bool {
    return a == b
}

fn forward<T>(a: Option<T>, b: Option<T>) -> bool {
    return opt_eq(a, b)
}

fn main() {
    let o: Option<int> = 3
    let n: Option<int> = nil
    io.print(opt_eq(o, 3))
    io.print(opt_eq(o, n))
    io.print(opt_eq(n, nil))
    io.print(slice_eq([1, 2], [1, 2]))
    io.print(slice_eq(["a"], ["b"]))
    io.print(tuple_ne((1, 2), (1, 2)))
    io.print(tuple_ne(("a", "b"), ("a", "c")))
    io.print(box_eq(Box{ v: [1.5] }, Box{ v: [1.5] }))
    io.print(pick_eq(Pick.One("x"), Pick.One("x")))
    io.print(pick_eq(Pick.One(1), Pick.Neither))
    io.print(map_eq({ "a": 1 }, { "a": 1 }))
    io.print(map_eq({ 1: "a" }, { 1: "b" }))
    io.print(forward(o, o))
}
"#,
    ),
    // An or-pattern's alternatives bind one local per name, in variants, nested
    // inside a variant, in tuples and in struct patterns against an optional. The
    // resolver declared each alternative's names afresh, so every one of these
    // was E0112 and no or-pattern could bind anything.
    (
        "or-pattern-alternatives-share-their-names",
        r#"enum E {
    A(int)
    B(int)
    C
}

enum M {
    S(E)
    N
}

struct P {
    x: int
    y: int
}

fn f(e: E) -> int {
    return match e {
        A(x) | B(x) => x,
        C => 0,
    }
}

fn g(m: M) -> int {
    return match m {
        S(A(y) | B(y)) => y,
        S(C) => -1,
        N => 0,
    }
}

fn h(t: (int, int)) -> int {
    return match t {
        (0, x) | (x, 0) => x,
        _ => -1,
    }
}

fn k(p: Option<P>) -> str {
    return match p {
        P{ x: 0, y } | P{ x: y, y: 0 } => "axis \(y)",
        nil => "none",
        _ => "off",
    }
}

fn main() {
    io.print("\(f(E.A(1))) \(f(E.B(2))) \(f(E.C))")
    io.print("\(g(M.S(E.A(3)))) \(g(M.S(E.B(4)))) \(g(M.S(E.C))) \(g(M.N))")
    io.print("\(h((0, 5))) \(h((6, 0))) \(h((1, 1)))")
    io.print(k(P{ x: 0, y: 7 }))
    io.print(k(P{ x: 8, y: 0 }))
    io.print(k(P{ x: 1, y: 1 }))
    io.print(k(nil))
}
"#,
    ),
    // A trait's default method, inherited by `impl<T> Show for Box<T>`, is a
    // method of `Box<T>` and has the block's `T`. It had none: a default calling
    // another method on `self` was E0209 "cannot infer `T`", even never called,
    // and one that compiled took the template `Box`, which Wasm refused to
    // validate against the `Box<int>` it was handed.
    (
        "default-methods-of-a-generic-impl",
        r#"trait Show {
    fn show(self) -> str
    fn twice(self) -> str {
        return self.show() + self.show()
    }
    fn tag(self) -> str {
        return "tag"
    }
    fn loud(self) -> str {
        return self.twice() + "!"
    }
}

trait Comparable {
    fn compare(self, other: Self) -> int
    fn less_than(self, other: Self) -> bool {
        return self.compare(other) < 0
    }
}

struct Box<T> {
    v: T
}

enum Maybe<T> {
    Some(T)
    Nothing
}

struct Wrap<T> {
    n: int
    v: T
}

impl<T> Show for Box<T> {
    fn show(self) -> str {
        return "B"
    }
}

impl<T> Show for Maybe<T> {
    fn show(self) -> str {
        return match self {
            Some(_) => "S",
            Nothing => "N",
        }
    }
}

impl<T> Comparable for Wrap<T> {
    fn compare(self, other: Wrap<T>) -> int {
        return self.n - other.n
    }
}

fn via<S: Show>(s: S) -> str {
    return s.loud() + s.tag()
}

fn smaller<C: Comparable>(a: C, b: C) -> C {
    if a.less_than(b) {
        return a
    }
    return b
}

fn main() {
    io.print(Box{ v: 1 }.twice())
    io.print(Box{ v: "s" }.tag())
    io.print(via(Box{ v: 2.5 }))
    io.print(via(Maybe.Some(3)))
    let m: Maybe<str> = Maybe.Nothing
    io.print(m.loud())
    let xs: [dyn Show] = [Box{ v: 1 }, Box{ v: "s" }, Maybe.Some(true)]
    for x in xs {
        io.print(x.tag() + x.twice())
    }
    let a = Wrap{ n: 1, v: "a" }
    let b = Wrap{ n: 2, v: "b" }
    io.print(a.less_than(b))
    io.print(smaller(b, a).v)
}
"#,
    ),
    // `Self` names the same type in a method's body as in its signature: in an
    // annotation, a slice of it, a closure's parameter, an associated function,
    // and a trait's default method. It was E0204 "unknown type `Self`" in a body.
    (
        "self-names-the-type-in-a-body",
        r#"struct P {
    n: int
}

struct Box<T> {
    v: T
}

impl P {
    fn twin(self) -> Self {
        let y: Self = self
        let ys: [Self] = [y]
        return ys[0]
    }

    fn make(n: int) -> Self {
        let p: Self = P{ n: n }
        return p
    }
}

impl<T> Box<T> {
    fn same(self) -> Self {
        let b: Self = self
        let keep = |x: Self| -> T { return x.v }
        return Box{ v: keep(b) }
    }
}

trait Merge {
    fn merge(self, other: Self) -> Self
    fn thrice(self) -> Self {
        let f = |x: Self| -> Self { return x.merge(self) }
        return f(f(self))
    }
    fn dup(self) -> [Self] {
        let me: Self = self
        return [me, me]
    }
}

impl Merge for P {
    fn merge(self, other: Self) -> Self {
        return P{ n: self.n + other.n }
    }
}

impl<T> Merge for Box<T> {
    fn merge(self, other: Self) -> Self {
        return other
    }
}

fn main() {
    io.print(P{ n: 1 }.twin().n)
    io.print(P.make(4).n)
    io.print(Box{ v: "b" }.same().v)
    io.print(P{ n: 2 }.thrice().n)
    io.print(P{ n: 2 }.dup().len())
    io.print(Box{ v: 3 }.thrice().v)
    io.print(Box{ v: 3.5 }.dup()[1].v)
}
"#,
    ),
    // A narrowing survives a loop whose every write to the local stores a value
    // that cannot be nil, as it survives the same writes in straight-line code.
    // The loop dropped it for any local it wrote, so each of these was E0201.
    (
        "a-loop-keeps-a-narrowing-its-writes-keep",
        r#"fn guard() {
    var x: Option<int> = 5
    if x == nil {
        return
    }
    for i in 0..3 {
        io.print(x + 1)
        x = i
    }
}

fn inside() {
    var x: Option<int> = 5
    if x != nil {
        for i in 0..3 {
            x = i * 10
            io.print(x + 1)
        }
    }
}

fn nested() {
    var x: Option<int> = 1
    if x == nil {
        return
    }
    for i in 0..2 {
        for j in 0..2 {
            x = x + i + j
        }
        io.print(x)
    }
}

fn main() {
    guard()
    inside()
    nested()
}
"#,
    ),
    // A pattern against an optional is one for the value present: a tuple, and a
    // variant or a struct of a generic type, were refused as the wrong type. An
    // `Option<Msg<int>>` or `Option<[int]>` wanted also says what a `Stop` or an
    // empty literal is.
    (
        "patterns-against-an-optional",
        r#"enum Msg<T> {
    Data(v: T)
    Stop
}

struct G<T> {
    x: T
    y: T
}

fn tuple(o: Option<(int, str)>) -> str {
    return match o {
        nil => "none",
        (0, s) => "zero " + s,
        (n, _) => "n \(n)",
    }
}

fn message(o: Option<Msg<int>>) -> str {
    return match o {
        Data(v) => "data \(v)",
        Stop => "stop",
        nil => "nil",
    }
}

fn qualified<T>(o: Option<Msg<T>>) -> str {
    return match o {
        nil => "nil",
        Msg.Data(_) => "data",
        Msg.Stop => "stop",
    }
}

fn grid(o: Option<G<int>>) -> str {
    return match o {
        nil => "nil",
        G{ x: 0, y } => "on y \(y)",
        G{ x, y } => "at \(x) \(y)",
    }
}

fn main() {
    io.print(tuple((0, "a")))
    io.print(tuple((4, "b")))
    io.print(tuple(nil))
    io.print(message(Data(1)))
    io.print(message(Stop))
    io.print(message(nil))
    let m: Msg<str> = Data("s")
    io.print(qualified(m))
    let none: Option<Msg<float>> = nil
    io.print(qualified(none))
    io.print(grid(G{ x: 0, y: 5 }))
    io.print(grid(G{ x: 2, y: 3 }))
    io.print(grid(nil))
    let words: Option<{ str: int }> = { }
    let nums: Option<[int]> = []
    if words != nil {
        if nums != nil {
            io.print("\(words.len()) \(nums.len())")
        }
    }
    let boxed: Option<G<str>> = G{ x: "a", y: "b" }
    io.print(boxed == nil)
}
"#,
    ),
    // An optional of an optional is the optional, so an expected `Option<int>`
    // makes a declared `Option<T>`'s `T` an `int` or an `Option<int>`, and the
    // arguments say which. The expected type used to fix `T` as `int` first, and
    // every one of these was E0209 "conflicting types for `T`".
    (
        "an-expected-optional-leaves-t-to-the-arguments",
        r#"struct Box<T> {
    v: T
}

impl<T> Box<T> {
    fn peek(self) -> Option<T> {
        return self.v
    }

    fn of(v: T) -> Option<Box<T>> {
        return Box{ v: v }
    }
}

fn first<T>(xs: [T]) -> Option<T> {
    return xs.get(0)
}

fn wrap<T>(x: T) -> Option<T> {
    return x
}

fn head(fs: [Option<int>]) -> Option<int> {
    return first(fs)
}

fn main() {
    let a: Option<int> = 5
    let fs: [Option<int>] = [a, nil]
    let r: Option<Option<int>> = first(fs)
    io.print(r == nil)
    let s: Option<int> = first(fs)
    io.print(s == 5)
    let w: Option<int> = wrap(a)
    io.print(w == 5)
    let n: Option<int> = wrap(7)
    io.print(n == 7)
    let none: Option<int> = first([])
    io.print(none == nil)
    io.print(head([nil, a]) == nil)
    let b = Box{ v: a }
    let p: Option<int> = b.peek()
    io.print(p == 5)
    let q: Option<Box<Option<int>>> = Box.of(a)
    io.print(q == nil)
}
"#,
    ),
    // `as` on a generic error type asks for the specialisation it is used as,
    // inside a generic function too, where the tag it tests for is settled once
    // the function is specialised. The declaration's tag answered `nil` for every
    // one.
    (
        "a-generic-error-downcasts-to-its-specialisation",
        r#"struct Wrapped<T> {
    inner: T
    why: str
}

impl<T> Error for Wrapped<T> {
    fn message(self) -> str {
        return "wrapped: " + self.why
    }
}

struct Plain {
    why: str
}

impl Error for Plain {
    fn message(self) -> str {
        return self.why
    }
}

fn inner_of<T>(e: error) -> Option<T> {
    let w: Option<Wrapped<T>> = Wrapped.as(e)
    if w != nil {
        return w.inner
    }
    return nil
}

fn main() {
    let e: error = Wrapped{ inner: 5, why: "five" }
    if e != nil {
        io.print(e.message())
    }
    let w: Option<Wrapped<int>> = Wrapped.as(e)
    if w != nil {
        io.print(w.inner + 1)
    } else {
        io.print("not an int one")
    }
    let s: Option<Wrapped<str>> = Wrapped.as(e)
    io.print(s == nil)
    let n: Option<int> = inner_of(e)
    io.print(n == 5)
    let t: Option<str> = inner_of(e)
    io.print(t == nil)
    let p: error = Plain{ why: "plain" }
    io.print(Plain.is(p))
    let q: Option<Wrapped<int>> = Wrapped.as(p)
    io.print(q == nil)
}
"#,
    ),
];

/// Programs above that need a rule of the checker's which may not have landed:
/// they are skipped while the checker still refuses them, and compared the
/// moment it accepts them. Empty now that `A(x) | B(x)` is admitted, the last
/// program that waited here.
const AWAITING_THE_CHECKER: &[&str] = &[];

fn run_on_vm(name: &str, src: &str) -> String {
    run_on_vm_at(&format!("{}.kite", name), name, src)
}

/// Compile from a real path, so a program's own modules — which are sibling
/// files and directories — can be found.
fn run_on_vm_at(path: &str, name: &str, src: &str) -> String {
    let c = compile(path, src, Emit::Check);
    assert!(
        !c.failed(),
        "{} does not compile:\n{}",
        name,
        c.render_diagnostics()
    );
    let mut out = Vec::new();
    c.run(&mut out)
        .unwrap_or_else(|t| panic!("{} trapped on the VM: {}", name, t));
    String::from_utf8(out).expect("output is valid UTF-8")
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the native backend runs on this host at all. It refuses Windows,
/// for a reason its own `supported_here` explains — so the corpus compares two
/// backends there and three everywhere else, and says which.
fn native_available() -> bool {
    kite_codegen_clif::supported_here().is_ok()
}

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run_on_native(name: &str, src: &str) -> String {
    run_on_native_at(&format!("{}.kite", name), name, src)
}

/// Compile to machine code and run in this process under the JIT — no linker,
/// which is what lets the whole corpus run natively on any machine that can
/// build the compiler.
fn run_on_native_at(path: &str, name: &str, src: &str) -> String {
    let c = compile(path, src, Emit::Native);
    assert!(
        !c.failed(),
        "{} does not compile natively:\n{}",
        name,
        c.render_diagnostics()
    );
    let program = c.native.as_ref().expect("a native program");
    let mut out = Vec::new();
    program
        .run(&mut out)
        .unwrap_or_else(|e| panic!("{} failed on the native backend: {}", name, e));
    String::from_utf8(out).expect("output is valid UTF-8")
}

fn run_on_wasm(name: &str, src: &str, dir: &std::path::Path) -> String {
    run_on_wasm_at(&format!("{}.kite", name), name, src, dir)
}

fn run_on_wasm_at(path: &str, name: &str, src: &str, dir: &std::path::Path) -> String {
    let c = compile(path, src, Emit::Wasm);
    assert!(
        !c.failed(),
        "{} does not compile to wasm:\n{}",
        name,
        c.render_diagnostics()
    );
    let module = c.wasm.as_ref().expect("a wasm module");

    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue("app.wasm"),
    )
    .expect("write glue");
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n",
    )
    .expect("write runner");

    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{} failed under node:\n{}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output is valid UTF-8")
}

/// Every shipped example must also agree, which is what stops the examples and
/// the backends drifting apart.
#[test]
fn every_example_agrees_across_backends() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let root = std::env::temp_dir().join(format!("kite-ex-{}", std::process::id()));
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let Ok(entries) = std::fs::read_dir(&examples) else {
        panic!("no examples directory at {}", examples.display());
    };

    let mut checked = 0;
    let mut total = 0;
    let mut skipped: Vec<String> = Vec::new();
    let mut mismatches = Vec::new();
    for entry in entries {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "kite") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read example");
        let dir = root.join(&name);
        std::fs::create_dir_all(&dir).expect("create work directory");

        let full = path.to_string_lossy().to_string();

        // An example that declares a host boundary cannot run here at all:
        // neither the bytecode VM nor the JIT has a network, and saying so is
        // the point of a declared boundary. Those examples are exercised where
        // a host exists — `tests/host.rs` and `tests/serve.rs`, under Node —
        // and are counted as skipped here rather than quietly dropped.
        let declared = compile(&full, &src, Emit::Wasm);
        if declared.wasm.as_ref().is_some_and(|m| !m.hosts.is_empty()) {
            skipped.push(format!("{} (needs a host)", name));
            continue;
        }

        // Every other example must at least run on the bytecode target.
        let vm = run_on_vm_at(&full, &name, &src);
        total += 1;

        // And on the native backend, which refuses nothing and needs nothing
        // installed — where it runs at all. The Wasm comparison below happens
        // either way: skipping one backend must not quietly skip the other.
        if native_available() {
            let native = run_on_native_at(&full, &name, &src);
            if vm != native {
                mismatches.push(format!(
                    "{}:\n  vm:     {:?}\n  native: {:?}",
                    name, vm, native
                ));
            }
        }

        // The Wasm target still refuses a few constructs. Those examples are
        // counted as skipped rather than silently passing, so the count below
        // fails if coverage ever goes backwards.
        let compiled = compile(&full, &src, Emit::Wasm);
        if compiled.failed() {
            skipped.push(name);
            continue;
        }

        let wasm = run_on_wasm_at(&full, &name, &src, &dir);
        if vm != wasm {
            mismatches.push(format!("{}:\n  vm:   {:?}\n  wasm: {:?}", name, vm, wasm));
        }
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&root);

    assert!(total >= 8, "only {} examples were found", total);
    assert!(
        checked >= 7,
        "only {} examples reached wasm; skipped: {:?}",
        checked,
        skipped
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn all_backends_agree() {
    let native = native_available();
    if !native {
        eprintln!(
            "skipping the native half: {}",
            kite_codegen_clif::supported_here().unwrap_err()
        );
    }
    let node = node_available();
    if !node {
        eprintln!("skipping the wasm half: node is not on PATH");
        // The modules are still built and validated by kite-codegen-wasm's own
        // tests, so this is a reduced check rather than no check — the VM and
        // the native backend still compare on every program.
    }

    let root = std::env::temp_dir().join(format!("kite-diff-{}", std::process::id()));
    let mut mismatches = Vec::new();

    for (name, src) in PROGRAMS
        .iter()
        .chain(MIDDLE_END)
        .chain(WASM_TARGET)
        .chain(NATIVE_TARGET)
        .chain(TYPE_CHECKER)
    {
        if AWAITING_THE_CHECKER.contains(name)
            && compile(format!("{}.kite", name), src, Emit::Check).failed()
        {
            eprintln!("skipping {}: the checker does not accept it yet", name);
            continue;
        }
        let vm = run_on_vm(name, src);

        if native {
            let out = run_on_native(name, src);
            if vm != out {
                mismatches.push(format!(
                    "{}:\n  vm:     {:?}\n  native: {:?}",
                    name, vm, out
                ));
            }
        }

        if node {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("create work directory");
            let wasm = run_on_wasm(name, src, &dir);
            if vm != wasm {
                mismatches.push(format!(
                    "{}:\n  vm:   {:?}\n  wasm: {:?}",
                    name, vm, wasm
                ));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&root);

    assert!(
        mismatches.is_empty(),
        "{} backend disagreement(s):\n\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}

/// A nursery of one page and an old generation small enough to be swept: the
/// collector runs between nearly every pair of allocations.
const PAGE_OF_NURSERY: kite_codegen_clif::RunConfig = kite_codegen_clif::RunConfig {
    nursery_bytes: Some(4096),
    major_threshold: Some(32 << 10),
};

/// Run natively with the collector configured, and hand back what was printed
/// and how many minor collections that took.
fn run_on_native_with(name: &str, src: &str, config: kite_codegen_clif::RunConfig) -> (String, u64) {
    let c = compile(format!("{}.kite", name), src, Emit::Native);
    assert!(
        !c.failed(),
        "{} does not compile natively:\n{}",
        name,
        c.render_diagnostics()
    );
    let mut out = Vec::new();
    let stats = c
        .native
        .as_ref()
        .expect("a native program")
        .run_with(config, &mut out)
        .unwrap_or_else(|e| panic!("{} failed on the native backend: {}", name, e));
    (String::from_utf8(out).expect("output is valid UTF-8"), stats.minor_collections)
}

/// The slice programs, natively, under a collector made to run constantly.
///
/// A slice written in place is the collector's business: once it has been
/// promoted, a young element written into it is reachable only through the
/// write barrier, and a slice grown past the nursery is born in the old
/// generation. The default nursery of a megabyte never fills for these
/// programs, so they run here too with one of a page.
#[test]
fn slices_agree_natively_with_a_page_of_nursery() {
    if !native_available() {
        eprintln!(
            "skipping: {}",
            kite_codegen_clif::supported_here().unwrap_err()
        );
        return;
    }
    let mut collections = 0;
    for name in ["slices-are-values", "slices-grow-in-place-natively"] {
        let (_, src) = WASM_TARGET
            .iter()
            .chain(NATIVE_TARGET)
            .find(|(n, _)| *n == name)
            .expect("the corpus has the program");
        let vm = run_on_vm(name, src);
        let (native, minor) = run_on_native_with(name, src, PAGE_OF_NURSERY);
        assert_eq!(native, vm, "{} with a page of nursery", name);
        collections += minor;
    }
    assert!(collections > 100, "only {} collections: the nursery did not fill", collections);
}

/// Literals longer than one `array.new_fixed` may be. V8 refuses more than
/// 10,000 operands to one when the module is *instantiated*, after the
/// validator has passed it, so a 10,001-element slice or map literal built a
/// module that `build` accepted and the browser would not load. Generated
/// rather than written out.
///
/// The native backend refused anything past the 4,096 words of its staging
/// window (E0204), so this compared the VM with Wasm alone. It builds a longer
/// literal a window at a time now. The map's last entry repeats its first key,
/// windows apart, which must still be one entry at the first position with the
/// last value — the rule each window follows within itself.
#[test]
fn literals_past_ten_thousand_elements_agree() {
    let elems: Vec<String> = (0..10_001).map(|i| i.to_string()).collect();
    let mut entries: Vec<String> = (0..10_001).map(|i| format!("{}: \"v{}\"", i, i)).collect();
    entries.push("0: \"again\"".to_string());
    let src = format!(
        "fn main() {{\n  let xs = [{}]\n  io.print(xs.len())\n  io.print(xs[10000])\n\
         \x20 let m = {{{}}}\n  io.print(m.len())\n  match m[10000] {{\n    nil => io.print(\"none\"),\n\
         \x20   v => io.print(v),\n  }}\n  match m[0] {{\n    nil => io.print(\"none\"),\n\
         \x20   v => io.print(v),\n  }}\n  io.print(m.keys()[0])\n}}\n",
        elems.join(", "),
        entries.join(", ")
    );
    let name = "literals-past-ten-thousand";
    let vm = run_on_vm(name, &src);
    assert_eq!(vm, "10001\n10000\n10001\nv10000\nagain\n0\n");
    if native_available() {
        let native = run_on_native(name, &src);
        assert_eq!(native, vm, "native");
        // A literal past the nursery's size is born in the old generation.
        let (paged, _) = run_on_native_with(name, &src, PAGE_OF_NURSERY);
        assert_eq!(paged, vm, "native, with a page of nursery");
    }
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-biglit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let wasm = run_on_wasm(name, &src, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wasm, vm, "wasm");
    }
}

/// The object-file path, through the system linker: one program built into a
/// real executable and run. The JIT above covers the codegen; this covers the
/// relocations, the symbol names and the `staticlib` runtime — the parts only
/// a linker exercises.
#[test]
fn a_linked_executable_agrees() {
    if !native_available() {
        eprintln!(
            "skipping the linked-executable check: {}",
            kite_codegen_clif::supported_here().unwrap_err()
        );
        return;
    }
    if !cc_available() {
        eprintln!("skipping the linked-executable check: `cc` is not on PATH");
        return;
    }
    let runtime = find_runtime_lib();
    let Some(runtime) = runtime else {
        eprintln!("skipping the linked-executable check: libkite_rt.a was not built");
        return;
    };

    let (name, src) = PROGRAMS
        .iter()
        .find(|(n, _)| *n == "structs")
        .expect("the corpus has a structs program");
    let vm = run_on_vm(name, src);

    let c = compile(format!("{}.kite", name), src, Emit::Native);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let object = c
        .native
        .as_ref()
        .expect("a native program")
        .object()
        .expect("an object file");

    let dir = std::env::temp_dir().join(format!("kite-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create work directory");
    let obj_path = dir.join("app.o");
    let exe_path = dir.join("app");
    std::fs::write(&obj_path, &object).expect("write object");
    let linked = Command::new("cc")
        .arg(&obj_path)
        .arg(&runtime)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .expect("cc runs");
    assert!(
        linked.status.success(),
        "linking failed:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let ran = Command::new(&exe_path).output().expect("the executable runs");
    assert!(ran.status.success(), "the executable exited nonzero");
    let native = String::from_utf8(ran.stdout).expect("output is valid UTF-8");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(vm, native, "the linked executable disagrees with the VM");
}

/// The runtime's static library, next to the test binary in the target
/// directory — where Cargo put it when it built the workspace.
fn find_runtime_lib() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..3 {
        let candidate = dir.join("libkite_rt.a");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

// ---------------------------------------------------------------------------
// Traps
// ---------------------------------------------------------------------------

/// Programs that must trap on every backend, after printing the same lines.
///
/// `all_backends_agree` insists every run finishes, so a program whose point
/// is where it stops needs a comparison of its own: the output before the
/// trap, and the fact of the trap. What each backend *says* about the trap is
/// not compared — Wasm says `unreachable` for everything.
const TRAPPING: &[(&str, &str)] = &[
    // A recursion deeper than a target allows ends in a trap on every one:
    // the VM's at a hundred thousand frames, the native runtime's at the
    // same call, and WebAssembly's wherever its host's stack ends, which is
    // sooner. Natively it ran on to the machine stack's end and aborted
    // there, and on Wasm the host's `RangeError` was not a trap at all.
    (
        "a-recursion-past-the-limit-traps",
        "\
fn depth(n: int) -> int {
  if n == 0 {
    return 0
  }
  return 1 + depth(n - 1)
}

fn main() {
  io.print(depth(1000))
  io.print(depth(200000))
  io.print(\"after\")
}
",
    ),
    // A write through a field or into a nested slice goes through a hidden
    // copy, and the bounds check has to survive the trip.
    (
        "a-nested-write-out-of-range-traps",
        "\
struct Board {
  var cells: [int]
}

fn main() {
  var b = Board{ cells: [1] }
  var grid = [[1]]
  grid[0][0] = 2
  io.print(\"before\")
  b.cells[3] = 2
  io.print(\"after\")
}
",
    ),
    // A discarded value is not a discarded check: sections 5.4 and 7.7 say
    // these trap, and lowering used to drop them along with the value.
    (
        "a-discarded-index-still-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  let xs = [1, 2, 3]
  io.print(\"before\")
  _ = xs[10]
  io.print(\"after\")
}
",
    ),
    (
        "a-discarded-division-still-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  io.print(\"before\")
  _ = id(1) / id(0)
  io.print(\"after\")
}
",
    ),
    (
        "a-bare-index-still-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  let xs = [1, 2, 3]
  io.print(\"before\")
  xs[id(5)]
  io.print(\"after\")
}
",
    ),
    (
        "a-discarded-negation-still-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  io.print(\"before\")
  _ = -id(-9223372036854775807 - 1)
  io.print(\"after\")
}
",
    ),
    // Negating `int`'s minimum overflows. Debug Wasm computed `0 - x` and
    // wrapped while the other two trapped.
    (
        "negating-the-minimum-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  io.print(\"before\")
  io.print(-id(-9223372036854775807 - 1))
}
",
    ),
    // A shift count outside 0..=63 traps in a debug build. Wasm took it
    // modulo 64 while the other two trapped.
    (
        "a-shift-past-the-width-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  io.print(id(1) << id(63))
  io.print(id(1) << id(64))
}
",
    ),
    // A slice's buffer is longer than the slice once `push` has grown it, so
    // an index has to be checked against the slice's own length: `v[1]` here
    // is inside the buffer and outside the slice.
    (
        "an-index-past-the-length-traps-within-capacity",
        "\
fn main() {
  var v: [int] = []
  v.push(1)
  io.print(v[0])
  io.print(v[1])
}
",
    ),
    (
        "a-write-past-the-length-traps-within-capacity",
        "\
fn main() {
  var v: [int] = []
  v.push(1)
  v[0] = 2
  io.print(v[0])
  v[2] = 3
  io.print(\"after\")
}
",
    ),
    // An index is an `int`; narrowing it to Wasm's i32 before the check made
    // `xs[4294967296]` read `xs[0]`.
    (
        "an-index-past-i32-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  let xs = [7, 8, 9]
  io.print(xs[id(2)])
  io.print(xs[id(4294967296)])
}
",
    ),
    (
        "a-negative-shift-traps",
        "\
fn id(x: int) -> int {
  return x
}

fn main() {
  io.print(id(16) >> id(4))
  io.print(id(16) >> id(-1))
}
",
    ),
];

/// What a run printed, and whether it ended in a trap.
type Outcome = (String, bool);

fn trap_on_vm(name: &str, src: &str) -> Outcome {
    let c = compile(format!("{}.kite", name), src, Emit::Check);
    assert!(!c.failed(), "{} does not compile:\n{}", name, c.render_diagnostics());
    let mut out = Vec::new();
    let trapped = c.run(&mut out).is_err();
    (String::from_utf8(out).expect("output is valid UTF-8"), trapped)
}

fn trap_on_wasm(name: &str, src: &str, dir: &std::path::Path) -> Outcome {
    let c = compile(format!("{}.kite", name), src, Emit::Wasm);
    assert!(!c.failed(), "{} does not compile to wasm:\n{}", name, c.render_diagnostics());
    let module = c.wasm.as_ref().expect("a wasm module");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(dir.join("app.js"), kite_driver::generate_glue("app.wasm")).expect("write glue");
    // The runner reports a trap through its exit code, and prints what the
    // program wrote before it either way.
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         let trapped = false;\n\
         try {\n\
         \x20 await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         } catch (e) {\n\
         \x20 trapped = e instanceof WebAssembly.RuntimeError;\n\
         \x20 if (!trapped) throw e;\n\
         }\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n\
         process.exitCode = trapped ? 3 : 0;\n",
    )
    .expect("write runner");
    let output = Command::new("node").arg(dir.join("run.mjs")).output().expect("node runs");
    let trapped = match output.status.code() {
        Some(0) => false,
        Some(3) => true,
        _ => panic!(
            "{} failed under node:\n{}",
            name,
            String::from_utf8_lossy(&output.stderr)
        ),
    };
    (String::from_utf8(output.stdout).expect("output is valid UTF-8"), trapped)
}

/// Where the child's program output begins and, for a run that finishes,
/// ends. A trap ends the process between the two.
const CHILD_BEGIN: &str = "\n<<kite-native-begin>>\n";
const CHILD_END: &str = "\n<<kite-native-end>>\n";

/// A native trap ends the process, exactly as it would a linked executable,
/// so each program runs in a child: this test binary again, asked through the
/// environment to run one program and nothing else.
fn trap_on_native(name: &str) -> Outcome {
    let exe = std::env::current_exe().expect("the test binary");
    let output = Command::new(exe)
        .args(["native_trap_child", "--exact", "--nocapture", "--test-threads=1"])
        .env("KITE_NATIVE_TRAP_CHILD", name)
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let Some((_, printed)) = stdout.split_once(CHILD_BEGIN) else {
        panic!(
            "{}: the native child never started:\n{}\n{}",
            name,
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    match printed.split_once(CHILD_END) {
        Some((printed, _)) => (printed.to_string(), false),
        None => {
            assert!(!output.status.success(), "{}: no end marker and a clean exit", name);
            (printed.to_string(), true)
        }
    }
}

/// The child half of [`trap_on_native`]. Does nothing unless asked.
#[test]
fn native_trap_child() {
    let Ok(name) = std::env::var("KITE_NATIVE_TRAP_CHILD") else {
        return;
    };
    let (_, src) = TRAPPING
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no trapping program {}", name));
    let c = compile(format!("{}.kite", name), src, Emit::Native);
    assert!(!c.failed(), "{} does not compile natively:\n{}", name, c.render_diagnostics());
    let program = c.native.as_ref().expect("a native program");
    // Written straight to the process's stdout, past the test harness's
    // capture: a trap writes what the program printed the same way and exits.
    let mut stdout = std::io::stdout().lock();
    use std::io::Write as _;
    stdout.write_all(CHILD_BEGIN.as_bytes()).unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    let mut out = Vec::new();
    program.run(&mut out).unwrap_or_else(|e| panic!("{} failed natively: {}", name, e));
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out).unwrap();
    stdout.write_all(CHILD_END.as_bytes()).unwrap();
    stdout.flush().unwrap();
}

#[test]
fn every_backend_traps_alike() {
    let native = native_available();
    let node = node_available();
    let root = std::env::temp_dir().join(format!("kite-traps-{}", std::process::id()));
    let mut mismatches = Vec::new();

    for (name, src) in TRAPPING {
        let vm = trap_on_vm(name, src);
        assert!(vm.1, "{} did not trap on the VM; printed {:?}", name, vm.0);

        if native {
            let out = trap_on_native(name);
            if out != vm {
                mismatches.push(format!("{}:\n  vm:     {:?}\n  native: {:?}", name, vm, out));
            }
        }
        if node {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("create work directory");
            let out = trap_on_wasm(name, src, &dir);
            if out != vm {
                mismatches.push(format!("{}:\n  vm:   {:?}\n  wasm: {:?}", name, vm, out));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    assert!(
        mismatches.is_empty(),
        "{} backend disagreement(s):\n\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}

/// Programs whose output is known, not merely agreed on.
///
/// Three backends agreeing is strong evidence, but not about a rule the
/// checker applies before any of them sees the program: a `defer` the checker
/// lowered to run once runs once on all three. These pin what the language
/// says the program prints.
const EXPECTED: &[(&str, &str, &str)] = &[
    (
        "defer-runs-per-registration",
        "fn note(s: str) {\n  io.print(s)\n}\n\
         fn fails() -> (int, error) {\n  return _, errors.new(\"boom\")\n}\n\
         fn with_check() -> (int, error) {\n  defer note(\"closed by check\")\n\
         \x20 let (v, err) = fails()\n  check err\n  return v, nil\n}\n\
         fn per_iteration() {\n  for i in 0..3 {\n    defer note(\"iteration \\(i)\")\n  }\n\
         \x20 note(\"loop done\")\n}\n\
         fn early() {\n  for i in 0..3 {\n    if i == 1 {\n      return\n    }\n\
         \x20   defer note(\"registered at \\(i)\")\n  }\n}\n\
         fn main() {\n  defer note(\"main deferred\")\n\
         \x20 let (v, err) = with_check()\n  if err != nil {\n    note(err.message())\n  }\n\
         \x20 per_iteration()\n  early()\n\
         \x20 let f = || {\n    defer note(\"closure deferred\")\n    note(\"in closure\")\n    return\n  }\n\
         \x20 f()\n  f()\n  note(\"end\")\n}\n",
        "closed by check\nboom\nloop done\niteration 2\niteration 1\niteration 0\n\
         registered at 0\nin closure\nclosure deferred\nin closure\nclosure deferred\nend\n\
         main deferred\n",
    ),
    (
        "closure-is-its-own-function",
        "struct P {\n  n: int\n}\n\
         impl P {\n  fn adder(self) -> fn(int) -> int {\n    return |x: int| x + self.n\n  }\n}\n\
         fn main() {\n  let add = P{ n: 10 }.adder()\n  io.print(add(5))\n\
         \x20 let f = |x: int| -> (int, error) {\n    if x < 0 {\n      return _, errors.new(\"neg\")\n    }\n\
         \x20   return x * 2, nil\n  }\n\
         \x20 let (v, err) = f(3)\n  if err != nil {\n    io.print(err.message())\n  } else {\n    io.print(v)\n  }\n\
         \x20 let (w, e2) = f(-1)\n  if e2 != nil {\n    io.print(e2.message())\n  } else {\n    io.print(w)\n  }\n}\n",
        "15\n6\nneg\n",
    ),
    (
        "patterns-match-what-they-name",
        "enum Color {\n  Red\n  Green\n  Blue\n}\n\
         fn name(c: Color) -> str {\n  return match c {\n    Color.Red => \"red\",\n\
         \x20   Color.Green => \"green\",\n    Color.Blue => \"blue\",\n  }\n}\n\
         fn sign(n: int) -> str {\n  return match n {\n    -5..=-1 => \"negative\",\n    0 => \"zero\",\n\
         \x20   _ => \"other\",\n  }\n}\n\
         fn main() {\n  io.print(name(Color.Green))\n  io.print(name(Color.Blue))\n\
         \x20 io.print(sign(-3))\n  io.print(sign(0))\n  io.print(sign(9))\n}\n",
        "green\nblue\nnegative\nzero\nother\n",
    ),
    (
        "compound-assignment-evaluates-once",
        "struct Counter {\n  var n: int\n}\nstruct Box {\n  var total: int\n}\n\
         fn next(var c: Counter) -> int {\n  c.n = c.n + 1\n  return c.n\n}\n\
         fn pick(var c: Counter, b: Box) -> Box {\n  c.n = c.n + 1\n  return b\n}\n\
         fn main() {\n  var c = Counter{ n: -1 }\n  var xs = [10, 20, 30]\n\
         \x20 xs[next(c)] += 1\n  io.print(\"\\(xs[0]) \\(xs[1]) \\(xs[2]) calls=\\(c.n + 1)\")\n\
         \x20 let b = Box{ total: 0 }\n  var d = Counter{ n: 0 }\n\
         \x20 pick(d, b).total += 5\n  io.print(\"\\(b.total) calls=\\(d.n)\")\n\
         \x20 var slots: [Option<int>] = [nil, nil]\n  slots[1] = 5\n  let got = slots[1]\n\
         \x20 if got != nil {\n    io.print(got + 1)\n  }\n}\n",
        "11 20 30 calls=1\n5 calls=1\n6\n",
    ),
    (
        "map-iteration-pairs-keys-with-values",
        "fn same(x: float) -> float {\n  return x\n}\n\
         fn main() {\n  var m: {float: int} = {}\n  let nan = same(0.0) / same(0.0)\n\
         \x20 m[nan] = 1\n  m[nan] = 2\n  m[1.5] = 3\n\
         \x20 for (k, v) in m {\n    io.print(v)\n  }\n\
         \x20 let words = {\"b\": 2, \"a\": 1}\n  for (w, n) in words {\n    io.print(\"\\(w)=\\(n)\")\n  }\n}\n",
        "1\n2\n3\nb=2\na=1\n",
    ),
    // A slice inside a slice used to be refused as "not a plain binding".
    // Changing one now copies each level out, changes the innermost, and
    // writes every level back — and since slices are values, a copy of the
    // outer slice taken before the write, or of a row, keeps what it had.
    (
        "nested-slices-write-back",
        r#"fn show(xs: [int]) -> str {
  var out = ""
  for x in xs {
    out = out + "\(x),"
  }
  return out
}

fn main() {
  var grid = [[1, 2], [3, 4]]
  let before = grid
  let row = grid[0]
  grid[0][1] = 9
  grid[1].push(5)
  io.print("\(show(grid[0])) \(show(grid[1])) \(show(before[0])) \(show(before[1])) \(show(row))")
  var cube = [[[1, 2], [3]], [[4], [5, 6]]]
  let copy = cube
  cube[1][1][0] = 50
  cube[0][1].push(30)
  cube[1][0][0] *= 10
  io.print("\(show(cube[1][1])) \(show(cube[0][1])) \(show(cube[1][0])) \(show(copy[1][1])) \(copy[1][0][0])")
  for i in 0..2 {
    for j in 0..2 {
      grid[i][j] += 10 * i + j
    }
  }
  io.print("\(show(grid[0])) \(show(grid[1])) \(show(before[1]))")
  var ms = [{"k": 1}]
  ms[0]["k"] = 2
  ms[0]["j"] = 3
  ms[0].remove("k")
  io.print("\(ms[0].len()) \(ms[0]["j"] == 3) \(ms[0]["k"] == nil)")
}
"#,
        "1,9, 3,4,5, 1,2, 3,4, 1,2,\n50,6, 3,30, 40, 5,6, 4\n1,10, 13,15,5, 3,4,\n1 true true\n",
    ),
    // A struct is a reference, so a slice or map in a `var` field is changed
    // by copying the field out and writing it back through the same struct —
    // which every holder of that struct then sees, while a copy of the field
    // taken before does not.
    (
        "slices-in-fields-write-back",
        r#"struct Board {
  var cells: [int]
  var rows: [[int]]
  var counts: {str: int}
}

impl Board {
  fn add(var self, n: int) {
    self.cells.push(n)
    self.rows[0][0] += n
  }
}

fn show(xs: [int]) -> str {
  var out = ""
  for x in xs {
    out = out + "\(x),"
  }
  return out
}

fn main() {
  var b = Board{ cells: [0, 0, 0], rows: [[1], [2]], counts: {"a": 1} }
  let same = b
  let cells = b.cells
  b.cells[1] = 7
  b.cells.push(4)
  b.cells[0] -= 2
  b.rows[1][0] = 20
  b.rows[0].push(11)
  b.counts["b"] = 2
  b.counts.remove("a")
  b.add(100)
  io.print("\(show(b.cells)) \(show(cells)) \(show(b.rows[0])) \(show(b.rows[1])) \(b.counts.len())")
  io.print("\(show(same.cells)) \(same.counts["b"] == 2)")
  var boards = [Board{ cells: [1], rows: [], counts: {} }]
  boards[0].cells.push(2)
  boards[0].cells[0] = 3
  io.print(show(boards[0].cells))
}
"#,
        "-2,7,0,4,100, 0,0,0, 101,11, 20, 1\n-2,7,0,4,100, true\n3,2,\n",
    ),
    // Every index and the value are evaluated once, left to right, before
    // anything is copied out — so nothing the program runs sits between the
    // copy and the write back, and a call that changes the same field is not
    // undone by it.
    (
        "place-operands-evaluate-once",
        r#"struct Counter {
  var n: int
  var log: [str]
}

struct Board {
  var cells: [int]
}

fn next(var c: Counter, what: str) -> int {
  c.n = c.n + 1
  c.log.push(what)
  return c.n
}

fn grow(var b: Board, n: int) -> int {
  b.cells.push(n)
  return n
}

fn main() {
  var c = Counter{ n: -1, log: [] }
  var sums = [[0, 0], [0, 0]]
  sums[next(c, "i")][next(c, "j")] += next(c, "v")
  io.print("\(sums[0][0]) \(sums[0][1]) \(sums[1][0]) \(c.n) \(c.log.len())")
  io.print(c.log[0] + c.log[1] + c.log[2])
  var g = Board{ cells: [0, 0] }
  g.cells[grow(g, 0)] = 9
  g.cells[1] += grow(g, 5)
  io.print("\(g.cells.len()) \(g.cells[0]) \(g.cells[1]) \(g.cells[3])")
}
"#,
        "0 2 0 2 3\nijv\n4 9 5 5\n",
    ),
    // An optional binding narrowed to a slice. `xs[0] = 5` wrote into an
    // unwrapped copy and the change went nowhere, on every backend.
    (
        "narrowed-optional-slices-change",
        r#"fn main() {
  var maybe: Option<[int]> = [1, 2]
  if maybe != nil {
    maybe[0] = 5
    maybe.push(3)
    io.print("\(maybe[0]) \(maybe.len())")
  }
  var nested: Option<[[int]]> = [[1]]
  if nested != nil {
    nested[0][0] = 8
    nested[0].push(9)
    io.print("\(nested[0][0]) \(nested[0][1])")
  }
  var table: Option<{str: int}> = {"a": 1}
  if table != nil {
    table["b"] = 2
    table.remove("a")
    io.print("\(table.len()) \(table["b"] == 2)")
  }
}
"#,
        "5 3\n8 9\n1 true\n",
    ),
];

#[test]
fn programs_print_what_the_language_says() {
    let root = std::env::temp_dir().join(format!("kite-expected-{}", std::process::id()));
    let mut wrong = Vec::new();
    for (name, src, want) in EXPECTED {
        let vm = run_on_vm(name, src);
        if vm != *want {
            wrong.push(format!("{} on the VM:\n  want: {:?}\n  got:  {:?}", name, want, vm));
        }
        if native_available() {
            let native = run_on_native(name, src);
            if native != *want {
                wrong.push(format!("{} natively:\n  want: {:?}\n  got:  {:?}", name, want, native));
            }
        }
        if node_available() {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("create work directory");
            let wasm = run_on_wasm(name, src, &dir);
            if wasm != *want {
                wrong.push(format!("{} on wasm:\n  want: {:?}\n  got:  {:?}", name, want, wasm));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}
