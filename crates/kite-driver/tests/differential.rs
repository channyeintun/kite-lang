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
        // A `for cond` narrows its body where the condition held, and what
        // follows a loop with no `break` where it failed.
        "for-condition-narrows",
        r#"fn f(n: int) -> error {
    if n > 2 {
        return errors.new("stop at \(n)")
    }
    return nil
}

fn next(n: int) -> Option<int> {
    if n > 30 {
        return nil
    }
    return n * 2
}

fn main() {
    var n = 0
    var e = f(n)
    for e == nil {
        n += 1
        e = f(n)
    }
    io.print(e.message())
    var o: Option<int> = 1
    var total = 0
    for o != nil {
        total += o
        o = next(o)
    }
    io.print(total)
    var p: Option<str> = nil
    for p == nil {
        p = "found"
    }
    io.print(p + "!")
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
        // A `remove` that finds nothing changes nothing (§3.2), identity
        // included: the VM once made the entries unique before looking, so a
        // miss gave the variable a fresh copy that `ptr.same` could see.
        "map-remove-miss-keeps-identity",
        "struct B {\n  var m: {str: int}\n}\n\
         fn drop_it(var m: {str: int}, k: str) -> {str: int} {\n  m.remove(k)\n  return m\n}\n\
         fn main() {\n\
         \x20 let m = {\"a\": 1}\n  var p = m\n  p.remove(\"absent\")\n\
         \x20 io.print(ptr.same(m, p))\n\
         \x20 io.print(ptr.same(m, drop_it(m, \"absent\")))\n\
         \x20 var b = B{ m: m }\n  b.m.remove(\"absent\")\n  io.print(ptr.same(m, b.m))\n\
         \x20 let e: {str: int} = {}\n  var q = e\n  q.remove(\"x\")\n  io.print(ptr.same(e, q))\n\
         \x20 var ms = [m]\n  ms[0].remove(\"absent\")\n  io.print(ptr.same(m, ms[0]))\n\
         \x20 p.remove(\"a\")\n  io.print(ptr.same(m, p))\n  io.print(m.len())\n  io.print(p.len())\n}\n",
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
    // A derived enum decoder was lenient about a positional payload's length
    // (`{"One":[1,2,3]}` was `One(1)`), took the first of several keys, and
    // told a unit variant written as an object that it was not "a variant
    // name or a one-key object".
    (
        "derived-json-enum-decode-is-strict",
        "use std/json\n\
         @derive(Debug, Decode)\n\
         enum Kind {\n  Plain\n  One(int)\n  Two(int, str)\n}\n\
         fn show(s: str) {\n\
         \x20 let (d, e) = json.parse(s)\n\
         \x20 if e != nil {\n    return\n  }\n\
         \x20 let (k, ke) = Kind.decode(d)\n\
         \x20 if ke != nil {\n    io.print(\"\\(s) -> ERR \\(ke.message())\")\n    return\n  }\n\
         \x20 io.print(\"\\(s) -> \\(k.debug())\")\n}\n\
         fn main() {\n\
         \x20 for s in [\"{\\\"One\\\":[1,2,3]}\", \"{\\\"One\\\":[1],\\\"Plain\\\":[]}\", \"{\\\"Plain\\\":[]}\", \
         \"{\\\"One\\\":[]}\", \"{\\\"Two\\\":[1]}\", \"\\\"One\\\"\", \"{\\\"Zed\\\":1}\", \"{}\", \
         \"{\\\"Two\\\":[1,\\\"x\\\"]}\", \"\\\"Plain\\\"\"] {\n    show(s)\n  }\n}\n",
    ),
    // `std/json` spelled like a name the derived code binds. The derive wrote
    // the module's own spelling into a `decode` whose parameter is `doc`, so
    // `doc.field(doc, …)` asked the parameter for a method, and the program
    // failed to compile with errors in `<derive>`.
    (
        "derived-json-spelled-like-its-parameter",
        "use std/json as doc\n\
         @derive(Encode, Decode, Debug)\n\
         enum Mark {\n  Plain\n  At(line: int, col: int)\n}\n\
         @derive(Encode, Decode, Debug)\n\
         struct P {\n  x: int\n  marks: [Mark]\n  note: Option<str>\n}\n\
         fn main() {\n\
         \x20 let p = P{ x: 3, marks: [Mark.Plain, Mark.At(line: 1, col: 2)], note: \"n\" }\n\
         \x20 let text = doc.stringify(p.encode())\n\
         \x20 io.print(text)\n\
         \x20 let (tree, err) = doc.parse(text)\n\
         \x20 if err != nil {\n    return\n  }\n\
         \x20 let (back, berr) = P.decode(tree)\n\
         \x20 if berr == nil {\n    io.print(back.debug())\n  }\n}\n",
    ),
    (
        "code-at-reads-a-character",
        "fn main() {\n  let s = \"h\\u{e9}llo\"\n  io.print(s.code_at(0))\n  io.print(s.code_at(1))\n\
         \x20 io.print(s.code_at(99))\n  io.print(hash_str(\"abc\") == hash_str(\"abc\"))\n\
         \x20 io.print(hash_str(\"abc\") == hash_str(\"abd\"))\n}\n",
    ),
    // Members at the margin of braces whose other members are indented. Kite's
    // indentation means nothing (§2.5); the parser's search for a missing `}`
    // once took each of these for one.
    (
        "members-at-the-margin",
        "struct Foo {\n    v: int\npub w: int\n}\n\n\
         impl Foo {\n    fn a(self) -> int {\n        return self.v\n    }\n\
         fn b(self) -> int {\n    return self.w + 1\n}\n\
         pub fn make(v: int) -> Foo {\n    return Foo{ v: v, w: v * 10 }\n}\n}\n\n\
         trait Named {\n    fn name(self) -> str\nfn loud(self) -> str\n}\n\n\
         impl Named for Foo {\n    fn name(self) -> str {\n        return \"foo\"\n    }\n\
         fn loud(self) -> str {\n    return \"FOO\"\n}\n}\n\n\
         fn main() {\n    let f = Foo.make(2)\n    io.print(f.a() + f.b())\n\
         \x20   io.print(f.name() + f.loud())\n}\n",
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
    // The module's own exports were `kite_poll` and `kite_invoke_0`, beside
    // the program's `pub fn`s, so an async program with a `pub fn kite_poll`
    // exported that name twice and was refused as a bug in the compiler
    // (E0900). They are spelled with a `$` now, which no Kite name holds.
    (
        "a-pub-fn-may-take-the-name-of-an-internal-export",
        r#"use std/task

pub fn kite_poll(n: int) -> int {
    return n + 1
}

pub fn kite_invoke_0(s: str) -> str {
    return s + "!"
}

async fn later() -> int {
    await task.sleep(1)
    return kite_poll(40)
}

async fn main() {
    await task.sleep(1)
    io.print(kite_poll(1))
    io.print(kite_invoke_0("hi"))
    io.print(await later())
}
"#,
    ),
    // `m[k] = v` and a literal's collapsing of repeated keys are helper
    // functions of their own on Wasm, one per map type, rather than loops
    // written out in the caller. Every key kind the equality they share
    // distinguishes: a string, an int, a float with `-0.0` and a NaN that
    // never finds itself, a bool, a struct and an optional.
    (
        "map-writes-and-repeated-literal-keys",
        r#"struct K {
    a: int
    b: str
}

fn show(m: {str: int}) -> str {
    var out = ""
    for k in m.keys() {
        let v = match m[k] {
            nil => -1,
            x => x,
        }
        out = out + k + "=\(v) "
    }
    return out
}

fn main() {
    let a = "x"
    let b = "y"
    var m = {a: 1, b: 2, "x": 3, "z": 4, b: 5}
    io.print(show(m))
    m["y"] = 6
    m["w"] = 7
    m["x"] = 8
    io.print(show(m))
    var n: {int: str} = {1: "one"}
    for i in 0..5 {
        n[i % 3] = "v\(i)"
    }
    io.print(n.len())
    for k in n.keys() {
        match n[k] {
            nil => io.print("missing"),
            v => io.print("\(k) \(v)"),
        }
    }
    let z = 0.0
    var f: {float: int} = {z: 1, -z: 2}
    f[0.0 / 0.0] = 3
    f[0.0 / 0.0] = 4
    io.print(f.len())
    var t: {bool: int} = {true: 1}
    t[false] = 2
    t[true] = 3
    io.print(t.len())
    var s: {K: int} = {K{ a: 1, b: "p" }: 1, K{ a: 1, b: "p" }: 2}
    s[K{ a: 2, b: "p" }] = 3
    s[K{ a: 1, b: "p" }] = 4
    io.print(s.len())
    for k in s.keys() {
        match s[k] {
            nil => io.print("missing"),
            v => io.print("\(k.a) \(k.b) \(v)"),
        }
    }
    var o: {Option<int>: str} = {nil: "none"}
    o[3] = "three"
    o[nil] = "nothing"
    io.print(o.len())
    match o[nil] {
        nil => io.print("missing"),
        v => io.print(v),
    }
}
"#,
    ),    // A map is its entries in insertion order with a hash index beside them
    // past a few entries, and nothing the index does may show: every key
    // kind `==` distinguishes, removals (which leave holes while indexed, and
    // close up when the holes outnumber the entries), re-inserts, iteration
    // after each, identity, a walk that changes the map, and a struct key
    // whose `var` field changes after it is stored — still found by what it
    // is now, and of two equal keys the earlier still wins.
    (
        "maps-are-indexed-and-keep-their-order",
        r#"use std/errors

struct P {
    x: int
    y: int
}

struct Cell {
    var n: int
}

enum Shape {
    Dot
    Circle(r: int)
    Rect(w: int, h: str)
}

fn show_int(m: { int: int }) -> str {
    var out = ""
    for (k, v) in m {
        out = out + "\(k)=\(v) "
    }
    return out
}

fn ints() {
    var m: { int: int } = { }
    for i in 0..40 {
        m[i * 7 % 41] = i
    }
    io.print(show_int(m))
    for i in 0..40 {
        if i % 3 == 0 {
            m.remove(i * 7 % 41)
        }
    }
    io.print(show_int(m))
    for i in 0..40 {
        m[i * 7 % 41] = i * 100
    }
    io.print(show_int(m))
    var misses = 0
    for i in 100..200 {
        if m[i] == nil {
            misses += 1
        }
    }
    io.print("misses \(misses) len \(m.len())")
    // Remove everything, then build again.
    for k in m.keys() {
        m.remove(k)
    }
    io.print("emptied \(m.len()) [\(show_int(m))]")
    for i in 0..20 {
        m[20 - i] = i
    }
    io.print(show_int(m))
}

fn floats() {
    var m: { float: int } = { }
    let nan = 0.0 / 0.0
    for i in 0..30 {
        m[i as float / 4.0] = i
    }
    m[nan] = 1
    m[nan] = 2
    m[-0.0] = 99
    m[0.0 - 1.5] = 7
    io.print("\(m.len()) \(or_else(m[0.0], -1)) \(or_else(m[-0.0], -1)) \(or_else(m[nan], -1))")
    m.remove(-0.0)
    m.remove(nan)
    io.print("\(m.len()) \(or_else(m[0.0], -1))")
    for (k, v) in m {
        io.print("  \(k) \(v)")
    }
}

fn strs() {
    var m: { str: int } = { }
    for i in 0..50 {
        m["k\(i % 37)é"] = i
    }
    m.remove("k3é")
    m.remove("k4é")
    m["k3é"] = 333
    var s = ""
    for (k, v) in m {
        s = s + "\(k):\(v),"
    }
    io.print(s)
    io.print("\(or_else(m["k10é"], -1)) \(or_else(m["nope"], -1)) \(m.len())")
}

fn bools() {
    var m: { bool: str } = { true: "t" }
    m[false] = "f"
    m[true] = "T"
    m.remove(false)
    m[false] = "F"
    for (k, v) in m {
        io.print("\(k) \(v)")
    }
}

fn structs() {
    var m: { P: int } = { }
    for i in 0..30 {
        m[P{ x: i % 6, y: i / 6 }] = i
    }
    m[P{ x: 2, y: 2 }] = 1000
    m.remove(P{ x: 0, y: 0 })
    var s = 0
    for (k, v) in m {
        s = s + k.x * 1000 + k.y * 10 + v
    }
    io.print("structs \(m.len()) \(s) \(or_else(m[P{ x: 2, y: 2 }], -1)) \(or_else(m[P{ x: 9, y: 9 }], -1))")
}

fn mutable() {
    // A key that changes after it is stored: a lookup by what it is now
    // finds it, and of two equal keys the earlier wins.
    var cells: [Cell] = []
    var m: { Cell: int } = { }
    for i in 0..20 {
        let c = Cell{ n: i }
        cells.push(c)
        m[c] = i
    }
    var c5 = cells[5]
    c5.n = 105
    var c9 = cells[9]
    c9.n = 105
    io.print("mutable \(or_else(m[Cell{ n: 105 }], -1)) \(or_else(m[Cell{ n: 5 }], -1)) \(m.len())")
    m[Cell{ n: 105 }] = 7
    m.remove(Cell{ n: 105 })
    io.print("after \(or_else(m[Cell{ n: 105 }], -1)) \(m.len())")
    var t = ""
    for (k, v) in m {
        t = t + "\(k.n):\(v) "
    }
    io.print(t)
}

fn enums() {
    var m: { Shape: int } = { }
    m[Dot] = 1
    for i in 0..20 {
        m[Circle(r: i)] = i
        m[Rect(w: i, h: "h\(i % 3)")] = 100 + i
    }
    m.remove(Circle(r: 4))
    m[Dot] = 2
    io.print("enums \(m.len()) \(or_else(m[Dot], -1)) \(or_else(m[Rect(w: 5, h: "h2")], -1)) \(or_else(m[Circle(r: 4)], -1))")
}

fn tuples() {
    var m: { (int, str): int } = { }
    for i in 0..30 {
        m[(i % 5, "s\(i % 7)")] = i
    }
    m.remove((1, "s1"))
    var s = ""
    for (k, v) in m {
        s = s + "\(k.0)\(k.1)=\(v) "
    }
    io.print(s)
}

fn slices() {
    var m: { [int]: int } = { }
    var key: [int] = []
    for i in 0..20 {
        key.push(i % 4)
        m[key] = i
    }
    m[[0, 1, 2]] = 99
    m.remove([0])
    io.print("slices \(m.len()) \(or_else(m[[0, 1, 2]], -1)) \(or_else(m[[0]], -1))")
}

fn optionals() {
    var m: { Option<int>: int } = { }
    for i in 0..20 {
        m[i] = i
    }
    m[nil] = -5
    m.remove(3)
    let none: Option<int> = nil
    io.print("optionals \(m.len()) \(or_else(m[none], 0)) \(or_else(m[7], 0)) \(or_else(m[3], 0))")
}

fn maps() {
    var m: { { str: int }: int } = { }
    for i in 0..20 {
        var inner: { str: int } = { }
        for j in 0..i {
            inner["x\(j)"] = j
        }
        m[inner] = i
    }
    var probe: { str: int } = { }
    for j in 0..12 {
        probe["x\(j)"] = j
    }
    io.print("maps \(m.len()) \(or_else(m[probe], -1))")
}

fn identity() {
    var m: { int: int } = { }
    for i in 0..30 {
        m[i] = i
    }
    let before = m
    m.remove(1000)
    io.print("miss keeps identity: \(ptr.same(before, m))")
    m[5] = 50
    io.print("write after sharing: \(ptr.same(before, m)) \(or_else(before[5], -1)) \(or_else(m[5], -1))")
    let again = m
    m.remove(6)
    io.print("remove after sharing: \(ptr.same(again, m)) \(again.len()) \(m.len())")
    io.print("equal: \(before == m) \(m == m)")
}

fn literals() {
    let (three, one) = (3, 1)
    let m = { three: "c", one: "a", 3: "C", 2: "b", 1: "A" }
    var s = ""
    for (k, v) in m {
        s = s + "\(k)\(v)"
    }
    io.print(s)
}

fn walk_and_change() {
    var m: { int: int } = { }
    for i in 0..20 {
        m[i] = i
    }
    for (k, v) in m {
        if k % 2 == 0 {
            m.remove(k + 1)
            m[k + 100] = v
        }
    }
    io.print(show_int(m))
}

fn errs() {
    var m: { error: int } = { }
    for i in 0..30 {
        m[errors.new("e\(i % 12)")] = i
    }
    m.remove(errors.new("e3"))
    io.print("errors \(m.len()) \(or_else(m[errors.new("e5")], -1)) \(or_else(m[errors.new("e3")], -1))")
}

fn main() {
    ints()
    errs()
    floats()
    strs()
    bools()
    structs()
    mutable()
    enums()
    tuples()
    slices()
    optionals()
    maps()
    identity()
    literals()
    walk_and_change()
}
"#,
    ),
    // What Wasm's map keeps beside its entries must not show either: a map
    // past a few entries is indexed, a removal leaves a hole there, and every
    // reader of the slots — `==`, `keys()`, `values()`, iteration, a copy, a
    // map used as a key — closes the holes up or skips them; a computed
    // literal is written entry by entry, and an equal key keeps the stored
    // one.
    (
        "a-map-with-holes-reads-as-its-entries",
        r#"fn show(m: { int: int }) -> str {
    var out = "\(m.len()):"
    for (k, v) in m {
        out = out + " \(k)=\(v)"
    }
    return out
}

fn main() {
    // Indexed, then holes: `==` against the same entries packed, and keys()
    // and values() in order.
    var m: { int: int } = { }
    for i in 0..40 {
        m[i] = i * 10
    }
    for i in 0..40 {
        if i % 3 != 0 {
            m.remove(i)
        }
    }
    var packed: { int: int } = { }
    for i in 0..40 {
        if i % 3 == 0 {
            packed[i] = i * 10
        }
    }
    io.print("\(m == packed) \(packed == m) \(m.len())")
    io.print(join_ints(m.keys(), ",") + " | " + join_ints(m.values(), ","))
    // A copy of a map with holes, then each written: neither sees the other.
    var c = m
    c[1] = 1
    m.remove(0)
    m[2] = 2
    io.print(show(c))
    io.print(show(m))
    // A map with holes as a key: found by an equal packed one.
    var outer: { { int: int }: str } = { }
    outer[m] = "holey"
    var same: { int: int } = { }
    for k in m.keys() {
        same[k] = or_else(m[k], 0)
    }
    io.print("\(or_else(outer[same], "missing")) \(outer.len())")
    // Removed down to nothing past the point of closing up, then refilled.
    for k in c.keys() {
        c.remove(k)
    }
    for i in 0..30 {
        c[30 - i] = i
    }
    io.print(show(c))
    // A computed literal naming keys twice: first position, last value.
    let a = 1
    let lit = { a: 1, 2: 2, a: 3, 4: 4, 5: 5, 6: 6, 7: 7, 8: 8, 9: 9, 10: 10, a + 1: 20, a + 10: 11 }
    io.print(show(lit))
    // A key equal to the stored one keeps the stored one.
    var f: { float: int } = { }
    for i in 0..20 {
        f[i as float] = i
    }
    f[0.0 - 0.0] = 100
    f[0.0] = 200
    var keys = ""
    for (k, v) in f {
        keys = keys + "\(k):\(v) "
    }
    io.print(keys)
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

// Every local is moved into the task's frame at a suspension and back out
// after it, its owned flag with it — and `seen` shares the slice, so the first
// push after `yield` must copy rather than write into it.
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
    // A string remembers how many characters it holds, natively, and an
    // ASCII one is indexed by byte; both the VM and the native runtime keep
    // where every sixty-fourth character starts in a long string that is not
    // ASCII, the runtime by address until a collection moves it. So
    // `slice` and `code_at` read by character at the ends, past them, across
    // a stride and across a multi-byte character, of strings both short and
    // long, ASCII and not — and of a slice and a concatenation, whose counts
    // are carried over rather than taken afresh.
    (
        "strings-index-by-character",
        r#"fn digest(s: str) -> int {
    var h = 0
    let n = s.len()
    for i in -2..n + 3 {
        h = (h * 31 + s.code_at(i)) % 1000000007
        let piece = s.slice(i, i + 3)
        h = (h * 31 + piece.len()) % 1000000007
        if piece.len() > 0 {
            h = (h * 31 + piece.code_at(piece.len() - 1)) % 1000000007
        }
    }
    h = (h * 31 + s.slice(n - 70, n + 5).len()) % 1000000007
    h = (h * 31 + s.slice(5, 2).len()) % 1000000007
    h = (h * 31 + s.slice(-10, 200).code_at(199)) % 1000000007
    return h
}

fn main() {
    var ascii = ""
    var mixed = ""
    for i in 0..300 {
        ascii = ascii + "abcdefghij".slice(i % 10, i % 10 + 1)
        if i % 7 == 0 {
            mixed = mixed + "é"
        } else {
            if i % 11 == 0 {
                mixed = mixed + "日本"
            } else {
                mixed = mixed + "x"
            }
        }
    }
    io.print("\(ascii.len()) \(mixed.len())")
    io.print("\(digest(ascii)) \(digest(mixed)) \(digest("é日")) \(digest(""))")
    io.print(mixed.slice(60, 75))
    io.print(mixed.slice(127, 130))
    io.print("\(mixed.slice(250, 400).len()) \(mixed.index_of("日本x"))")
    let both = ascii + mixed
    io.print("\(both.len()) \(both.code_at(300)) \(both.slice(299, 302))")
    let cut = mixed.slice(1, 200) + ascii.slice(0, 64)
    io.print("\(cut.len()) \(cut.code_at(198)) \(cut.code_at(199)) \(digest(cut))")
}
"#,
    ),
    // A suspension moves every local into the task's frame and back out, and
    // a slice or map brings its owned flag with it: the frame held a second
    // reference to every resumed slice, so each `push` after an `await` or a
    // `yield` copied the whole of it, and accumulating across suspensions was
    // quadratic on every backend. What shares the slice before the
    // suspension — `kept`, `snap`, `mk` — still does after it, and the first
    // write copies.
    (
        "slices-and-maps-move-across-a-suspension",
        r#"fn show(xs: [int]) -> str {
    return "[" + join_ints(xs, ",") + "]"
}

fn showm(m: { str: int }) -> str {
    var out = "{"
    for k in m.keys() {
        out = out + k + "=\(or_else(m[k], 0));"
    }
    return out + "}"
}

async fn one(i: int) -> int {
    task.yield()
    return i
}

async fn grow(var xs: [int]) -> [int] {
    xs.push(await one(9))
    return xs
}

pub async fn main() {
    var xs: [int] = [1, 2]
    let kept = xs
    task.yield()
    xs.push(3)
    task.yield()
    let after = xs
    xs.push(await one(4))
    xs[0] = 10
    io.print("\(show(kept)) \(show(after)) \(show(xs))")
    var m: { str: int } = { "a": 1 }
    let mk = m
    task.yield()
    m["b"] = 2
    let mk2 = m
    task.yield()
    m.remove("a")
    io.print("\(showm(mk)) \(showm(mk2)) \(showm(m))")
    var ys: [int] = []
    for i in 0..5 {
        ys.push(await one(i))
        if i == 2 {
            let snap = ys
            task.yield()
            ys.push(100)
            io.print("\(show(snap)) \(show(ys))")
        }
    }
    let base: [int] = [7]
    let g = await grow(base)
    io.print("\(show(base)) \(show(g))")
    var s = "a"
    for i in 0..3 {
        s = s + "b"
        task.yield()
    }
    io.print(s)
    // Enough pushes and writes across suspensions to grow many times and to
    // fill a small nursery, each written in place.
    var big: [str] = []
    var counts: { int: int } = { }
    for i in 0..3000 {
        big.push("item \(i)")
        counts[i % 17] = or_else(counts[i % 17], 0) + 1
        if i % 3 == 0 {
            task.yield()
        } else {
            big[i - 1] = big[i - 1] + "!"
            let got = await one(i)
            counts[got % 5] = or_else(counts[got % 5], 0) + 1
        }
    }
    io.print("\(big.len()) \(big[0]) \(big[1]) \(big[2999]) \(counts.len()) \(or_else(counts[3], 0))")
}
"#,
    ),
    // A write through a field goes straight into the field's slice or map
    // when nothing else holds it. Here everything else holds it once: read
    // into a binding, passed and kept, captured, stored in another field and
    // another struct, built into one with `..base`, returned from a method,
    // put in an enum payload and handed to a task, then written through the
    // field — directly, nested, through `var self`, inside a struct held in a
    // slice, and across `await` and `task.yield()`. None of them may see the
    // write. Its output is also pinned in `EXPECTED`.
    ("fields-are-written-in-place-unseen", FIELD_ALIASING),
    // A slice or map passed to a function that keeps nothing stays the
    // caller's to write in place. Here every way a callee can keep one is
    // tried, then the caller writes: returned, moved and returned, stored in
    // a struct, a slice, a map, a tuple, an optional, an enum payload, a
    // closure and a task, kept on one branch only, two calls deep, round a
    // recursion and a mutual one, through a `dyn` method, a function value
    // and a closure. None of them may see the write. Its output is also
    // pinned in `EXPECTED`.
    ("passed-slices-are-written-in-place-unseen", CALL_ALIASING),
    // A field's slice or map passed to a function between writes through
    // the field: kept by the callee, or passed to one that writes through a
    // copy of the struct, or through the struct itself two calls down, or
    // calls a closure, or prints — and passed to readers that do none of
    // these, in a `var self` method, a nested field and a struct in a slice.
    // Its output is also pinned in `EXPECTED`.
    ("passed-fields-are-written-in-place-unseen", PASSED_FIELDS),
];

/// See `toml-reads-what-it-read` in [`EXPECTED`]. Each document's parse is
/// written out in full: keys in their order with their lengths, values with
/// their types, then `toml.emit`'s text of it.
const TOML_DOCUMENTS: &str = r##"use std/toml

fn dump(v: toml.Toml) -> str {
    return match v {
        toml.Toml.Bool(b) => if b { "T" } else { "F" }
        toml.Toml.Int(n) => "I\(n)"
        toml.Toml.Float(f) => "D\(f)"
        toml.Toml.Text(s) => "S" + "\(s.len())" + ":" + s
        toml.Toml.Array(items) => dump_items(items)
        toml.Toml.Table(t) => dump_table(t)
    }
}

fn dump_items(items: [toml.Toml]) -> str {
    var out: [str] = []
    for item in items {
        out.push(dump(item))
    }
    return "[" + join(out, ",") + "]"
}

fn dump_table(t: { str: toml.Toml }) -> str {
    var out: [str] = []
    for (k, v) in t {
        out.push("\(k.len())" + ":" + k + "=" + dump(v))
    }
    return "{" + join(out, ",") + "}"
}

fn main() {
    let docs = ["", "# just a comment\n", "a = 1", "a = 1\r\nb = 2\r\n", "a = 1 # trailing\nb = 'x' # y", "\"quoted key\" = 1\n'lit key' = 2\nbare-key_1 = 3", "a.b.c = 1\na.b.d = 2\na.e = 3", "a . b = 1", "\"a.b\".c = 1", "[t]\nx = 1\n[t.u]\ny = 2\n[v]\nz = 3", "[a.b.c]\nx = 1\n[a]\ny = 2", "[a]\n[a]", "[a]\nb = 1\n[a.b]", "[a.b]\n[a]\n[a.b]", "[[fruit]]\nname = \"apple\"\n[fruit.physical]\ncolor = \"red\"\n[[fruit.variety]]\nname = \"red delicious\"\n[[fruit.variety]]\nname = \"granny\"\n[[fruit]]\nname = \"banana\"\n[[fruit.variety]]\nname = \"plantain\"", "[[a]]\n[a]", "[a]\n[[a]]", "a = [1]\n[[a]]", "a = 1\n[a]", "a = {}\n[a]", "a = { b = 1 }\n[a.c]", "a = { b = 1 }\na.c = 2", "[a]\nb.c = 1\n[a.b]", "[a]\nb.c = 1\n[a.b.d]\nx = 1", "a.b = 1\n[a]\nc = 2", "[x]\na.b = 1\n[y]\n[x.a]", "[x]\na.b = 1\n[x.a.c]", "a.b = 1\na = 2", "a = 2\na.b = 1", "a = 1\na = 2", "[[a]]\nb.c = 1\n[[a]]\nb.c = 2", "[[a]]\nx = 1\n[a.b]\ny = 2\n[[a]]\n[a.b]\nz = 3", "[[a.b]]\n[[a.b]]\n[a]\nc = 1", "[[a]]\n[[a.b]]\nx = 1\n[[a]]\n[[a.b]]\ny = 2", "[a]\n[[a.b]]\nc = 1\nd.e = 2", "a = [[1, 2], [\"x\"], [{ b = 1 }]]\n[[a]]", "a = []\n[[a]]", "t = { a = 1, b.c = 2, b.d = 3 }", "t = { a = 1, a = 2 }", "t = { a = { b = 1 }, a.c = 2 }", "t = { a.b = 1, a = 2 }", "t = { a = [1], a.b = 2 }", "t = { }", "t = {a=1}", "t = { a = 1, }", "t = { a = 1", "t = { a = 1 b = 2 }", "a = [\n 1,\n 2, # c\n]", "a = [1,,2]", "a = [", "a = ]", "s = \"tab\\there\\n\\u00e9\\U0001F600 \\\\ \\\"\"", "s = 'C:\\\\no\\\\escape'", "s = \"\"\"\nline1\nline2 \\\n   folded\"\"\"", "s = '''\nraw \\n\nx'''", "s = \"\\x\"", "s = \"open", "s = \"\"\"open", "s = '''open", "s = \"a\nb\"", "n = [0, -1, +2, 1_000, 0xdead_BEEF, 0o755, 0b1101, -0]", "f = [1.0, -3.5e2, 6.02E23, 1e-3, 0.1]", "n = 1__0", "n = _1", "n = 1_", "n = 0x", "n = 00", "n = 9223372036854775808", "n = -9223372036854775808", "f = 1.", "f = .5", "f = inf", "f = nan", "f = 1e", "b = [true, false]", "b = True", "d = [1979-05-27, 07:32:00, 1979-05-27T07:32:00Z, 1979-05-27 07:32:00.999-07:00, 1979-05-27T00:32:00.5]", "d = 1979-13-27", "d = 1979-05-27T25:00:00", "= 1", "a", "a b = 1", "a = 1 2", "[a", "[[a]", "[a]]", "[]", "[ a . b ]\nx=1", "a.= 1", ".a = 1", "a.. b = 1", "\"\" = 1", "'' = 2", "a = \"\u{1}\"", "a = 1\u{7f}", "[k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k]", "[k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k]\nx = 1", "k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k = 1", "a = [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]", "a = [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]", "[k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k.k]\nj.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j.j = 1", "t = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = { a = 1 } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } } }", "[a.\"b.c\".'d']\nx = 1", "[ \"x\" ]\n[\"x\".y]", "[a]\nb = 1\n[c]\nd = 2\n[a.e]\nf = 3", "x.y.z = 1\nx.y.w = 2\n[x.v]\nq = 1", "[s1]\na = 1\n[s2]\na = 2\n[s1.t]\nb = 1", "name = \"kite\"\n[package]\nname = \"x\"\nversion = \"0.1.0\"\n[dependencies]\nfoo = { version = \"1\", path = \"../foo\" }\nbar = \"2\"", "[a]\nx = 1\n\n\n   [b]   # comment\n   y = 2", "a = 1\n\t[b]\n\tc = 2", "[a]b = 1", "a = 1 # comment with \"quote", "[[a]]\nb = { c = 1 }\n[a.b.d]", "[[a]]\n[a.b]\n[[a.b.c]]\nx = 1\n[[a.b.c]]\nx = 2", "a = { b = [ { c = 1 } ] }\n[a.b]"]
    for d in docs {
        let (doc, err) = toml.parse(d)
        if err != nil {
            io.print("E " + err.message())
        } else {
            io.print("O " + dump(doc))
            io.print("M " + toml.emit(doc))
        }
    }
}
"##;

const TOML_DOCUMENTS_READ: &str = r##"O {}
M 
O {}
M 
O {1:a=I1}
M a = 1

O {1:a=I1,1:b=I2}
M a = 1
b = 2

O {1:a=I1,1:b=S1:x}
M a = 1
b = "x"

O {10:quoted key=I1,7:lit key=I2,10:bare-key_1=I3}
M "quoted key" = 1
"lit key" = 2
bare-key_1 = 3

O {1:a={1:b={1:c=I1,1:d=I2},1:e=I3}}
M 
[a]
e = 3

[a.b]
c = 1
d = 2

O {1:a={1:b=I1}}
M 
[a]
b = 1

O {3:a.b={1:c=I1}}
M 
["a.b"]
c = 1

O {1:t={1:x=I1,1:u={1:y=I2}},1:v={1:z=I3}}
M 
[t]
x = 1

[t.u]
y = 2

[v]
z = 3

O {1:a={1:b={1:c={1:x=I1}},1:y=I2}}
M 
[a]
y = 2

[a.b]

[a.b.c]
x = 1

E toml: `[a]` defines a table that is already defined
E toml: `a.b` is already a value, not a table
E toml: `[a.b]` defines a table that is already defined
O {5:fruit=[{4:name=S5:apple,8:physical={5:color=S3:red},7:variety=[{4:name=S13:red delicious},{4:name=S6:granny}]},{4:name=S6:banana,7:variety=[{4:name=S8:plantain}]}]}
M 
[[fruit]]
name = "apple"

[fruit.physical]
color = "red"

[[fruit.variety]]
name = "red delicious"

[[fruit.variety]]
name = "granny"

[[fruit]]
name = "banana"

[[fruit.variety]]
name = "plantain"

E toml: `a` is an array of tables, not a table
E toml: `a` is a table, not an array of tables
E toml: `a` is already a value; an array written with `[…]` cannot be appended to
E toml: `a` is already a value, not a table
E toml: `a` is already a value, not a table
E toml: `a` is already a value, not a table
E toml: `a` is already a value, not a table
E toml: `[a.b]` defines a table that is already defined
O {1:a={1:b={1:c=I1,1:d={1:x=I1}}}}
M 
[a]

[a.b]
c = 1

[a.b.d]
x = 1

E toml: `[a]` defines a table that is already defined
E toml: `[x.a]` defines a table that is already defined
O {1:x={1:a={1:b=I1,1:c={}}}}
M 
[x]

[x.a]
b = 1

[x.a.c]

E toml: `a` is set twice
E toml: `a` is already a value, not a table
E toml: `a` is set twice
O {1:a=[{1:b={1:c=I1}},{1:b={1:c=I2}}]}
M 
[[a]]

[a.b]
c = 1

[[a]]

[a.b]
c = 2

O {1:a=[{1:x=I1,1:b={1:y=I2}},{1:b={1:z=I3}}]}
M 
[[a]]
x = 1

[a.b]
y = 2

[[a]]

[a.b]
z = 3

O {1:a={1:b=[{},{}],1:c=I1}}
M 
[a]
c = 1

[[a.b]]

[[a.b]]

O {1:a=[{1:b=[{1:x=I1}]},{1:b=[{1:y=I2}]}]}
M 
[[a]]

[[a.b]]
x = 1

[[a]]

[[a.b]]
y = 2

O {1:a={1:b=[{1:c=I1,1:d={1:e=I2}}]}}
M 
[a]

[[a.b]]
c = 1

[a.b.d]
e = 2

E toml: `a` is already a value; an array written with `[…]` cannot be appended to
E toml: `a` is already a value; an array written with `[…]` cannot be appended to
O {1:t={1:a=I1,1:b={1:c=I2,1:d=I3}}}
M 
[t]
a = 1

[t.b]
c = 2
d = 3

E toml: `a` is set twice
E toml: `a` is already a value, not a table
E toml: `a` is set twice
E toml: `a` is already a value, not a table
O {1:t={}}
M 
[t]

O {1:t={1:a=I1}}
M 
[t]
a = 1

E toml: expected a key at 13
E toml: expected `,` or `}` at 11
E toml: `1 b = 2` is not a number
O {1:a=[I1,I2]}
M a = [1, 2]

E toml: expected a value at 7
E toml: unterminated array at 4
E toml: expected a value at 4
O {1:s=S15:tab	here
é😀 \ "}
M s = "tab\there\né😀 \\ \""

O {1:s=S14:C:\\no\\escape}
M s = "C:\\\\no\\\\escape"

O {1:s=S18:line1
line2 folded}
M s = "line1\nline2 folded"

O {1:s=S8:raw \n
x}
M s = "raw \\n\nx"

E toml: unknown escape `\x` at 5
E toml: unterminated string at 4
E toml: unterminated multi-line string at 7
E toml: unterminated multi-line string at 7
E toml: newline in a single-line string at 6
O {1:n=[I0,I-1,I2,I1000,I3735928559,I493,I13,I0]}
M n = [0, -1, 2, 1000, 3735928559, 493, 13, 0]

O {1:f=[D1.0,D-350.0,D6.02e+23,D0.001,D0.1]}
M f = [1.0, -350.0, 6.02e+23, 0.001, 0.1]

E toml: `1__0` is not a number
E toml: `_1` is not a number
E toml: `1_` is not a number
E toml: `0x` is not a number
E toml: `00` starts with a zero, which only `0` itself may
E toml: `9223372036854775808` does not fit in an int
O {1:n=I-9223372036854775808}
M n = -9223372036854775808

E toml: `1.` needs a digit after its point
E toml: `.5` is not a number
E toml: `inf` has no value in Kite
E toml: `nan` has no value in Kite
E toml: `1e` needs digits in its exponent
O {1:b=[T,F]}
M b = [true, false]

E toml: `True` is not a number
O {1:d=[S10:1979-05-27,S8:07:32:00,S20:1979-05-27T07:32:00Z,S29:1979-05-27 07:32:00.999-07:00,S21:1979-05-27T00:32:00.5]}
M d = [1979-05-27, 07:32:00, 1979-05-27T07:32:00Z, 1979-05-27 07:32:00.999-07:00, 1979-05-27T00:32:00.5]

E toml: `1979-13-27` is not a date or a time TOML can write
E toml: `1979-05-27T25:00:00` is not a date or a time TOML can write
E toml: expected a key at 0
E toml: expected `=` at 1
E toml: expected `=` at 2
E toml: `1 2` is not a number
E toml: expected `]` at 2
E toml: expected `]` at 4
E toml: unexpected text at 3
E toml: expected a key at 1
O {1:a={1:b={1:x=I1}}}
M 
[a]

[a.b]
x = 1

E toml: expected a key at 2
E toml: expected a key at 0
E toml: expected a key at 2
O {0:=I1}
M "" = 1

O {0:=I2}
M "" = 2

E toml: a control character in a string must be escaped, at 5
E toml: unexpected control character at 5
E toml: nested deeper than 128 at 0
E toml: nested deeper than 128 at 258
E toml: nested deeper than 128 at 0
E toml: nested deeper than 128 at 131
O {1:a=[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]}
M a = [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]

E toml: nested deeper than 128 at 202
O {1:t={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a={1:a=I1}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}
M 
[t]

[t.a]

[t.a.a]

[t.a.a.a]

[t.a.a.a.a]

[t.a.a.a.a.a]

[t.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]

[t.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a]
a = 1

O {1:a={3:b.c={1:d={1:x=I1}}}}
M 
[a]

[a."b.c"]

[a."b.c".d]
x = 1

O {1:x={1:y={}}}
M 
[x]

[x.y]

O {1:a={1:b=I1,1:e={1:f=I3}},1:c={1:d=I2}}
M 
[a]
b = 1

[a.e]
f = 3

[c]
d = 2

O {1:x={1:y={1:z=I1,1:w=I2},1:v={1:q=I1}}}
M 
[x]

[x.y]
z = 1
w = 2

[x.v]
q = 1

O {2:s1={1:a=I1,1:t={1:b=I1}},2:s2={1:a=I2}}
M 
[s1]
a = 1

[s1.t]
b = 1

[s2]
a = 2

O {4:name=S4:kite,7:package={4:name=S1:x,7:version=S5:0.1.0},12:dependencies={3:foo={7:version=S1:1,4:path=S6:../foo},3:bar=S1:2}}
M name = "kite"

[package]
name = "x"
version = "0.1.0"

[dependencies]
bar = "2"

[dependencies.foo]
version = "1"
path = "../foo"

O {1:a={1:x=I1},1:b={1:y=I2}}
M 
[a]
x = 1

[b]
y = 2

O {1:a=I1,1:b={1:c=I2}}
M a = 1

[b]
c = 2

E toml: unexpected text at 3
O {1:a=I1}
M a = 1

E toml: `a.b` is already a value, not a table
O {1:a=[{1:b={1:c=[{1:x=I1},{1:x=I2}]}}]}
M 
[[a]]

[a.b]

[[a.b.c]]
x = 1

[[a.b.c]]
x = 2

E toml: `a` is already a value, not a table
"##;

/// See `passed-fields-are-written-in-place-unseen` in [`NATIVE_TARGET`].
const PASSED_FIELDS: &str = r#"// A field's slice or map passed to a function between writes through the
// field: whatever the function kept, or saw while it ran, must not see a
// write that came after it read.

struct Q {
    var items: [int]
    var m: { str: int }
}

struct Box {
    var q: Q
}

fn sl(xs: [int]) -> str {
    return "[" + join_ints(xs, ",") + "]"
}

fn mp(m: { str: int }) -> str {
    var parts: [str] = []
    for k in m.keys() {
        parts.push("\(k)=\(or_else(m[k], 0))")
    }
    return "{" + join(parts, ",") + "}"
}

fn seen(m: { str: int }, w: str) -> bool {
    return has_key(m, w)
}

fn count(xs: [int]) -> int {
    return xs.len()
}

fn keep(xs: [int]) -> [int] {
    return xs
}

fn keep_map(m: { str: int }) -> { str: int } {
    return m
}

// Writes through a copy of the struct while it holds the field's map.
fn write_through(m: { str: int }, q: Q) -> str {
    var t = q
    t.m["w"] = 9
    t.items.push(9)
    return mp(m) + " " + mp(t.m)
}

// Writes through the struct it was given while holding its slice.
fn push_through(xs: [int], var q: Q) -> str {
    q.items.push(7)
    return sl(xs) + " " + sl(q.items)
}

fn deeper(xs: [int], q: Q) -> str {
    return push_through(xs, q)
}

fn via_closure(xs: [int], f: fn() -> int) -> int {
    return xs.len() + f()
}

fn printed(xs: [int]) -> int {
    io.print("printed \(sl(xs))")
    return xs.len()
}

impl Q {
    fn add(var self, w: str) {
        if !seen(self.m, w) {
            self.m[w] = self.m.len()
        }
        self.items.push(count(self.items))
    }

    fn add_kept(var self, w: str) -> { str: int } {
        let k = keep_map(self.m)
        self.m[w] = 1
        return k
    }
}

fn main() {
    var q = Q{ items: [1], m: { "a": 1 } }
    for i in 0..5 {
        q.add("k\(i % 3)")
    }
    io.print("add \(sl(q.items)) \(mp(q.m))")

    let k1 = keep(q.items)
    q.items.push(100)
    let k2 = q.add_kept("z")
    q.m["y"] = 2
    io.print("kept \(sl(k1)) \(mp(k2)) now \(sl(q.items)) \(mp(q.m))")

    let w1 = write_through(q.m, q)
    q.m["after"] = 3
    io.print("write_through \(w1) now \(mp(q.m))")

    let p1 = push_through(q.items, q)
    q.items.push(200)
    let p2 = deeper(q.items, q)
    q.items.push(300)
    io.print("push_through \(p1) | \(p2) now \(sl(q.items))")

    let n = 5
    let c1 = via_closure(q.items, || n)
    q.items.push(400)
    let c2 = printed(q.items)
    q.items.push(500)
    io.print("closure \(c1) \(c2) now \(sl(q.items))")

    var b = Box{ q: Q{ items: [], m: { } } }
    var log: [int] = []
    for i in 0..6 {
        if count(b.q.items) % 2 == 0 {
            b.q.items.push(i)
        } else {
            log.push(count(keep(b.q.items)))
            b.q.items.push(i * 10)
        }
        if !seen(b.q.m, "x\(i % 2)") {
            b.q.m["x\(i % 2)"] = i
        }
    }
    io.print("box \(sl(b.q.items)) \(sl(log)) \(mp(b.q.m))")

    var qs = [Q{ items: [5], m: { } }]
    for i in 0..4 {
        if count(qs[0].items) < 10 {
            qs[0].items.push(i)
        }
        let snap = keep(qs[0].items)
        qs[0].items[0] = i
        io.print("qs \(sl(snap)) \(sl(qs[0].items))")
    }
}
"#;

/// See `passed-slices-are-written-in-place-unseen` in [`NATIVE_TARGET`].
const CALL_ALIASING: &str = r#"use std/task

// A slice or map handed to a function, then written by the caller: whatever
// the function kept, in any way, must not see the write.

struct Holder {
    xs: [int]
}

struct Box {
    var xs: [int]
    var m: { str: int }
}

enum Kept {
    Slice([int])
    Table({ str: int })
}

trait Keeper {
    fn keep(self, xs: [int]) -> [int]
}

struct K {
    n: int
}

impl Keeper for K {
    fn keep(self, xs: [int]) -> [int] {
        return xs
    }
}

fn sl(xs: [int]) -> str {
    return "[" + join_ints(xs, ",") + "]"
}

fn mp(m: { str: int }) -> str {
    var parts: [str] = []
    for k in m.keys() {
        parts.push("\(k)=\(or_else(m[k], 0))")
    }
    return "{" + join(parts, ",") + "}"
}

fn id(xs: [int]) -> [int] {
    return xs
}

fn id_map(m: { str: int }) -> { str: int } {
    return m
}

fn via_var(xs: [int]) -> [int] {
    var t = xs
    let u = t
    return u
}

fn hold(xs: [int]) -> Holder {
    return Holder{ xs: xs }
}

fn wrap(xs: [int]) -> [[int]] {
    return [xs]
}

fn in_map(xs: [int]) -> { str: [int] } {
    return { "a": xs }
}

fn as_value(m: { str: int }) -> { str: { str: int } } {
    var out: { str: { str: int } } = { }
    out["m"] = m
    return out
}

fn pair(xs: [int]) -> ([int], int) {
    return (xs, xs.len())
}

fn opt(xs: [int]) -> Option<[int]> {
    if xs.len() > 100 {
        return nil
    }
    return xs
}

fn tagged(xs: [int]) -> Kept {
    return Kept.Slice(xs)
}

fn tagged_map(m: { str: int }) -> Kept {
    return Kept.Table(m)
}

fn captured(xs: [int]) -> fn() -> [int] {
    return || xs
}

fn maybe(xs: [int], keep: bool) -> [int] {
    if keep {
        return xs
    }
    return []
}

fn outer(xs: [int]) -> [int] {
    return id(xs)
}

fn outer2(xs: [int]) -> Holder {
    let ys = xs
    return hold(ys)
}

fn rec(xs: [int], n: int) -> [int] {
    if n == 0 {
        return xs
    }
    return rec(xs, n - 1)
}

fn even(xs: [int], n: int) -> [int] {
    if n == 0 {
        return xs
    }
    return odd(xs, n - 1)
}

fn odd(xs: [int], n: int) -> [int] {
    if n == 0 {
        return []
    }
    return even(xs, n - 1)
}

fn count(xs: [int], n: int) -> int {
    if n == 0 {
        return xs.len()
    }
    return count(xs, n - 1)
}

fn ping(xs: [int], n: int) -> int {
    if n == 0 {
        return xs.len()
    }
    return pong(xs, n - 1)
}

fn pong(xs: [int], n: int) -> int {
    return ping(xs, n)
}

fn into_box(var b: Box, xs: [int]) -> Box {
    b.xs = xs
    return b
}

fn put_into(var b: Box, m: { str: int }) -> Box {
    b.m = m
    return b
}

fn bump(var xs: [int]) -> int {
    xs.push(99)
    xs[0] = -1
    return xs.len()
}

fn bump_map(var m: { str: int }) -> int {
    m["zz"] = 26
    m.remove("a")
    return m.len()
}

fn first_row(g: [[int]]) -> [int] {
    return g[0]
}

fn lookup(m: { str: [int] }) -> [int] {
    return or_else(m["a"], [])
}

fn both(a: [int], b: [int]) -> [int] {
    if a.len() > 0 {
        return b
    }
    return a
}

fn reads(xs: [int]) -> int {
    var t = 0
    for x in xs {
        t += x
    }
    return t
}

fn window(xs: [int]) -> [int] {
    return xs[0..1]
}

fn keys_of(m: { str: int }) -> [str] {
    return m.keys()
}

fn show(xs: [int]) -> str {
    return join_ints(xs, "+")
}

async fn later(xs: [int]) -> int {
    task.yield()
    return xs.len()
}

async fn later_back(xs: [int]) -> [int] {
    task.yield()
    return xs
}

async fn run_tasks() {
    var xs = [1, 2]
    let t = later(xs)
    xs.push(3)
    io.print("task len \(await t) now \(sl(xs))")
    let u = later_back(xs)
    xs.push(4)
    xs[0] = 100
    io.print("task back \(sl(await u)) now \(sl(xs))")
}

async fn main() {
    var a = [1, 2]
    let a1 = id(a)
    a.push(3)
    a[0] = 10
    io.print("id \(sl(a1)) \(sl(a))")

    var b = [1, 2]
    let b1 = via_var(b)
    b.push(3)
    io.print("via_var \(sl(b1)) \(sl(b))")

    var c = [1, 2]
    let c1 = hold(c)
    c.push(3)
    c[1] = 20
    io.print("hold \(sl(c1.xs)) \(sl(c))")

    var d = [1, 2]
    let d1 = wrap(d)
    d.push(3)
    io.print("wrap \(sl(d1[0])) \(sl(d))")

    var e = [1, 2]
    let e1 = in_map(e)
    e.push(3)
    io.print("in_map \(sl(or_else(e1["a"], []))) \(sl(e))")

    var f = [1, 2]
    let f1 = pair(f)
    f.push(3)
    io.print("pair \(sl(f1.0)) \(f1.1) \(sl(f))")

    var g = [1, 2]
    let g1 = opt(g)
    g.push(3)
    io.print("opt \(sl(or_else(g1, []))) \(sl(g))")

    var h = [1, 2]
    let h1 = tagged(h)
    h.push(3)
    let h2 = match h1 {
        Slice(xs) => sl(xs),
        Table(m) => mp(m),
    }
    io.print("tagged \(h2) \(sl(h))")

    var i = [1, 2]
    let i1 = captured(i)
    i.push(3)
    io.print("captured \(sl(i1())) \(sl(i))")

    var j = [1, 2]
    let j1 = maybe(j, false)
    j.push(3)
    let j2 = maybe(j, true)
    j.push(4)
    io.print("maybe \(sl(j1)) \(sl(j2)) \(sl(j))")

    var k = [1, 2]
    let k1 = outer(k)
    k.push(3)
    let k2 = outer2(k)
    k.push(4)
    io.print("outer \(sl(k1)) \(sl(k2.xs)) \(sl(k))")

    var l = [1, 2]
    let l1 = rec(l, 5)
    l.push(3)
    let l2 = even(l, 4)
    l.push(4)
    let l3 = count(l, 7)
    l.push(5)
    let l4 = ping(l, 3)
    l.push(6)
    io.print("rec \(sl(l1)) \(sl(l2)) \(l3) \(l4) \(sl(l))")

    var bx = Box{ xs: [], m: { } }
    var n = [1, 2]
    bx = into_box(bx, n)
    n.push(3)
    var mm = { "a": 1 }
    bx = put_into(bx, mm)
    mm["b"] = 2
    io.print("box \(sl(bx.xs)) \(sl(n)) \(mp(bx.m)) \(mp(mm))")

    var o = [1, 2]
    let o1 = bump(o)
    o.push(3)
    var om = { "a": 1, "b": 2 }
    let om1 = bump_map(om)
    om["c"] = 3
    io.print("bump \(o1) \(sl(o)) \(om1) \(mp(om))")

    var rows = [[1], [2]]
    let r1 = first_row(rows)
    rows[0] = [7, 7]
    rows.push([3])
    let r2 = first_row(rows)
    rows[0].push(8)
    io.print("rows \(sl(r1)) \(sl(r2)) \(sl(rows[0])) \(rows.len())")

    var lm = { "a": [1] }
    let lm1 = lookup(lm)
    lm["a"] = [5]
    lm["b"] = [6]
    io.print("lookup \(sl(lm1)) \(lm.len())")

    var p = [1]
    var q = [2]
    let pq = both(p, q)
    q.push(3)
    p.push(4)
    let pp = both(p, p)
    p.push(5)
    io.print("both \(sl(pq)) \(sl(pp)) \(sl(p)) \(sl(q))")

    var s = [1, 2, 3]
    var tot = 0
    var w: [int] = []
    var ks: [str] = []
    var shown = ""
    var ms = { "x": 1 }
    for z in 0..5 {
        tot += reads(s)
        w = window(s)
        shown = show(s)
        s.push(z)
        s[0] = z
        ks = keys_of(ms)
        ms["k\(z)"] = z
    }
    io.print("reads \(tot) \(sl(w)) \(shown) \(sl(s)) \(join(ks, ",")) \(ms.len())")

    var t1 = { "a": 1 }
    let t2 = id_map(t1)
    t1["b"] = 2
    t1.remove("a")
    let t3 = as_value(t1)
    t1["c"] = 3
    let t4 = tagged_map(t1)
    t1["d"] = 4
    let t5 = match t4 {
        Slice(xs) => sl(xs),
        Table(m) => mp(m),
    }
    io.print("maps \(mp(t2)) \(mp(or_else(t3["m"], { }))) \(t5) \(mp(t1))")

    let kp: dyn Keeper = K{ n: 1 }
    var u = [1, 2]
    let u1 = kp.keep(u)
    u.push(3)
    let fv = id
    let u2 = fv(u)
    u.push(4)
    let cl = |ys: [int]| ys
    let u3 = cl(u)
    u.push(5)
    io.print("indirect \(sl(u1)) \(sl(u2)) \(sl(u3)) \(sl(u))")

    var big = [0]
    var kept: [[int]] = []
    for z in 0..20 {
        if z % 3 == 0 {
            kept.push(maybe(big, true))
        }
        big.push(z)
        if z % 4 == 0 {
            kept.push(rec(big, z % 3))
        }
        big[0] = z
    }
    var lines: [str] = []
    for r in kept {
        lines.push(sl(r))
    }
    io.print("loop \(join(lines, ";")) \(sl(big))")

    await run_tasks()
}
"#;

/// See `fields-are-written-in-place-unseen` in [`NATIVE_TARGET`].
const FIELD_ALIASING: &str = r#"// Every way a slice or map read out of a field can be kept, each followed by
// a write through the field: what was kept must not see the write.

struct Q {
    var items: [int]
    var other: [int]
    var m: { str: int }
}

struct Holder {
    xs: [int]
}

struct In {
    var items: [int]
    var m: { str: int }
}

struct Out {
    var b: In
}

struct Grid {
    var rows: [[int]]
}

enum Kept {
    Slice([int])
    Table({ str: int })
}

impl Q {
    fn add(var self, v: int) {
        self.items.push(v)
    }

    fn put(var self, k: str, v: int) {
        self.m[k] = v
    }

    fn snapshot(self) -> [int] {
        return self.items
    }

    fn table(self) -> { str: int } {
        return self.m
    }

    fn add_keeping(var self, v: int) -> [int] {
        let before = self.items
        self.items.push(v)
        return before
    }
}

fn sl(xs: [int]) -> str {
    return "[" + join_ints(xs, ",") + "]"
}

fn mp(m: { str: int }) -> str {
    var parts: [str] = []
    for k in m.keys() {
        parts.push("\(k)=\(or_else(m[k], 0))")
    }
    return "{" + join(parts, ",") + "}"
}

fn grid(rows: [[int]]) -> str {
    var parts: [str] = []
    for r in rows {
        parts.push(sl(r))
    }
    return join(parts, ";")
}

fn fresh() -> Q {
    return Q{ items: [1, 2], other: [], m: { "a": 1 } }
}

fn keep(xs: [int]) -> Holder {
    return Holder{ xs: xs }
}

fn grow(var q: Q, n: int) {
    for i in 0..n {
        q.items.push(i)
        q.m["k\(i)"] = i
    }
}

async fn later(xs: [int], m: { str: int }) -> str {
    task.yield()
    return "\(sl(xs)) \(mp(m))"
}

async fn across() {
    var q = fresh()
    grow(q, 3)
    let a = q.items
    let am = q.m
    task.yield()
    q.items.push(7)
    q.m["y"] = 7
    io.print("yield kept: \(sl(a)) \(mp(am)) now \(sl(q.items)) \(mp(q.m))")

    let t = later(q.items, q.m)
    q.items.push(8)
    q.m.remove("a")
    q.items[0] = 100
    io.print("task saw: \(await t) now \(sl(q.items)) \(mp(q.m))")

    let b = q.items
    let bm = q.m
    let u = later([], { })
    _ = await u
    q.items.push(9)
    q.m["z"] = 9
    io.print("await kept: \(sl(b)) \(mp(bm)) now \(sl(q.items)) \(mp(q.m))")

    // Pushed in place before a suspension, kept after, written after.
    for i in 0..4 {
        q.items.push(i)
    }
    task.yield()
    let c = q.items
    q.items.push(10)
    task.yield()
    q.items.push(11)
    io.print("in place then kept: \(sl(c)) now \(sl(q.items))")

    var o = Out{ b: In{ items: [], m: { } } }
    for i in 0..3 {
        o.b.items.push(i)
        o.b.m["n\(i)"] = i
    }
    let d = o.b.items
    let dm = o.b.m
    task.yield()
    o.b.items.push(5)
    o.b.m.remove("n0")
    io.print("nested across: \(sl(d)) \(mp(dm)) now \(sl(o.b.items)) \(mp(o.b.m))")
}

fn main() {
    // A let.
    var q = fresh()
    let a = q.items
    let am = q.m
    q.items.push(3)
    q.m["b"] = 2
    io.print("let: \(sl(a)) \(mp(am)) now \(sl(q.items)) \(mp(q.m))")

    // Grown in place first, then kept, then written.
    for i in 0..5 {
        q.items.push(i)
        q.m["g\(i)"] = i
    }
    let a2 = q.items
    let am2 = q.m
    q.items.push(99)
    q.items[0] = -1
    q.m["g0"] = -1
    q.m.remove("g1")
    io.print("grown then let: \(sl(a2)) \(mp(am2)) now \(sl(q.items)) \(mp(q.m))")

    // Passed to a function that keeps it.
    let h = keep(q.items)
    q.items.push(4)
    io.print("kept by callee: \(sl(h.xs)) now \(q.items.len())")

    // Captured by a closure.
    let snap = q.items
    let msnap = q.m
    let f = || snap.len()
    let g = || msnap.len()
    q.items.push(5)
    q.m["c"] = 3
    io.print("closure: \(f()) \(g()) now \(q.items.len()) \(q.m.len())")

    // Stored in a second field, then in a second struct.
    q.other = q.items
    q.items.push(6)
    io.print("second field: \(q.other.len()) now \(q.items.len())")
    var p = Q{ items: q.items, other: [], m: q.m }
    q.items.push(7)
    q.m["d"] = 4
    io.print("second struct: \(p.items.len()) \(p.m.len()) now \(q.items.len()) \(q.m.len())")
    p.items.push(70)
    p.m["p"] = 70
    io.print("second struct written: \(p.items.len()) \(p.m.len()) q \(q.items.len()) \(q.m.len())")

    // A struct built with `..base`.
    let r = Q{ ..q, other: [0] }
    q.items.push(8)
    q.m["e"] = 5
    io.print("..base: \(r.items.len()) \(r.m.len()) now \(q.items.len()) \(q.m.len())")

    // Returned from a method.
    let s = q.snapshot()
    let sm = q.table()
    q.add(9)
    q.put("f", 6)
    io.print("method: \(s.len()) \(sm.len()) now \(q.items.len()) \(q.m.len())")
    let before = q.add_keeping(10)
    io.print("kept inside the method: \(before.len()) now \(q.items.len())")

    // A local copy changed on its own leaves the field alone.
    var l = q.items
    l.push(11)
    var lm = q.m
    lm["own"] = 1
    io.print("local copy: \(l.len()) \(lm.len()) field \(q.items.len()) \(q.m.len())")

    // A walk over the field while the field grows.
    var walked = 0
    for x in q.items {
        if walked < 3 {
            q.items.push(x)
        }
        walked += 1
    }
    io.print("walk: \(walked) now \(q.items.len())")

    // A struct reference shares its fields: that is not a slice alias.
    var q2 = q
    q2.items.push(12)
    io.print("same struct: \(q.items.len()) \(q2.items.len())")

    // Nested fields.
    var o = Out{ b: In{ items: [1], m: { } } }
    let ob = o.b.items
    o.b.items.push(2)
    o.b.m["x"] = 1
    let obm = o.b.m
    o.b.m["y"] = 2
    io.print("nested: \(sl(ob)) \(mp(obm)) now \(sl(o.b.items)) \(mp(o.b.m))")
    let inner = o.b
    o.b.items.push(3)
    io.print("nested holder kept: \(sl(inner.items)) \(sl(o.b.items))")

    // A field of a struct held in a slice and in a map.
    var qs = [fresh(), fresh()]
    let e0 = qs[0].items
    qs[0].items.push(3)
    qs[1].m["z"] = 26
    io.print("in a slice: \(sl(e0)) \(sl(qs[0].items)) \(sl(qs[1].items)) \(mp(qs[1].m))")
    var byname: { str: In } = { "one": In{ items: [1], m: { } } }
    let one = byname["one"]
    if one != nil {
        var held = one
        let kept = held.items
        held.items.push(2)
        io.print("in a map: \(sl(kept)) \(sl(held.items)) \(byname["one"] != nil)")
    }

    // A slice of slices in a field.
    var gr = Grid{ rows: [[1], [2]] }
    let row = gr.rows[0]
    let rows = gr.rows
    gr.rows[0].push(10)
    gr.rows.push([3])
    io.print("grid: \(sl(row)) \(grid(rows)) now \(grid(gr.rows))")

    // An enum payload.
    let k = Kept.Slice(q.items)
    let km = Kept.Table(q.m)
    q.items.push(13)
    q.m["g"] = 7
    let klen = match k {
        Slice(xs) => xs.len(),
        Table(t) => t.len(),
    }
    let kmlen = match km {
        Slice(xs) => xs.len(),
        Table(t) => t.len(),
    }
    io.print("enum: \(klen) \(kmlen) now \(q.items.len()) \(q.m.len())")

    // Equality against a copy taken before.
    let eq_before = q.items
    q.items.push(14)
    io.print("equal: \(eq_before == q.items) \(eq_before.len())")

    // Kept and read again with no call between the write and the read.
    let near = q.items
    let nearm = q.m
    q.items.push(15)
    q.items[0] = 77
    q.m["h"] = 8
    q.m.remove("a")
    let n1 = near.len()
    let n2 = near[0]
    let n3 = nearm.len()
    let n4 = nearm["a"] != nil
    io.print("near: \(n1) \(n2) \(n3) \(n4) now \(q.items.len()) \(q.items[0])")

    // Kept in a loop, by a local written on one pass and read on a later one.
    var carry: [int] = []
    var carrym: { str: int } = { }
    for i in 0..6 {
        if i == 3 {
            carry = q.items
            carrym = q.m
        }
        q.items.push(i)
        q.m["L\(i)"] = i
    }
    io.print("carried: \(carry.len()) \(carrym.len()) now \(q.items.len()) \(q.m.len())")

    // The pushed value read from the field itself.
    q.items.push(q.items.len())
    q.items.push(q.items[0])
    q.m["self"] = q.m.len()
    io.print("own value: \(q.items[q.items.len() - 2]) \(q.items[q.items.len() - 1]) \(q.m["self"] != nil)")

    // A map grown through the field past the size that has an index, then
    // emptied in part — which leaves holes on Wasm — and written again,
    // each state kept on the way.
    var big = fresh()
    for i in 0..30 {
        big.put("b\(i)", i)
    }
    let full = big.m
    for i in 0..20 {
        big.m.remove("b\(i * 3 % 30)")
    }
    let holey = big.table()
    big.m["b0"] = 100
    big.put("b1", 101)
    big.m.remove("b2")
    io.print("indexed: \(full.len()) \(mp(holey)) now \(mp(big.m))")
    io.print("indexed kept: \(full == big.m) \(holey == big.m) \(or_else(holey["b0"], -1)) \(or_else(full["b3"], -1)) \(or_else(big.m["b1"], -1))")

    let t = across()
    _ = t
}
"#;

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
    // A header that writes a generic type with no arguments stands for it at
    // the block's own parameters, so `impl<X, Y> Named for Pair` is
    // `impl<X, Y> Named for Pair<X, Y>`, directly, through a bound and as a
    // `dyn`. The same header with no parameters, `impl Named for Pair`, is
    // E0208: its bodies were typed against the declaration's `A` and `B`, and
    // Wasm refused the module while the VM and native code ran it.
    (
        "a-bare-header-stands-for-the-blocks-parameters",
        r#"trait Named {
    fn name(self) -> str
}

struct Pair<A, B> {
    a: A
    b: B
}

impl<X: Display, Y: Display> Named for Pair {
    fn name(self) -> str {
        let first: X = self.a
        return "\(first)/\(self.b)"
    }
}

struct P {
    n: int
}

impl Display for P {
    fn show(self) -> str {
        return "P\(self.n)"
    }
}

fn via<T: Named>(x: T) -> str {
    return x.name()
}

fn main() {
    let p = Pair{ a: P{ n: 1 }, b: P{ n: 2 } }
    io.print(p.name())
    io.print(via(Pair{ a: P{ n: 3 }, b: P{ n: 4 } }))
    let d: dyn Named = Pair{ a: P{ n: 5 }, b: P{ n: 6 } }
    io.print(d.name())
}
"#,
    ),
    // Where an `Option<(…)>` is wanted, a tuple literal is typed against the
    // tuple inside it, as a slice or a map literal is: `(4, 1)` for an
    // `Option<(Option<int>, int)>` wraps the `4`, and then the whole. It was
    // E0200, although the bare tuple was accepted and a `T` stands where an
    // `Option<T>` is wanted.
    (
        "a-tuple-literal-is-typed-through-an-optional",
        r#"fn first(p: Option<(Option<int>, str)>) -> str {
    if p == nil {
        return "none"
    }
    let (a, s) = p
    if a == nil {
        return "nil \(s)"
    }
    return "\(a) \(s)"
}

fn make(n: int) -> Option<(Option<int>, str)> {
    if n < 0 {
        return nil
    }
    if n == 0 {
        return (nil, "zero")
    }
    return (n, "n")
}

fn main() {
    let a: (Option<int>, int) = (4, 1)
    io.print(a.1)
    let b: Option<(Option<int>, int)> = (4, 1)
    io.print(b == nil)
    if b != nil {
        io.print(b.0 == 4)
    }
    let c: Option<[Option<int>]> = [1, 2]
    io.print(c == nil)
    let d: Option<{str: Option<int>}> = {"a": 1}
    io.print(d == nil)
    io.print(first((7, "x")))
    io.print(first(nil))
    io.print(first(make(0)))
    io.print(first(make(3)))
    io.print(first(make(-1)))
}
"#,
    ),
    // `Self` inside an `impl` block is the type the block is for wherever a
    // body writes a type's name: a struct literal, a pattern, the head of an
    // associated call or of a variant, for a generic type too. It was known
    // to type annotations only, and `Self{ n: n }` was E0204.
    (
        "self-names-the-blocks-type-in-a-body",
        r#"struct P {
    n: int
}

impl P {
    fn make(n: int) -> Self {
        return Self{ n: n }
    }
    fn other() -> Self {
        return Self.make(3)
    }
    fn bump(self) -> Self {
        return Self{ n: self.n + 1 }
    }
    fn is_zero(self) -> bool {
        return match self {
            Self{ n: 0 } => true,
            _ => false,
        }
    }
}

struct Box<T> {
    v: T
}

impl<T> Box<T> {
    fn of(v: T) -> Self {
        return Self{ v: v }
    }
    fn again(self) -> Self {
        return Self.of(self.v)
    }
}

enum Light {
    On(level: int)
    Off
}

impl Light {
    fn dim(self) -> Self {
        return match self {
            Self.On(level) => if level > 1 { Self.On(level - 1) } else { Self.Off },
            Self.Off => Self.Off,
        }
    }
    fn level(self) -> int {
        return match self {
            Self.On(level) => level,
            Self.Off => 0,
        }
    }
}

trait Named {
    fn name(self) -> str
}

impl Named for P {
    fn name(self) -> str {
        let q = Self{ n: self.n * 2 }
        return "p\(q.n)"
    }
}

fn main() {
    io.print(P.make(1).n)
    io.print(P.other().n)
    io.print(P.make(4).bump().n)
    io.print(P.make(0).is_zero())
    io.print(P.make(2).is_zero())
    io.print(Box.of("x").again().v)
    io.print(Light.On(3).dim().level())
    io.print(Light.On(1).dim().level())
    io.print(P.make(5).name())
}
"#,
    ),
    // An alias of a struct or an enum is that type where its name is written:
    // an `impl` header, a literal, a variant, an associated call, a pattern,
    // through an alias of an alias. The header's block was dropped unseen, and
    // the rest were refused as "a type alias, not an enum". An alias of one
    // instantiation, `Wrapped<int>`, asks `is` and `as` about that.
    (
        "an-alias-is-its-type-where-its-name-is-written",
        r#"trait Named {
    fn name(self) -> str
}

struct Point {
    x: int
}

type Pt = Point

type Spot = Pt

impl Named for Pt {
    fn name(self) -> str {
        return "pt \(self.x)"
    }
}

impl Spot {
    fn origin() -> Spot {
        return Spot{ x: 0 }
    }
}

enum Color {
    Red
    Green(int)
}

type C = Color

fn shade(c: C) -> int {
    return match c {
        C.Red => 0,
        C.Green(n) => n,
    }
}

struct Wrapped<T> {
    inner: T
    why: str
}

impl<T> Error for Wrapped<T> {
    fn message(self) -> str {
        return "wrapped: " + self.why
    }
}

type IntWrapped = Wrapped<int>

fn via<T: Named>(x: T) -> str {
    return x.name()
}

fn main() {
    let p: Pt = Point{ x: 1 }
    io.print(p.name())
    io.print(Pt{ x: 2 }.name())
    io.print(via(Spot.origin()))
    let d: dyn Named = Spot{ x: 3 }
    io.print(d.name())
    io.print(shade(C.Red) + shade(C.Green(4)))
    io.print(C.Green(2) == Color.Green(2))
    let e: error = Wrapped{ inner: 5, why: "five" }
    if e == nil {
        return
    }
    io.print(IntWrapped.is(e))
    let w = IntWrapped.as(e)
    if w != nil {
        io.print(w.inner + 1)
    }
    let s: error = Wrapped{ inner: "s", why: "str" }
    if s != nil {
        io.print(IntWrapped.is(s))
    }
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

/// A program whose own `log` module shares its name with a package only one of
/// its dependencies declares. Both were identified as `log`, so the second to
/// load was refused as a second package of one name — on every backend, and
/// in whichever file the order of the `use` lines put it. It runs the same on
/// all three now, in either order.
#[test]
fn an_undeclared_package_and_a_program_module_of_one_name_agree() {
    let root = std::env::temp_dir().join(format!("kite-diff-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("create");
        std::fs::write(path, text).expect("write");
    };
    write("libl/kite.toml", "[package]\nname = \"log\"\nversion = \"1.0.0\"\n");
    write("libl/log.kite", "pub fn tag() -> str {\n  return \"dep-log\"\n}\n");
    write(
        "dep/kite.toml",
        "[package]\nname = \"web\"\nversion = \"1.0.0\"\n\n[dependencies]\nlog = { path = \"../libl\" }\n",
    );
    write("dep/web.kite", "use log\n\npub fn get() -> str {\n  return \"http:\" + log.tag()\n}\n");
    write(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nweb = { path = \"../dep\" }\n",
    );
    write("app/log.kite", "pub fn mine() -> str {\n  return \"app-log\"\n}\n");
    let body = "\nfn main() {\n  io.print(web.get())\n  io.print(log.mine())\n}\n";
    for (name, uses) in [("main", "use web\nuse log\n"), ("main2", "use log\nuse web\n")] {
        let src = format!("{}{}", uses, body);
        let path = root.join(format!("app/{}.kite", name));
        std::fs::write(&path, &src).expect("write");
        let full = path.to_string_lossy().to_string();
        let expected = "http:dep-log\napp-log\n";
        assert_eq!(run_on_vm_at(&full, name, &src), expected, "{} on the VM", name);
        if native_available() {
            assert_eq!(run_on_native_at(&full, name, &src), expected, "{} natively", name);
        }
        if node_available() {
            let dir = root.join(format!("wasm-{}", name));
            std::fs::create_dir_all(&dir).expect("create");
            assert_eq!(run_on_wasm_at(&full, name, &src, &dir), expected, "{} on wasm", name);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
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

/// The slice and map programs, natively, under a collector made to run
/// constantly.
///
/// A slice written in place is the collector's business: once it has been
/// promoted, a young element written into it is reachable only through the
/// write barrier, and a slice grown past the nursery is born in the old
/// generation. The default nursery of a megabyte never fills for these
/// programs, so they run here too with one of a page. A map is written in
/// place the same way, and its index moves with it. The string program is
/// here because a string's character count lives in its header, which must
/// move with it.
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
    for name in [
        "slices-are-values",
        "slices-grow-in-place-natively",
        "strings-index-by-character",
        "maps-are-indexed-and-keep-their-order",
        "slices-and-maps-move-across-a-suspension",
        "fields-are-written-in-place-unseen",
        "passed-slices-are-written-in-place-unseen",
        "passed-fields-are-written-in-place-unseen",
    ] {
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

/// Programs whose output is the scheduler's interleaving, pinned to what it
/// printed before an `await` parked on the task it awaits.
///
/// The interleaving is spawn-order sweeps, and it is observable: these print
/// from tasks that yield, sleep, time out, race, lock, queue, await one
/// another in chains and fans and spawn more tasks as they finish. Every
/// completion used to wake every awaiting task, and a scheduler that wakes
/// only the awaiters of what finished must still poll each of them exactly
/// where the old one would have found its task finished — one way of getting
/// that wrong reordered all three backends alike, which agreement cannot
/// catch and these do. The `.out` files were written by the scheduler before
/// that change. Natively they run again under a collector made to run
/// constantly, because the scheduler holds every suspended task's closure.
#[test]
fn the_scheduling_order_is_unchanged() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scheduling");
    let root = std::env::temp_dir().join(format!("kite-scheduling-{}", std::process::id()));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("tests/scheduling")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            Some(name.strip_suffix(".kite")?.to_string())
        })
        .collect();
    names.sort();
    assert!(names.len() >= 7, "only {} scheduling programs", names.len());
    let mut wrong = Vec::new();
    for name in &names {
        let src = std::fs::read_to_string(dir.join(format!("{}.kite", name))).expect("read program");
        let want = std::fs::read_to_string(dir.join(format!("{}.out", name))).expect("read output");
        let mut got = vec![("the VM", run_on_vm(name, &src))];
        if native_available() {
            got.push(("natively", run_on_native(name, &src)));
            got.push(("natively with a page of nursery", run_on_native_with(name, &src, PAGE_OF_NURSERY).0));
        }
        if node_available() {
            let work = root.join(name);
            std::fs::create_dir_all(&work).expect("create work directory");
            got.push(("on wasm", run_on_wasm(name, &src, &work)));
        }
        for (backend, out) in got {
            if out != want {
                wrong.push(format!("{} {}:\n  want: {:?}\n  got:  {:?}", name, backend, want, out));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
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

/// Literals of elements computed at run time, longer than the window MIR
/// evaluates before building one, agree on all three — natively under a
/// collector made to run constantly too.
///
/// Each element was a temporary of its own, all of them live until the
/// literal was built. Natively that was a collector root per element live
/// across every later call, and the stack maps for it quadratic: four
/// thousand interpolated strings took a gigabyte to compile, and eight
/// thousand made more machine code than the JIT could place, which panicked.
/// Past the window each element is pushed, or each entry set, as soon as it is
/// computed, reusing the temporaries of the one before. That is `m[k] = v`
/// for a map, so a key named twice must still be one entry at its first
/// position with its last value; and an element may be an `if`, a struct, a
/// literal of its own, or an `await` that suspends between two elements.
#[test]
fn computed_literals_past_a_window_agree() {
    let n = 400;
    let list = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>().join(", ");
    let src = format!(
        "use std/task\n\n\
         struct P {{\n  a: int\n  b: str\n}}\n\n\
         fn key(i: int) -> str {{\n  return \"k\\(i)\"\n}}\n\n\
         fn build(n: int) {{\n\
         \x20 let strs = [{}]\n\
         \x20 io.print(\"\\(strs.len()) \\(strs[0]) \\(strs[{last}])\")\n\
         \x20 let ps = [{}]\n\
         \x20 io.print(\"\\(ps.len()) \\(ps[{last}].a) \\(ps[{last}].b)\")\n\
         \x20 let picked = [{}]\n\
         \x20 io.print(\"\\(picked[0]) \\(picked[1]) \\(picked[{last}])\")\n\
         \x20 let grid = [{}]\n\
         \x20 io.print(\"\\(grid.len()) \\(grid[{last}][1])\")\n\
         \x20 let m = {{{}}}\n\
         \x20 io.print(\"\\(m.len()) \\(m.keys()[0]) \\(m.values()[0]) \\(m.keys()[99])\")\n}}\n\n\
         async fn later(i: int) -> int {{\n  task.yield()\n  return i * 2\n}}\n\n\
         async fn waited() -> [int] {{\n  return [{}]\n}}\n\n\
         async fn main() {{\n  build(0)\n  let w = await waited()\n\
         \x20 var total = 0\n  for x in w {{\n    total = total + x\n  }}\n\
         \x20 io.print(\"\\(w.len()) \\(total)\")\n}}\n",
        list(&|i| format!("\"s\\(n + {})\"", i)),
        list(&|i| format!("P{{ a: n + {}, b: key({}) }}", i, i)),
        list(&|i| format!("if (n + {}) % 2 == 0 {{ key({}) }} else {{ \"odd\" }}", i, i)),
        list(&|i| format!("[n + {}, n + {}]", i, i + 1)),
        list(&|i| format!("\"k\\((n + {}) % 100)\": n + {}", i, i)),
        (0..100).map(|i| format!("await later({})", i)).collect::<Vec<_>>().join(", "),
        last = n - 1,
    );
    let name = "computed-literals-past-a-window";
    let vm = run_on_vm(name, &src);
    assert_eq!(vm, "400 s0 s399\n400 399 k399\nk0 odd odd\n400 400\n100 k0 300 k99\n100 9900\n");
    if native_available() {
        assert_eq!(run_on_native(name, &src), vm, "native");
        let (paged, minor) = run_on_native_with(name, &src, PAGE_OF_NURSERY);
        assert_eq!(paged, vm, "native, with a page of nursery");
        assert!(minor > 10, "only {} collections: the nursery did not fill", minor);
    }
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-computed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let wasm = run_on_wasm(name, &src, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wasm, vm, "wasm");
    }
}

/// A literal of twenty thousand interpolated strings compiles for the VM and
/// for Wasm, and the two agree. As a temporary per element it needed a frame
/// of 80,010 registers and a function of 60,015 locals, and both targets
/// refused it (E0902). Natively it is the program above, longer.
#[test]
fn a_literal_past_either_frame_limit_compiles() {
    let elems: Vec<String> = (0..20_000).map(|i| format!("\"s\\(n + {})\"", i)).collect();
    let src = format!(
        "fn main() {{\n  var n = 0\n  let xs = [{}]\n  io.print(\"\\(xs.len()) \\(xs[19999])\")\n}}\n",
        elems.join(", ")
    );
    let name = "a-literal-past-either-frame-limit";
    let vm = run_on_vm(name, &src);
    assert_eq!(vm, "20000 s19999\n");
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-wide-literal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let wasm = run_on_wasm(name, &src, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wasm, vm, "wasm");
    }
}

/// A literal of a hundred and twenty thousand strings runs under V8 as it does
/// on the VM. Written inline it was one function of 1.9 MB, which the
/// validator accepts and V8 cannot compile: the engine aborted the process
/// ("a single code object needs more than half of the code space"). A chunk of
/// constants is a function of its own now (see the Wasm backend's `pieces`),
/// and a local is stored in place: one in every chunk, as here, once kept all
/// the constants inline and aborted V8 just the same. The names cycle through
/// five, so the chunks are not all one piece; the map's chunks are pieces too.
/// The native backend is left out: it is not what this is about, and it takes
/// ten seconds to compile the literal in a debug build.
#[test]
fn a_literal_of_strings_too_long_for_one_v8_function_runs() {
    let names = ["ant", "bee", "cat", "dog", "eel"];
    let elems: Vec<String> = (1..120_000)
        .map(|i| if i % 4_000 == 0 { "first".to_string() } else { format!("\"{}\"", names[i % 5]) })
        .collect();
    let entries: Vec<String> = (0..5_000).map(|i| format!("\"k{}\": \"v{}\"", i, i)).collect();
    let src = format!(
        "fn main() {{\n  let first = \"local\"\n  let xs = [first, {}]\n\
         \x20 io.print(\"\\(xs.len()) \\(xs[0]) \\(xs[1]) \\(xs[4096]) \\(xs[8000]) \\(xs[119999])\")\n\
         \x20 let m = {{{}}}\n  io.print(\"\\(m.len()) \\(m.keys()[4999]) \\(m.values()[4096])\")\n}}\n",
        elems.join(", "),
        entries.join(", ")
    );
    let name = "a-literal-too-long-for-one-v8-function";
    let vm = run_on_vm(name, &src);
    assert_eq!(vm, "120000 local bee bee local eel\n5000 k4999 v4096\n");
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-v8-literal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let wasm = run_on_wasm(name, &src, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wasm, vm, "wasm");
    }
}

/// A literal of seventy thousand constants runs on the VM as it does on the
/// other two. The VM staged a literal's every element in a register of its
/// own, so a slice of 70,000 constants was a frame of 70,007 registers and
/// E0902, while 70,000 computed elements — built as they go — ran. It stages
/// a window of them now and adds the rest. The map's last entry names its
/// first key again, so the rule a key named twice follows must hold across
/// what was staged and what was added.
#[test]
fn a_literal_of_constants_past_the_vms_frame_runs() {
    let elems: Vec<String> = (0..70_000).map(|i| i.to_string()).collect();
    let mut entries: Vec<String> = (0..3_000).map(|i| format!("\"k{}\": {}", i, i)).collect();
    entries.push("\"k0\": -1".to_string());
    let src = format!(
        "fn main() {{\n  let xs = [{}]\n  io.print(\"\\(xs.len()) \\(xs[0]) \\(xs[69999])\")\n\
         \x20 let m = {{{}}}\n  io.print(\"\\(m.len()) \\(m.values()[0]) \\(m.keys()[0]) \\(m.values()[2999])\")\n}}\n",
        elems.join(", "),
        entries.join(", ")
    );
    let name = "a-literal-of-constants";
    let vm = kite_driver::on_compiler_stack({
        let src = src.clone();
        move || run_on_vm(name, &src)
    });
    assert_eq!(vm, "70000 0 69999\n3000 -1 k0 2999\n");
    if native_available() {
        let native = kite_driver::on_compiler_stack({
            let src = src.clone();
            move || run_on_native(name, &src)
        });
        assert_eq!(native, vm, "native");
    }
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-constant-literal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let wasm = kite_driver::on_compiler_stack({
            let dir = dir.clone();
            move || run_on_wasm(name, &src, &dir)
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(wasm, vm, "wasm");
    }
}

/// Long chains compile and agree: a table of three hundred `else if`, a text
/// of two thousand pieces joined with `+`, three hundred `||` and three
/// hundred method calls in a row. The parser once charged each link against
/// the 256 levels nesting may go, and refused every one of these — programs
/// the compiler had always compiled.
///
/// Compiled on the stack `kitec` gives the compiler: every pass recurses as
/// deep as a chain is long, and a debug build's type checker spends thirty
/// kilobytes a method call, which no test thread has.
#[test]
fn long_chains_agree_across_backends() {
    let mut branches = String::from("  if c == 0 {\n    io.print(0)\n  }");
    for i in 1..300 {
        branches.push_str(&format!(" else if c == {} {{\n    io.print({})\n  }}", i, i));
    }
    let pieces: Vec<String> = (0..2000).map(|i| format!("\"{},\"", i)).collect();
    let terms: Vec<String> = (0..300).map(|i| format!("c == {}", 1000 + i)).collect();
    let src = format!(
        "struct Tally {{\n  n: int\n}}\n\nimpl Tally {{\n  fn up(self) -> Tally {{\n\
         \x20   return Tally{{ n: self.n + 1 }}\n  }}\n}}\n\n\
         fn main() {{\n  let c = 150\n{}\n  let s = {}\n  io.print(s.len())\n\
         \x20 let far = {}\n  io.print(far)\n  let t = Tally{{ n: 0 }}{}\n  io.print(t.n)\n}}\n",
        branches,
        pieces.join(" + "),
        terms.join(" || "),
        ".up()".repeat(300)
    );
    let name = "long-chains";
    let outputs = kite_driver::on_compiler_stack(move || {
        let vm = run_on_vm(name, &src);
        let native = native_available().then(|| run_on_native(name, &src));
        let wasm = node_available().then(|| {
            let dir = std::env::temp_dir().join(format!("kite-chains-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create work directory");
            let out = run_on_wasm(name, &src, &dir);
            let _ = std::fs::remove_dir_all(&dir);
            out
        });
        (vm, native, wasm)
    });
    let (vm, native, wasm) = outputs;
    assert_eq!(vm, "150\n8890\nfalse\n300\n");
    if let Some(native) = native {
        assert_eq!(native, vm, "native");
    }
    if let Some(wasm) = wasm {
        assert_eq!(wasm, vm, "wasm");
    }
}

/// An `else if` chain nearly as long as the parser takes compiles, and in
/// memory in proportion to it.
///
/// Pruning after specialisation walked each function body through a copy,
/// made afresh at every block — so each block was copied once for every
/// block around it, and an `else if` chain is one block inside the next. A
/// chain of 4,000 took gigabytes to check, and one of 8,000 was killed for
/// memory though the parser had accepted it.
#[test]
fn an_else_if_chain_at_the_ceiling_compiles() {
    let mut branches = String::from("  if c == 0 {\n    io.print(0)\n  }");
    for i in 1..8000 {
        branches.push_str(&format!(" else if c == {} {{\n    io.print({})\n  }}", i, i));
    }
    let src = format!("fn main() {{\n  let c = 7999\n{}\n}}\n", branches);
    let out = kite_driver::on_compiler_stack(move || run_on_vm("else-if-ceiling", &src));
    assert_eq!(out, "7999\n");
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

/// The runtime's static library, from the target directory the test binary
/// was built into — the newest one there.
///
/// Cargo writes the archive under `deps/` with a hash in its name and copies
/// it up beside the binaries only on some builds, so the copy without a hash
/// can be older than the runtime the code generator was just built from — a
/// change of profile flags is enough to leave it behind. Linking against a
/// stale runtime fails on every symbol added since, which reads as a broken
/// backend rather than a stale file, so every candidate is looked at and the
/// most recently written wins.
fn find_runtime_lib() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut consider = |path: std::path::PathBuf| {
        let Ok(written) = std::fs::metadata(&path).and_then(|m| m.modified()) else { return };
        if best.as_ref().is_none_or(|(when, _)| written > *when) {
            best = Some((written, path));
        }
    };
    for _ in 0..3 {
        consider(dir.join("libkite_rt.a"));
        if let Ok(entries) = std::fs::read_dir(dir.join("deps")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("libkite_rt-") && name.ends_with(".a") {
                    consider(entry.path());
                }
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
    best.map(|(_, path)| path)
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
    // A `buffer.F64` checks its shape. A push of the wrong length was
    // accepted and shifted every record after it, and a field past the
    // stride read the neighbouring record: both silent, on every backend.
    (
        "a-ragged-buffer-push-traps",
        "\
use std/buffer

fn main() {
  var b = buffer.f64(2)
  b = buffer.push(b, [1.0, 2.0])
  io.print(buffer.count(b))
  b = buffer.push(b, [1.0, 2.0, 3.0])
  io.print(\"after\")
}
",
    ),
    (
        "a-buffer-field-outside-the-stride-traps",
        "\
use std/buffer

fn main() {
  var b = buffer.f64(2)
  b = buffer.push(b, [1.0, 2.0])
  b = buffer.push(b, [3.0, 4.0])
  io.print(buffer.get(b, 0, 1))
  io.print(buffer.get(b, 0, 2))
  io.print(\"after\")
}
",
    ),
    // A race of no tasks has no winner. It waited forever, until the
    // scheduler gave up on every task without saying which call was at
    // fault; now it is a `require` at the call.
    (
        "a-race-of-no-tasks-traps",
        "\
use std/task

async fn go() {
  let none: [Task<int>] = []
  io.print(\"before\")
  io.print(await task.race(none))
}

fn main() {
  _ = go()
}
",
    ),
    // The one gcd an `int` cannot hold is 2⁶³, and `math.gcd` says so rather
    // than overflowing as it negates.
    (
        "a-gcd-of-two-to-the-63-traps",
        "\
use std/math

fn main() {
  io.print(math.gcd(math.min_int(), 6))
  io.print(math.gcd(math.min_int(), 0))
}
",
    ),
    (
        "a-ragged-row-in-from-records-traps",
        "\
use std/buffer

fn main() {
  io.print(buffer.count(buffer.from_records(2, [[1.0, 2.0]])))
  io.print(buffer.count(buffer.from_records(2, [[1.0, 2.0], [3.0]])))
}
",
    ),
    (
        "a-negative-buffer-field-traps",
        "\
use std/buffer

fn main() {
  var b = buffer.f64(2)
  b = buffer.push(b, [1.0, 2.0])
  b = buffer.push(b, [3.0, 4.0])
  b = buffer.set(b, 1, -1, 9.0)
  io.print(buffer.get(b, 0, 1))
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
    // `_` stands for no value, so the error beside it has to be a failure.
    // One that is nil at run time traps at the `return`. It used to leave a
    // hole the caller was allowed to read: the VM trapped at the read, native
    // code printed a zero or crashed on a `str`, and Wasm printed a zero or
    // dereferenced null. `a_nil_error_beside_a_hole_traps_with_one_message`
    // compares what each says.
    (
        "a-nil-error-beside-a-hole-traps",
        "\
fn lookup(found: bool) -> error {
  if found {
    return nil
  }
  return errors.new(\"missing\")
}

fn fetch(found: bool) -> (str, error) {
  return _, lookup(found)
}

fn main() {
  let (_, err) = fetch(false)
  if err != nil {
    io.print(\"failed: \" + err.message())
  }
  io.print(\"before\")
  let (s, serr) = fetch(true)
  if serr != nil {
    return
  }
  io.print(\"len \\(s.len())\")
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
    trap_on_wasm_with_stderr(name, src, dir).0
}

/// [`trap_on_wasm`], and what the run wrote to standard error.
fn trap_on_wasm_with_stderr(name: &str, src: &str, dir: &std::path::Path) -> (Outcome, String) {
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
    (
        (String::from_utf8(output.stdout).expect("output is valid UTF-8"), trapped),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Where the child's program output begins and, for a run that finishes,
/// ends. A trap ends the process between the two.
const CHILD_BEGIN: &str = "\n<<kite-native-begin>>\n";
const CHILD_END: &str = "\n<<kite-native-end>>\n";

/// A native trap ends the process, exactly as it would a linked executable,
/// so each program runs in a child: this test binary again, asked through the
/// environment to run one program and nothing else.
fn trap_on_native(name: &str) -> Outcome {
    trap_on_native_with_stderr(name).0
}

/// [`trap_on_native`], and what the child wrote to standard error.
fn trap_on_native_with_stderr(name: &str) -> (Outcome, String) {
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    match printed.split_once(CHILD_END) {
        Some((printed, _)) => ((printed.to_string(), false), stderr),
        None => {
            assert!(!output.status.success(), "{}: no end marker and a clean exit", name);
            ((printed.to_string(), true), stderr)
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

/// `return _, err` with a nil `err` traps at the `return`, and here what each
/// backend *says* is compared too: it is one `require` the checker wrote, so
/// there is one message, and it names the mistake rather than whatever the
/// caller happened to do with the hole next.
#[test]
fn a_nil_error_beside_a_hole_traps_with_one_message() {
    const MESSAGE: &str = "`return _, err` was handed a nil error: `_` stands for no value, \
                           so the error beside it must be a failure";
    let name = "a-nil-error-beside-a-hole-traps";
    let (_, src) = TRAPPING.iter().find(|(n, _)| *n == name).expect("the program");
    let printed = "failed: missing\nbefore\n".to_string();

    let c = compile(format!("{}.kite", name), src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    let trap = c.run(&mut out).expect_err("a trap on the VM");
    assert_eq!(String::from_utf8(out).unwrap(), printed);
    assert_eq!(trap.to_string(), MESSAGE);

    if native_available() {
        let (outcome, stderr) = trap_on_native_with_stderr(name);
        assert_eq!(outcome, (printed.clone(), true));
        assert!(stderr.contains(&format!("error: {}\n", MESSAGE)), "native said:\n{}", stderr);
    }
    if node_available() {
        let dir = std::env::temp_dir().join(format!("kite-nil-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create work directory");
        let (outcome, stderr) = trap_on_wasm_with_stderr(name, src, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(outcome, (printed, true));
        assert!(stderr.contains(&format!("{}\n", MESSAGE)), "wasm said:\n{}", stderr);
    }
}

/// `task.race` of no tasks traps at the call, saying so. It used to wait
/// forever for a winner, until the scheduler trapped with "2 tasks can never
/// make progress" — true, and no help finding the call.
#[test]
fn a_race_of_no_tasks_names_the_mistake() {
    const MESSAGE: &str = "task.race of no tasks: there is nothing to finish first";
    let name = "a-race-of-no-tasks-traps";
    let (_, src) = TRAPPING.iter().find(|(n, _)| *n == name).expect("the program");
    let c = compile(format!("{}.kite", name), src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    let trap = c.run(&mut out).expect_err("a trap on the VM");
    assert_eq!(String::from_utf8(out).unwrap(), "before\n");
    assert_eq!(trap.to_string(), MESSAGE);
    if native_available() {
        let (outcome, stderr) = trap_on_native_with_stderr(name);
        assert_eq!(outcome, ("before\n".to_string(), true));
        assert!(stderr.contains(&format!("error: {}\n", MESSAGE)), "native said:\n{}", stderr);
    }
}

/// Programs whose output is known, not merely agreed on.
///
/// Three backends agreeing is strong evidence, but not about a rule the
/// checker applies before any of them sees the program: a `defer` the checker
/// lowered to run once runs once on all three. These pin what the language
/// says the program prints.
const EXPECTED: &[(&str, &str, &str)] = &[
    (
        // §2.4: the closing line's indentation comes off each line, as much
        // of it as the line starts with. A tab where the delimiter has spaces
        // is not that indentation and stays — with a hole and without.
        "a-dedent-takes-off-the-closing-lines-indentation-only",
        "fn main() {\n  let n = 1\n\
         \x20 let a = \"\"\"\n\tx\n      y\n  z\n        w\n        \"\"\"\n\
         \x20 let b = \"\"\"\n\tx\\(n)\n      y\n        \"\"\"\n\
         \x20 io.print(\"[\\(a)]\")\n  io.print(\"[\\(b)]\")\n}\n",
        "[\tx\ny\nz\nw]\n[\tx1\ny]\n",
    ),
    (
        // A block string whose only line is the closing one has no lines:
        // the opening break goes and the closing line goes with its
        // indentation. It was the indentation, eight spaces of it — in a
        // body and in a module constant alike.
        "an-empty-block-string-is-empty",
        "let E = \"\"\"\n    \"\"\"\n\
         fn main() {\n  let a = \"\"\"\n        \"\"\"\n\
         \x20 let b = \"\"\"   \n\t\"\"\"\n\
         \x20 io.print(\"[\\(a)] \\(a.len()) [\\(b)] [\\(E)] \\(E.len())\")\n}\n",
        "[] 0 [] [] 0\n",
    ),
    (
        // An escaped quote in a block string is text, so it cannot be the
        // first quote of the closing `"""` (§2.4) — with a hole in the
        // string and without, and inside a hole's own block string.
        "an-escaped-quote-does-not-close-a-block-string",
        "fn main() {\n  let n = 2\n\
         \x20 let a = \"\"\"\nsay \\\"\"\"\"\n\
         \x20 let b = \"\"\"\n    a \\\"\"\" b \\(n)\n    \"\"\"\n\
         \x20 let c = \"\"\"x \\(\"\"\"in \\\"\"\"\") y\"\"\"\n\
         \x20 io.print(\"[\\(a)] [\\(b)] [\\(c)]\")\n}\n",
        "[say \"] [a \"\"\" b 2] [x in \" y]\n",
    ),
    (
        // A hole is an ordinary expression (§2.4), and that includes a block
        // string: the lexer ended the outer one at the inner one's `"""`.
        "a-block-string-in-a-hole-of-a-block-string",
        "fn wrap(s: str) -> str {\n  return \"(\\(s))\"\n}\n\
         fn main() {\n  io.print(\"\"\"p \\(\"\"\"in\"\"\") q\"\"\")\n\
         \x20 io.print(\"\"\"p \\(wrap(\"\"\"a\"b\"\"\")) q\"\"\")\n\
         \x20 io.print(\"\"\"x\\\\(y\"\"\")\n  let n = 3\n\
         \x20 let block = \"\"\"\n    first \\(wrap(\"\"\"(\\(n))\"\"\"))\n    second\n    \"\"\"\n\
         \x20 io.print(block)\n}\n",
        "p in q\np (a\"b) q\nx\\(y\nfirst ((3))\nsecond\n",
    ),
    (
        // Writing to a key that is already there changes the value and keeps
        // the stored key. `-0.0 == 0.0`, so the two keys are one, and Wasm
        // alone wrote the new key over the old: iterating the map printed
        // `-0.0` there and `0.0` on the VM and natively.
        "an-equal-key-keeps-the-stored-one",
        "fn z() -> float {\n  return -0.0\n}\n\
         fn main() {\n  var m = {0.0: \"first\"}\n  m[-0.0] = \"second\"\n\
         \x20 m[1.5] = \"third\"\n  m[1.5] = \"fourth\"\n\
         \x20 for (k, v) in m {\n    io.print(\"\\(k) -> \\(v)\")\n  }\n  io.print(m.len())\n\
         \x20 var n = {-0.0: 1}\n  n[0.0] = 2\n\
         \x20 for (k, v) in n {\n    io.print(\"\\(k) -> \\(v)\")\n  }\n\
         \x20 let l = {0.0: \"a\", z(): \"b\"}\n\
         \x20 for (k, v) in l {\n    io.print(\"\\(k) -> \\(v)\")\n  }\n}\n",
        "0.0 -> second\n1.5 -> fourth\n2\n-0.0 -> 2\n0.0 -> b\n",
    ),
    (
        // §4.2 allows an operator applied to constants, and `as` is one
        // (§5.1): a constant's cast folds by the rule the same cast in a body
        // runs by, truncating and saturating.
        "a-cast-in-a-constant-folds-as-it-runs",
        "let A = 3 as float\nlet B = -7.9 as int\nlet C = (A * 2.0) as int + 1\n\
         let D = \"\\(2 as float)\"\nlet E = 1e300 as int\n\
         fn main() {\n  io.print(A)\n  io.print(B)\n  io.print(C)\n  io.print(D)\n  io.print(E)\n\
         \x20 io.print(-7.9 as int)\n  io.print(1e300 as int)\n}\n",
        "3.0\n-7\n7\n2.0\n9223372036854775807\n-7\n9223372036854775807\n",
    ),
    (
        // §5.4: a window clamps, so `..=max_int` is to the end. Its `+ 1`
        // trapped in a debug build and wrapped to an empty window in a
        // release one; it saturates now, which is the same in both, and an
        // end bound for reuse still runs after the sequence and the start.
        "an-inclusive-window-to-max-int-clamps",
        "fn big() -> int {\n  io.print(\"end\")\n  return 9223372036854775807\n}\n\
         fn seq() -> [int] {\n  io.print(\"seq\")\n  return [1, 2, 3, 4]\n}\n\
         fn from() -> int {\n  io.print(\"start\")\n  return 1\n}\n\
         fn main() {\n  let xs = [1, 2, 3, 4]\n  let end = 9223372036854775807\n\
         \x20 io.print(xs[1..=end].len())\n  io.print(\"abcd\"[1..=end])\n\
         \x20 io.print(xs[1..=9223372036854775807].len())\n\
         \x20 io.print(seq()[from()..=big()].len())\n\
         \x20 io.print(xs[..=end].len())\n  io.print(xs[1..=2].len())\n  io.print(\"abcd\"[0..=1])\n\
         \x20 let two = 2\n  io.print(xs[1..=two - 1].len())\n}\n",
        "3\nbcd\n3\nseq\nstart\nend\n3\n4\n2\nab\n1\n",
    ),
    (
        // §5.3: a struct literal's fields and a variant's named payload run in
        // the order written, and `..base` runs first and once. They ran in
        // declaration order, and the base ran again for every field it
        // supplied — in a generic function, a closure and across `await` too.
        "constructor-fields-run-in-the-order-written",
        "struct Box<T> {\n  a: T\n  b: T\n  label: str\n}\n\
         enum Shape {\n  Rect(w: int, h: int)\n  Dot\n}\n\
         fn say(s: str) -> str {\n  io.print(s)\n  return s\n}\n\
         fn n(x: int) -> int {\n  io.print(\"n \\(x)\")\n  return x\n}\n\
         fn base<T>(v: T) -> Box<T> {\n  io.print(\"base\")\n  return Box{ a: v, b: v, label: \"base\" }\n}\n\
         fn rebox<T>(v: T, w: T) -> Box<T> {\n  return Box{ ..base(v), b: w }\n}\n\
         async fn later(x: int) -> int {\n  io.print(\"later \\(x)\")\n  return x\n}\n\
         async fn main() {\n\
         \x20 let r = rebox(1, 2)\n  io.print(\"\\(r.a) \\(r.b) \\(r.label)\")\n\
         \x20 let f = |k: int| Box{ label: say(\"closure\"), b: n(k), a: n(k + 1) }\n\
         \x20 let g = f(5)\n  io.print(\"\\(g.a) \\(g.b)\")\n\
         \x20 let t = later(9)\n  let u = later(8)\n\
         \x20 let h = Box{ b: await t, a: await u, label: \"x\" }\n  io.print(\"\\(h.a) \\(h.b)\")\n\
         \x20 let all = Box{ ..base(\"q\"), label: say(\"l\"), b: say(\"b\"), a: say(\"a\") }\n\
         \x20 io.print(all.label)\n\
         \x20 let s = Rect(h: n(1), w: n(2))\n\
         \x20 match s {\n    Rect(w, h) => io.print(\"\\(w)x\\(h)\"),\n    Dot => io.print(\"dot\"),\n  }\n}\n",
        "base\n1 2 base\nclosure\nn 5\nn 6\n6 5\nlater 9\nlater 8\n8 9\nbase\nl\nb\na\nl\nn 1\nn 2\n2x1\n",
    ),
    (
        // §5.3: the fields `..base` supplies are read before any written
        // field runs. A struct is a reference, so a written field whose call
        // changed the base through a `var` parameter leaked into the copy:
        // both lines printed `99 7 99`, whichever field was declared first.
        "base-fields-are-read-before-the-written-ones-run",
        "struct Q {\n  var a: int\n  b: int\n}\n\
         struct P {\n  b: int\n  var a: int\n}\n\
         fn bump(var q: Q, s: str) -> int {\n  io.print(s)\n  q.a = 99\n  return 7\n}\n\
         fn bump_p(var p: P) -> int {\n  io.print(\"bump p\")\n  p.a = 99\n  return 7\n}\n\
         fn pick(q: Q) -> Q {\n  io.print(\"base\")\n  return q\n}\n\
         fn main() {\n  var q = Q{ a: 1, b: 2 }\n  let r = Q{ ..pick(q), b: bump(q, \"b\") }\n\
         \x20 io.print(\"\\(r.a) \\(r.b) \\(q.a)\")\n\
         \x20 var p = P{ b: 2, a: 1 }\n  let s = P{ ..p, b: bump_p(p) }\n\
         \x20 io.print(\"\\(s.a) \\(s.b) \\(p.a)\")\n}\n",
        "base\nb\n1 7 99\nbump p\n1 7 99\n",
    ),
    (
        // §5.3: operands run in the order written and are fixed then. A
        // local was named, not copied, so it was read when the value was
        // built: a later operand's match arm block that assigned it changed
        // an earlier argument, field, element, payload, key or capture.
        "an-operand-is-fixed-where-it-is-written",
        "struct Q {\n  u: ()\n  y: int\n}\nenum E {\n  V(u: (), y: int)\n}\n\
         fn g(x: int, u: (), z: int) -> int {\n  return x * 100 + z\n}\n\
         fn n(x: int) -> int {\n  io.print(\"n \\(x)\")\n  return x\n}\n\
         fn main() {\n\
         \x20 var a = 1\n  let r = g(a, match a {\n    _ => {\n      io.print(\"arm\")\n      a = 7\n    }\n  }, n(a))\n\
         \x20 io.print(\"\\(r) \\(a)\")\n\
         \x20 var b = 1\n  let q = Q{ y: b, u: match b {\n    _ => {\n      io.print(\"arm\")\n      b = 7\n    }\n  } }\n\
         \x20 io.print(\"\\(q.y) \\(b)\")\n\
         \x20 var c = 1\n  let t = (c, match c {\n    _ => {\n      io.print(\"arm\")\n      c = 7\n    }\n  }, n(c))\n\
         \x20 io.print(\"\\(t.0) \\(t.2) \\(c)\")\n\
         \x20 var d = 1\n  let e = V(y: d, u: match d {\n    _ => {\n      d = 9\n    }\n  })\n\
         \x20 match e {\n    V(u, y) => io.print(\"\\(y) \\(d)\"),\n  }\n\
         \x20 var k = \"k\"\n  let m = {k: match k {\n    _ => {\n      k = \"j\"\n    }\n  }}\n\
         \x20 io.print(\"\\(m.keys()[0]) \\(k)\")\n\
         \x20 var w = 1\n  let f = |x: int, u: ()| x * 10\n\
         \x20 io.print(f(w, match w {\n    _ => {\n      w = 5\n    }\n  }))\n}\n",
        "arm\nn 7\n107 7\narm\n1 7\narm\nn 7\n1 7 7\n1 9\nk j\n10\n",
    ),
    (
        // §5.4: `target op= value` evaluates the target's operands and the
        // value first, then reads the target, for a local and a field as for
        // an element. A field was read before the value ran, so `c.count +=
        // incr(c)` ignored what `incr` stored, unlike `xs[0] += …`.
        "a-compound-assignment-reads-its-target-last",
        "struct Counter {\n  var count: int\n}\n\
         struct Holder {\n  var items: [int]\n}\n\
         fn g(x: int, u: ()) -> int {\n  return x\n}\n\
         fn incr(var c: Counter, s: str) -> int {\n  io.print(s)\n  c.count = 50\n  return 1\n}\n\
         fn set(var h: Holder) -> int {\n  io.print(\"element\")\n  h.items[0] = 50\n  return 1\n}\n\
         fn make(c: Counter) -> Counter {\n  io.print(\"target\")\n  return c\n}\n\
         fn main() {\n\
         \x20 var x = 1\n  x += g(1, match x {\n    _ => {\n      io.print(\"arm\")\n      x = 5\n    }\n  })\n  io.print(x)\n\
         \x20 var c = Counter{ count: 0 }\n  make(c).count += incr(c, \"value\")\n  io.print(c.count)\n\
         \x20 var h = Holder{ items: [0] }\n  h.items[0] += set(h)\n  io.print(h.items[0])\n\
         \x20 var e = Counter{ count: 0 }\n  e.count = e.count + incr(e, \"plain\")\n  io.print(e.count)\n}\n",
        "arm\n6\ntarget\nvalue\n51\nelement\n51\nplain\n1\n",
    ),
    (
        // §5.4: `xs[i] = v` stores into the slice the local holds when the
        // store happens. Fixing operands where they are written put the base
        // in a temporary too, so a value that reassigned `xs` lost the write
        // into a copy (`7 8`); and `ys[0] += v` reads the element after `v`
        // has run, as `n += v` does, where it read it first (`11 8`).
        "an-indexed-store-lands-in-the-slice-the-local-holds",
        "fn one(u: ()) -> int {\n  return 1\n}\n\
         fn main() {\n\
         \x20 var xs = [10, 20]\n  xs[0] = one(match 0 {\n    _ => {\n      xs = [7, 8]\n    }\n  })\n\
         \x20 io.print(\"\\(xs[0]) \\(xs[1])\")\n\
         \x20 var ys = [10, 20]\n  ys[0] += one(match 0 {\n    _ => {\n      ys = [7, 8]\n    }\n  })\n\
         \x20 io.print(\"\\(ys[0]) \\(ys[1])\")\n\
         \x20 var zs = [10, 20]\n  zs[0] += one(match 0 {\n    _ => {\n      io.print(\"arm\")\n    }\n  })\n\
         \x20 io.print(zs[0])\n}\n",
        "1 8\n8 8\narm\n11\n",
    ),
    (
        // §2.4, in a file written with CRLF line endings: a line break in the
        // source is a line break however it was written, so no line of a
        // block string ends in `\r` — with a hole, without one, in a
        // constant, and with the closing delimiter at the end of a line of
        // text. Spaces after an opening `"""` go with its line break (`t`):
        // they once kept it, and the string began with a newline.
        "block-strings-in-a-crlf-file",
        "let K = \"\"\"\r\n  k1\r\n  k2\r\n  \"\"\"\r\n\
         fn codes(s: str) -> str {\r\n  var out = \"\"\r\n  for i in 0..s.len() {\r\n\
         \x20   out = out + \" \\(s.code_at(i))\"\r\n  }\r\n  return out\r\n}\r\n\
         fn main() {\r\n  let n = 7\r\n\
         \x20 let a = \"\"\"\r\n    a\r\n    b\r\n    \"\"\"\r\n\
         \x20 let h = \"\"\"\r\n    n=\\(n)\r\n    m\r\n    \"\"\"\r\n\
         \x20 let t = \"\"\" \r\n    sp\r\n    \"\"\"\r\n\
         \x20 let u = \"\"\"\r\n    x\r\n    y\"\"\"\r\n\
         \x20 io.print(codes(a))\r\n  io.print(codes(h))\r\n  io.print(codes(t))\r\n\
         \x20 io.print(codes(u))\r\n  io.print(codes(K))\r\n}\r\n",
        " 97 10 98\n 110 61 55 10 109\n 115 112\n 32 32 32 32 120 10 32 32 32 32 121\n 107 49 10 107 50\n",
    ),
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
    // A type parameter inside a fallible function type is solved by the
    // argument. Inference stopped at `-> (T, error)`, so neither helper could
    // be called at all.
    (
        "parameters-solve-through-fallible-functions",
        r#"struct Job<T> {
  run: fn() -> (T, error)
}
fn seven() -> (int, error) {
  return 7, nil
}
fn label(n: int) -> (str, error) {
  return "n\(n)", nil
}
fn retry<T>(g: fn() -> (T, error)) -> (T, error) {
  let (v, err) = g()
  check err
  return v, nil
}
fn each<T, U>(xs: [T], g: fn(T) -> (U, error)) -> ([U], error) {
  var out: [U] = []
  for x in xs {
    let (v, err) = g(x)
    check err
    out.push(v)
  }
  return out, nil
}
fn main() {
  let (a, e1) = retry(seven)
  if e1 == nil {
    io.print(a)
  }
  let (xs, e2) = each([1, 2], label)
  if e2 == nil {
    io.print(xs[1])
  }
  let job = Job{ run: seven }
  let (b, e3) = retry(job.run)
  if e3 == nil {
    io.print(b + 1)
  }
}
"#,
        "7\nn2\n8\n",
    ),
    // A default method knows `Self` only by the trait's methods, so its
    // `self.hi()` is the implementation's `hi` — directly, through a bound and
    // through a `dyn`. It used to see an inherent `hi` too, and run it; a type
    // can no longer have both (E0112).
    (
        "a-default-calls-the-traits-own-methods",
        r#"trait Greet {
  fn hi(self) -> str
  fn twice(self) -> str {
    return self.hi() + self.hi()
  }
}
struct P {
  n: int
}
impl Greet for P {
  fn hi(self) -> str {
    return "[greet \(self.n)]"
  }
}
struct Q {
  n: int
}
impl Greet for Q {
  fn hi(self) -> str {
    return "[q]"
  }
}
fn twice_of<T: Greet>(x: T) -> str {
  return x.twice()
}
fn main() {
  let d: dyn Greet = P{ n: 1 }
  io.print(d.twice())
  io.print(P{ n: 2 }.twice())
  io.print(twice_of(P{ n: 5 }))
  let all: [dyn Greet] = [P{ n: 6 }, Q{ n: 0 }]
  for g in all {
    io.print(g.twice())
  }
}
"#,
        "[greet 1][greet 1]\n[greet 2][greet 2]\n[greet 5][greet 5]\n\
         [greet 6][greet 6]\n[q][q]\n",
    ),
    // A bound's generic method whose signature mentions `Self`, called from
    // a default (where `Self` is a parameter) and from a generic function
    // whose first parameter is the bound one. The receiver's type was written
    // into the signature in the caller's numbering, where it collided with
    // the method's own `U`: E0209 "conflicting types for U".
    (
        "a-generic-method-mentioning-self-through-a-bound",
        r#"trait Pipe {
  fn val(self) -> int
  fn conv<U>(self, o: Self, u: U) -> U
  fn pipe<U>(self, f: fn(Self) -> U) -> U {
    return f(self)
  }
  fn text(self) -> str {
    return self.pipe(|y: Self| "v\(y.val())") + self.conv(self, "s")
  }
}
struct P {
  n: int
}
impl Pipe for P {
  fn val(self) -> int {
    return self.n
  }
  fn conv<U>(self, o: P, u: U) -> U {
    return u
  }
}
fn run<T: Pipe>(x: T) -> str {
  return x.conv(x, "r") + x.pipe(|y: T| "\(y.val())")
}
fn main() {
  io.print(P{ n: 5 }.text())
  io.print(run(P{ n: 7 }))
}
"#,
        "v5s\nr7\n",
    ),
    // A closure is typed by the place it is used: through an `Option<fn …>`
    // (an argument, a binding, a field, a fallible one), with `-> (T, error)`
    // written on it as the fallible form wherever it stands, and — passed
    // where a generic `fn() -> T` or `fn() -> (T, error)` is wanted — with a
    // block body's result learned from its `return`s. Each of these was
    // E0211 or E0200.
    (
        "a-closure-is-typed-by-where-it-is-used",
        r#"struct Hooks {
  on_load: Option<fn() -> (int, error)>
  on_step: Option<fn(int) -> int>
}
struct Job<T> {
  run: fn() -> (T, error)
}
fn apply(f: Option<fn(int) -> int>, x: int) -> int {
  if f != nil {
    return f(x)
  }
  return x
}
fn run<T>(g: fn() -> T) -> T {
  return g()
}
fn retry<T>(g: fn() -> (T, error)) -> (T, error) {
  let (v, err) = g()
  check err
  return v, nil
}
fn main() {
  io.print(apply(|n| n * 3, 2))
  let f: Option<fn(int) -> int> = |n| n + 1
  if f != nil {
    io.print(f(2))
  }
  let h = Hooks{ on_load: || { return 1, nil }, on_step: |x| x + 10 }
  let step = h.on_step
  if step != nil {
    io.print(step(2))
  }
  let load = h.on_load
  if load != nil {
    let (n, e) = load()
    if e == nil {
      io.print(n)
    }
  }
  let g: fn() -> (bool, error) = || -> (bool, error) { return true, nil }
  let (b, e1) = g()
  if e1 == nil {
    io.print(b)
  }
  let j = Job{ run: || -> (str, error) { return "job", nil } }
  let (s, e2) = j.run()
  if e2 == nil {
    io.print(s)
  }
  io.print(run(|| {
    return 6
  }))
  let (v, e3) = retry(|| {
    if false {
      return _, errors.new("no")
    }
    return 7, nil
  })
  if e3 == nil {
    io.print(v)
  }
}
"#,
        "6\n3\n12\n1\ntrue\njob\n6\n7\n",
    ),
    // A closure given to a generic struct's field is typed as a generic
    // call's argument is: its parameters from the fields that fix them, and
    // a block body's result from its `return`s, fallible where the field is.
    // `Job{ run: || { return true, nil } }` was E0209, nothing pinning `T`.
    (
        "a-closure-in-a-generic-literal-solves-its-type",
        r#"struct Job<T> {
  run: fn() -> (T, error)
}
struct Holder<T> {
  f: fn(T) -> int
  v: T
}
struct Gen<T> {
  g: fn() -> T
}
fn main() {
  let j = Job{ run: || {
    return true, nil
  } }
  let (b, e) = j.run()
  if e == nil {
    io.print(b)
  }
  let h = Holder{ f: |x| x + 1, v: 3 }
  io.print(h.f(h.v))
  let g = Gen{ g: || {
    return "s"
  } }
  io.print(g.g())
}
"#,
        "true\n4\ns\n",
    ),
    // A closure literal that needs what a later argument fixes is typed after
    // the call's other arguments (§4.5): `r2(|| { return _, err }, 6)` was
    // E0209 and `ap(|n| n + 1, 41)` E0211, though the `6` and the `41` fix
    // `T`. The same holds for a generic struct literal's fields. The
    // arguments are still evaluated in the order written.
    (
        "a-closure-learns-what-a-later-argument-fixes",
        r#"use std/errors
fn r2<T>(g: fn() -> (T, error), x: T) -> (T, error) {
  return g()
}
fn ap<T>(f: fn(T) -> T, x: T) -> T {
  return f(x)
}
fn three<T>(a: int, f: fn(T) -> int, b: T) -> int {
  return a + f(b)
}
fn tick(label: str, n: int) -> int {
  io.print(label)
  return n
}
struct Job<T> {
  run: fn() -> (T, error)
  seed: T
}
struct Map2<A, B> {
  f: fn(A) -> B
  seed: A
}
struct Box2 {
  n: int
}
impl Box2 {
  fn with<T>(self, f: fn(T) -> int, x: T) -> int {
    return f(x) + self.n
  }
}
fn main() {
  let (b, e) = r2(|| {
    return _, errors.new("no")
  }, 6)
  if e != nil {
    io.print(e.message())
  } else {
    io.print(b)
  }
  io.print(ap(|n| n + 1, 41))
  io.print(ap(|s| "\(s)!", "hi"))
  io.print(three(tick("first", 1), |t| t * 10, tick("third", 2)))
  io.print(ap(|n| ap(|m| m - 1, n), 9))
  io.print(Box2{ n: 10 }.with(|t| t * 3, 2))
  let j = Job{ run: || {
    return _, errors.new("job")
  }, seed: 6 }
  let (v, je) = j.run()
  if je != nil {
    io.print("\(je.message()) \(j.seed)")
  } else {
    io.print(v)
  }
  let m = Map2{ f: |a| a + 1, seed: 3 }
  io.print(m.f(m.seed))
}
"#,
        "no\n42\nhi!\nfirst\nthird\n21\n8\n16\njob 6\n4\n",
    ),
    // The place a closure stands in is looked through `Option` (§4.5), for
    // waiting as for being told what is known: `ap_opt(|n| n + 1, 41)`
    // against `f: Option<fn(T) -> T>` was E0211, and the fallible form two
    // errors, though the same calls against a plain `fn(T) -> T` compiled.
    (
        "a-closure-for-an-optional-function-learns-what-a-later-argument-fixes",
        r#"use std/errors
fn ap_opt<T>(f: Option<fn(T) -> T>, x: T) -> T {
  if f == nil {
    return x
  }
  return f(x)
}
fn ap_opt_r<T>(x: T, f: Option<fn(T) -> T>) -> T {
  return ap_opt(f, x)
}
fn r3<T>(g: Option<fn() -> (T, error)>, x: T) -> T {
  if g == nil {
    return x
  }
  let (v, err) = g()
  if err != nil {
    return x
  }
  return v
}
fn made<T>(g: Option<fn() -> T>) -> int {
  return 1
}
struct Job<T> {
  run: Option<fn(T) -> T>
  seed: T
}
fn main() {
  io.print(ap_opt(|n| n + 1, 41))
  io.print(ap_opt_r(41, nil))
  io.print(r3(|| {
    return _, errors.new("y")
  }, 6))
  io.print(r3(|| {
    return 7, nil
  }, 6))
  io.print(made(|| {
    return "s"
  }))
  let j = Job{ run: |n| n * 2, seed: 5 }
  let run = j.run
  if run != nil {
    io.print(run(j.seed))
  }
}
"#,
        "42\n41\n6\n7\n1\n10\n",
    ),
    // What E0114's notes tell the reader to write, written: a `var` copy and
    // a captured `let` of the result; a copy made inside the closure; a
    // `var` given its value where it is declared; a copy of a loop's or a
    // pattern's binding. The notes said "declare it `var`" for each, which
    // is E0211, E0110 or a parse error.
    (
        "e0114-notes-followed-compile",
        r#"fn main() {
  let xs = [1]
  var copy = xs
  copy.push(2)
  let done = copy
  let f = || {
    io.print(done.len())
  }
  f()
  let g = || {
    let ys = [1]
    let h = || {
      var copy = ys
      copy.push(2)
      io.print(copy.len())
    }
    h()
  }
  g()
  var zs: [int] = [1]
  zs.push(2)
  io.print(zs.len())
  for ys in [[1]] {
    var copy = ys
    copy.push(2)
    io.print(copy.len())
  }
  let (ws, n) = ([1], 2)
  var wcopy = ws
  wcopy.push(n)
  io.print(wcopy.len())
}
"#,
        "2\n2\n2\n2\n2\n",
    ),
    // A named `async fn` used as a value is its starter, a function that
    // returns the task: passed where `fn(int) -> Task<int>` is wanted, bound
    // and called, or handed to `task.map`. It was typed `fn(int) -> int`, so
    // the first two were refused, and called through a binding it answered
    // a task typed as an `int`.
    (
        "an-async-fn-as-a-value-returns-its-task",
        r#"use std/task
async fn dbl(x: int) -> int {
  return x * 2
}
fn apply(f: fn(int) -> Task<int>, x: int) -> Task<int> {
  return f(x)
}
async fn main() {
  let t = apply(dbl, 4)
  io.print(await t)
  let f = dbl
  io.print(await f(5))
  let all = await task.map([1, 2, 3], dbl)
  io.print(join_ints(all, ","))
}
"#,
        "8\n10\n2,4,6\n",
    ),
    // A derived `Encode` writes a tuple as an array of its elements in order,
    // in a field, a slice, a nested tuple and a payload, and `Decode` reads
    // back an array of exactly its length. Both used to refuse a tuple.
    (
        "a-tuple-derives-encode-and-decode",
        r#"use std/json

@derive(Encode, Decode, Debug)
struct Inner {
    n: int
}

@derive(Encode, Decode)
struct P {
    t: (int, str)
    nested: [(bool, Option<float>)]
    deep: (Inner, (int, int))
}

@derive(Encode, Decode)
enum E {
    Pt(at: (int, int))
    Bare((str, int))
}

fn main() {
    let p = P{ t: (1, "a"), nested: [(true, nil), (false, 2.5)], deep: (Inner{ n: 7 }, (8, 9)) }
    let text = json.stringify(p.encode())
    io.print(text)
    let (doc, err) = json.parse(text)
    if err == nil {
        let (q, e2) = P.decode(doc)
        if e2 == nil {
            io.print("\(q.t.0) \(q.t.1) \(q.nested.len()) \(q.deep.0.n) \(q.deep.1.1)")
        }
    }
    for bad in ["{\"t\":[1],\"nested\":[],\"deep\":[{\"n\":1},[1,2]]}", "{\"t\":[1,2],\"nested\":[],\"deep\":[{\"n\":1},[1,2]]}"] {
        let (d, e3) = json.parse(bad)
        if e3 == nil {
            let (_, e4) = P.decode(d)
            if e4 != nil {
                io.print(e4.message())
            }
        }
    }
    io.print(json.stringify(E.Pt(at: (1, 2)).encode()))
    io.print(json.stringify(E.Bare(("x", 3)).encode()))
    let (ed, e5) = json.parse("{\"Bare\":[[\"y\",4]]}")
    if e5 == nil {
        let (ev, e6) = E.decode(ed)
        if e6 == nil {
            match ev {
                E.Bare(b) => io.print("\(b.0)\(b.1)")
                E.Pt(at) => io.print(at.0)
            }
        }
    }
}

"#,
        r#"{"t":[1,"a"],"nested":[[true,null],[false,2.5]],"deep":[{"n":7},[8,9]]}
1 a 2 7 9
P.t: expected an array of 2 values
P.t.1: expected a string
{"Pt":{"at":[1,2]}}
{"Bare":[["x",3]]}
y4
"#,
    ),
    // An `Option<fn(…)>` proved present is the function it holds, so it can
    // be called as written, after an `if` or an early return. The call
    // looked at the declared type and said it was not a function.
    (
        "a-narrowed-optional-function-is-called",
        r#"fn run(f: Option<fn(int) -> int>, x: int) -> int {
  if f == nil {
    return -1
  }
  return f(x)
}
fn main() {
  let f: Option<fn(int) -> int> = |a: int| a + 1
  if f != nil {
    io.print(f(41))
  }
  io.print(run(|a: int| a * 2, 4))
  io.print(run(nil, 4))
}
"#,
        "42\n8\n-1\n",
    ),
    // A concrete value converts to `dyn T` and is wrapped where an
    // `Option<dyn T>` is wanted, as a `T` is where an `Option<T>` is: a
    // binding, a return, an argument, a field and a slice element.
    (
        "a-value-becomes-an-optional-trait-object",
        r#"trait T {
  fn a(self) -> int
}

struct S {
  n: int
}

impl T for S {
  fn a(self) -> int {
    return self.n
  }
}

struct Slot {
  held: Option<dyn T>
}

fn pick(on: bool) -> Option<dyn T> {
  if on {
    return S{ n: 2 }
  }
  return nil
}

fn take(x: Option<dyn T>) -> int {
  if x == nil {
    return 0
  }
  return x.a()
}

fn main() {
  let s: Option<dyn T> = S{ n: 1 }
  if s != nil {
    io.print(s.a())
  }
  let p = pick(true)
  if p != nil {
    io.print(p.a())
  }
  io.print(pick(false) == nil)
  io.print(take(S{ n: 3 }))
  let slot = Slot{ held: S{ n: 4 } }
  io.print(take(slot.held))
  var xs: [Option<dyn T>] = [S{ n: 5 }, nil]
  xs.push(S{ n: 6 })
  io.print(take(xs[0]) + take(xs[1]) + take(xs[2]))
}
"#,
        "1\n2\ntrue\n3\n4\n11\n",
    ),
    // `@derive(Debug, Hash)` on a generic type: a parameter no field reaches
    // takes no bound, one a field reaches is walked through its own method,
    // and a recursive generic enum and a field of a generic derived type
    // walk too. Every generic type was refused.
    (
        "a-generic-type-derives-debug-and-hash",
        r#"@derive(Debug, Hash)
struct Tagged<T> {
  id: int
}

@derive(Debug, Hash)
struct Pair<A, B: Display> {
  a: A
  b: [B]
  note: Option<A>
}

@derive(Debug, Hash)
enum Tree<T> {
  Leaf(T)
  Node(left: Tree<T>, right: Tree<T>)
}

@derive(Debug)
struct Holder {
  p: Pair<int, str>
}

fn main() {
  let t: Tagged<fn()> = Tagged{ id: 3 }
  io.print(t.debug())
  let u: Tagged<str> = Tagged{ id: 3 }
  io.print(t.hash() == u.hash())
  let p = Pair{ a: 1, b: ["x", "y"], note: nil }
  io.print(p.debug())
  io.print(p.hash() == Pair{ a: 1, b: ["x", "y"], note: nil }.hash())
  io.print(p.hash() == Pair{ a: 2, b: ["x", "y"], note: nil }.hash())
  let tree = Node(left: Leaf("a"), right: Node(left: Leaf("b"), right: Leaf("c")))
  io.print(tree.debug())
  io.print(Holder{ p: p }.debug())
}
"#,
        "Tagged{ id: 3 }\ntrue\nPair{ a: 1, b: [\"x\", \"y\"], note: nil }\ntrue\nfalse\nNode(left: Leaf(\"a\"), right: Node(left: Leaf(\"b\"), right: Leaf(\"c\")))\nHolder{ p: Pair{ a: 1, b: [\"x\", \"y\"], note: nil } }\n",
    ),
    // The built-in types meet the prelude's `Display`, `Debug` and `Hash`
    // bounds, and a call through one is what `@derive` and interpolation do
    // with a field of that type. They met none, so the specification's own
    // `Cache<K: Hash, V>` could not have a `str` key.
    (
        "primitives-meet-the-prelude-bounds",
        r#"pub struct Cache<K: Hash, V> {
  var entries: {K: V}
  capacity: int
}

impl<K: Hash, V> Cache<K, V> {
  pub fn get(self, key: K) -> Option<V> {
    return self.entries[key]
  }

  pub fn slot(self, key: K) -> int {
    return key.hash() % self.capacity
  }
}

struct S<T> {
  v: T
}

impl<T: Display> Display for S<T> {
  fn show(self) -> str {
    return "S(\(self.v))"
  }
}

fn tell<T: Display>(x: T) -> str {
  return "\(x)|" + x.show()
}

fn dbg<T: Debug>(x: T) -> str {
  return x.debug()
}

fn h<T: Hash>(x: T) -> int {
  return x.hash()
}

@derive(Hash, Debug)
struct Row {
  i: int
  f: float
  b: bool
  s: str
}

fn main() {
  let c = Cache{ entries: {"a": 1}, capacity: 7 }
  io.print(c.get("a") == 1)
  io.print(c.slot("a") == hash_str("a") % 7)
  let n = Cache{ entries: {3: "x"}, capacity: 5 }
  io.print(n.slot(3) == hash_int(3) % 5)
  io.print(tell(3))
  io.print(tell(2.5))
  io.print(tell(true))
  io.print(tell("s"))
  io.print(S{ v: 1 })
  io.print(S{ v: "two" })
  io.print(dbg(3))
  io.print(dbg(1.5))
  io.print(dbg(false))
  io.print(dbg("a\"b"))
  let r = Row{ i: 4, f: 0.5, b: true, s: "q" }
  io.print(r.debug() == "Row{ i: \(dbg(4)), f: \(dbg(0.5)), b: \(dbg(true)), s: \(dbg("q")) }")
  var want = hash_seed()
  want = hash_combine(want, h(4))
  want = hash_combine(want, h(0.5))
  want = hash_combine(want, h(true))
  want = hash_combine(want, h("q"))
  io.print(r.hash() == want)
  io.print(h(0.0) == h(-0.0))
}
"#,
        "true\ntrue\ntrue\n3|3\n2.5|2.5\ntrue|true\ns|s\nS(1)\nS(two)\n3\n1.5\nfalse\n\"a\\\"b\"\ntrue\ntrue\ntrue\n",
    ),
    // A module-level constant named in a pattern is its value, as it is
    // everywhere else. `LIMIT => …` bound a fresh `LIMIT` that matched every
    // number, so `classify(5)` said "at the limit".
    (
        "constant-in-a-pattern-is-its-value",
        r#"let LIMIT = 10
let TWENTY = 20
let NAME = "kite"
let HALF = 0.5
let ON = true

fn classify(n: int) -> str {
  return match n {
    0 => "zero",
    LIMIT => "at the limit",
    -1 | TWENTY => "minus one or twenty",
    _ => "other",
  }
}

fn opt(o: Option<int>) -> str {
  return match o {
    nil => "none",
    LIMIT => "ten",
    _ => "some",
  }
}

fn main() {
  io.print(classify(5))
  io.print(classify(10))
  io.print(classify(20))
  io.print(opt(10))
  io.print(opt(11))
  io.print(match "kite" { NAME => "name", _ => "no" })
  io.print(match 0.25 { HALF => "half", _ => "no" })
  io.print(match false { ON => "on", _ => "off" })
  let LIMIT = 3
  io.print(match 4 { LIMIT => "bound \(LIMIT)" })
}
"#,
        "other\nat the limit\nminus one or twenty\nten\nsome\nname\nno\noff\nbound 4\n",
    ),
    // A unit variant two enums share is resolved against the scrutinee's
    // type, through an optional too: `Slow` against an `Option<Pace>` was a
    // binding that took every value, so `Pace.Quick` printed "slow". And a
    // tuple naming it twice, `(Slow, Slow)`, was "already declared".
    (
        "a-shared-variant-name-resolves-by-the-scrutinee",
        r#"enum Speed {
  Slow
  Fast
}

enum Pace {
  Slow
  Quick
}

fn name(o: Option<Pace>) -> str {
  return match o {
    nil => "none",
    Slow => "slow",
    Quick => "quick",
  }
}

fn pair(s: Speed, p: Pace) -> str {
  return match (s, p) {
    (Slow, Slow) => "ss",
    (Fast, Slow) => "fs",
    (_, Quick) => "q",
  }
}

fn mixed(p: (Speed, int)) -> int {
  return match p {
    (Slow, Slow) => Slow,
    (Fast, _) => 0,
  }
}

fn main() {
  io.print(name(Pace.Quick))
  io.print(name(Pace.Slow))
  io.print(name(nil))
  io.print(pair(Speed.Slow, Pace.Slow))
  io.print(pair(Speed.Fast, Pace.Quick))
  io.print(mixed((Speed.Slow, 4)))
}
"#,
        "quick\nslow\nnone\nss\nq\n4\n",
    ),
    // The same in an or-pattern, as std/text's `class_needs_bidi` writes
    // it: a shared name among the alternatives is the variant and matches
    // only it. The local the resolver declares for the name is never read,
    // and no longer has the error type.
    (
        "a-shared-variant-name-in-an-or-pattern-is-the-variant",
        r#"enum Bidi {
  R
  AL
  L
}

enum Break {
  AL
  SP
}

fn strong(c: Bidi) -> int {
  return match c {
    R | AL => 1,
    L => 0,
  }
}

fn left(c: Bidi) -> int {
  return match c {
    L | AL => 1,
    _ => 0,
  }
}

fn space(b: Break) -> int {
  return match b {
    AL => 0,
    SP => 1,
  }
}

fn main() {
  io.print(strong(Bidi.R))
  io.print(strong(Bidi.AL))
  io.print(strong(Bidi.L))
  io.print(left(Bidi.R))
  io.print(left(Bidi.AL))
  io.print(space(Break.SP))
}
"#,
        "1\n1\n0\n0\n1\n1\n",
    ),
    // A `let` after a `return` never runs and is never typed: its local
    // keeps the error type, unread, and the program is the warning E0116
    // and nothing else — not the guard's internal error.
    (
        "a-let-after-a-return-is-unreachable-not-an-internal-error",
        r#"fn main() {
  io.print(1)
  return
  let z = 5
  io.print(z)
}
"#,
        "1\n",
    ),
    // A trait that declares no method is implemented by an empty block,
    // which the resolver, knowing an implementation by its methods, has no
    // record of. The checker counts the block (a written impl is an impl),
    // and the type is a row of the trait's vtable, so a `dyn` of it holds
    // the type on every backend: the WasmGC module was invalid without it.
    (
        "a-trait-with-no-methods-is-implemented-by-an-empty-block",
        r#"trait Marker {}

struct P {
  a: int
}

enum K {
  One
  Two(int)
}

struct Box<T> {
  v: T
}

impl Marker for P {}
impl Marker for K {}
impl<T> Marker for Box<T> {}

fn keep(d: dyn Marker) -> dyn Marker {
  return d
}

fn one<T: Marker>(x: T) -> int {
  return 1
}

fn main() {
  let xs: [dyn Marker] = [P{ a: 1 }, K.Two(2), K.One, Box{ v: "s" }]
  var ys: [dyn Marker] = []
  for x in xs {
    ys.push(keep(x))
  }
  io.print(ys.len())
  io.print(one(P{ a: 1 }) + one(Box{ v: [1] }) + one(K.One))
  let m: {str: dyn Marker} = {"a": P{ a: 1 }, "b": K.One}
  io.print(m.len())
}
"#,
        "4\n3\n2\n",
    ),
    // A value-`if` branch, or a `match` block arm, whose one expression is
    // an `if` or a `match` yields it. An `if` opening a block parses as a
    // statement, so the branch was "must produce a value" and the arm `()`.
    (
        "a-branch-that-is-an-if-or-a-match-is-one-expression",
        r#"fn pick(a: bool, b: bool) -> int {
  return if a { if b { 1 } else { 2 } } else { 3 }
}

fn inner_match(a: bool, n: int) -> int {
  return if a {
    match n {
      2 => 20,
      _ => 0,
    }
  } else {
    7
  }
}

fn arm(n: int, a: bool) -> int {
  return match n {
    2 => {
      if a { 1 } else { 0 }
    },
    _ => 5,
  }
}

fn main() {
  io.print(pick(true, false))
  io.print(pick(false, true))
  io.print(inner_match(true, 2))
  io.print(inner_match(false, 2))
  io.print(arm(2, true))
  io.print(arm(2, false))
  io.print(arm(1, true))
  match 3 {
    3 => {
      if true { io.print("statement") } else { io.print("no") }
    },
    _ => {},
  }
}
"#,
        "2\n3\n20\n7\n1\n0\n5\nstatement\n",
    ),
    // A `match` statement discards its arms' values, so a print beside a
    // call returning an `int` or a `str` is fine. It was "match arms have
    // different types".
    (
        "a-match-statement-discards-arms-of-different-types",
        r#"fn f() -> int {
  io.print("f ran")
  return 7
}

fn g() -> str {
  io.print("g ran")
  return "s"
}

fn main() {
  for n in 0..4 {
    match n {
      0 => io.print("zero"),
      1 => f(),
      2 => g(),
      _ => {
        io.print("many")
      },
    }
  }
}
"#,
        "zero\nf ran\ng ran\nmany\n",
    ),
    // A loop body is a scope nested in its header's, as a function body is
    // in its parameters', so it may shadow the loop variable as it may a
    // parameter. It was E0112 for the one and fine for the other.
    (
        "a-loop-body-may-shadow-its-loop-variable",
        r#"fn f(x: int) -> int {
  let x = x + 1
  return x
}

fn main() {
  for i in 0..3 {
    let i = i * 10
    io.print(i)
  }
  let m = {"a": 1}
  for (k, v) in m {
    let v = v + 1
    let k = "\(k)!"
    io.print("\(k) \(v)")
  }
  io.print(f(1))
}
"#,
        "0\n10\n20\na! 2\n2\n",
    ),
    // A task carries its number with the scheduler, so `==` on two tasks is
    // true only for the same one (§12.1). Two tasks with equal results, or
    // two unfinished ones, used to compare equal, and a map keyed by tasks
    // folded them into one entry.
    (
        "a-task-is-equal-only-to-itself",
        r#"async fn one() -> int {
  return 1
}

pub async fn main() {
  let a = one()
  let b = one()
  let c = a
  io.print("\(a == b) \(a == c)")
  _ = await a
  _ = await b
  io.print("\(a == b) \(a == c) \(a == a)")
  var m: {Task<int>: int} = {}
  m[a] = 1
  m[b] = 2
  m[c] = 3
  io.print("\(m.len()) \(or_else(m[a], 0))")
}
"#,
        "false true\nfalse true true\n2 3\n",
    ),
    // What every copy of a field's slice or map holds after a write through
    // the field: what it held when it was taken, as it was before writes
    // through a field went in place.
    ("fields-are-written-in-place-unseen", FIELD_ALIASING, "let: [1,2] {a=1} now [1,2,3] {a=1,b=2}\n\
grown then let: [1,2,3,0,1,2,3,4] {a=1,b=2,g0=0,g1=1,g2=2,g3=3,g4=4} now [-1,2,3,0,1,2,3,4,99] {a=1,b=2,g0=-1,g2=2,g3=3,g4=4}\n\
kept by callee: [-1,2,3,0,1,2,3,4,99] now 10\n\
closure: 10 6 now 11 7\n\
second field: 11 now 12\n\
second struct: 12 7 now 13 8\n\
second struct written: 13 8 q 13 8\n\
..base: 13 8 now 14 9\n\
method: 14 9 now 15 10\n\
kept inside the method: 15 now 16\n\
local copy: 17 11 field 16 10\n\
walk: 16 now 19\n\
same struct: 20 20\n\
nested: [1] {x=1} now [1,2] {x=1,y=2}\n\
nested holder kept: [1,2,3] [1,2,3]\n\
in a slice: [1,2] [1,2,3] [1,2] {a=1,z=26}\n\
in a map: [1] [1,2] true\n\
grid: [1] [1];[2] now [1,10];[2];[3]\n\
enum: 20 10 now 21 11\n\
equal: false 21\n\
near: 22 -1 11 true now 23 77\n\
carried: 26 14 now 29 17\n\
own value: 29 77 true\n\
indexed: 31 {a=1,b1=1,b2=2,b4=4,b5=5,b7=7,b8=8,b10=10,b11=11,b13=13,b14=14,b16=16,b17=17,b19=19,b20=20,b22=22,b23=23,b25=25,b26=26,b28=28,b29=29} now {a=1,b1=101,b4=4,b5=5,b7=7,b8=8,b10=10,b11=11,b13=13,b14=14,b16=16,b17=17,b19=19,b20=20,b22=22,b23=23,b25=25,b26=26,b28=28,b29=29,b0=100}\n\
indexed kept: false false -1 3 101\n\
yield kept: [1,2,0,1,2] {a=1,k0=0,k1=1,k2=2} now [1,2,0,1,2,7] {a=1,k0=0,k1=1,k2=2,y=7}\n\
task saw: [1,2,0,1,2,7] {a=1,k0=0,k1=1,k2=2,y=7} now [100,2,0,1,2,7,8] {k0=0,k1=1,k2=2,y=7}\n\
await kept: [100,2,0,1,2,7,8] {k0=0,k1=1,k2=2,y=7} now [100,2,0,1,2,7,8,9] {k0=0,k1=1,k2=2,y=7,z=9}\n\
in place then kept: [100,2,0,1,2,7,8,9,0,1,2,3] now [100,2,0,1,2,7,8,9,0,1,2,3,10,11]\n\
nested across: [0,1,2] {n0=0,n1=1,n2=2} now [0,1,2,5] {n1=1,n2=2}\n\
"),
    // What a caller's slice or map holds after it was passed on, and what
    // every function that kept it holds: as it was before a call to one that
    // keeps nothing stopped ending the caller's right to write in place.
    ("passed-slices-are-written-in-place-unseen", CALL_ALIASING, "id [1,2] [10,2,3]\n\
via_var [1,2] [1,2,3]\n\
hold [1,2] [1,20,3]\n\
wrap [1,2] [1,2,3]\n\
in_map [1,2] [1,2,3]\n\
pair [1,2] 2 [1,2,3]\n\
opt [1,2] [1,2,3]\n\
tagged [1,2] [1,2,3]\n\
captured [1,2] [1,2,3]\n\
maybe [] [1,2,3] [1,2,3,4]\n\
outer [1,2] [1,2,3] [1,2,3,4]\n\
rec [1,2] [1,2,3] 4 5 [1,2,3,4,5,6]\n\
box [1,2] [1,2,3] {a=1} {a=1,b=2}\n\
bump 3 [1,2,3] 2 {a=1,b=2,c=3}\n\
rows [1] [7,7] [7,7,8] 3\n\
lookup [1] 2\n\
both [2] [1,4] [1,4,5] [2,3]\n\
reads 42 [3] 3+2+3+0+1+2+3 [4,2,3,0,1,2,3,4] x,k0,k1,k2,k3 6\n\
maps {a=1} {b=2} {b=2,c=3} {b=2,c=3,d=4}\n\
indirect [1,2] [1,2,3] [1,2,3,4] [1,2,3,4,5]\n\
loop [0];[0,0];[2,0,1,2];[3,0,1,2,3,4];[5,0,1,2,3,4,5];[7,0,1,2,3,4,5,6,7,8];[8,0,1,2,3,4,5,6,7,8];[11,0,1,2,3,4,5,6,7,8,9,10,11];[11,0,1,2,3,4,5,6,7,8,9,10,11,12];[14,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14];[15,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16];[17,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17] [19,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19]\n\
task len 2 now [1,2,3]\n\
task back [1,2,3] now [100,2,3,4]\n\
"),
    // What a field's slice or map, and everything that kept it, holds
    // after it was passed to a function and written through the field.
    ("passed-fields-are-written-in-place-unseen", PASSED_FIELDS, "add [1,1,2,3,4,5] {a=1,k0=1,k1=2,k2=3}\n\
kept [1,1,2,3,4,5] {a=1,k0=1,k1=2,k2=3} now [1,1,2,3,4,5,100] {a=1,k0=1,k1=2,k2=3,z=1,y=2}\n\
write_through {a=1,k0=1,k1=2,k2=3,z=1,y=2} {a=1,k0=1,k1=2,k2=3,z=1,y=2,w=9} now {a=1,k0=1,k1=2,k2=3,z=1,y=2,w=9,after=3}\n\
push_through [1,1,2,3,4,5,100,9] [1,1,2,3,4,5,100,9,7] | [1,1,2,3,4,5,100,9,7,200] [1,1,2,3,4,5,100,9,7,200,7] now [1,1,2,3,4,5,100,9,7,200,7,300]\n\
printed [1,1,2,3,4,5,100,9,7,200,7,300,400]\n\
closure 17 13 now [1,1,2,3,4,5,100,9,7,200,7,300,400,500]\n\
box [0,10,2,30,4,50] [1,3,5] {x0=0,x1=1}\n\
qs [5,0] [0,0]\n\
qs [0,0,1] [1,0,1]\n\
qs [1,0,1,2] [2,0,1,2]\n\
qs [2,0,1,2,3] [3,0,1,2,3]\n\
"),
    // What `toml.parse` makes of a hundred and fifteen documents — every
    // form the module reads, every rule about how a table comes to exist, and
    // every error it reports — pinned to what it answered while it still
    // built the document as nested `Toml`, before it read into flat tables.
    ("toml-reads-what-it-read", TOML_DOCUMENTS, TOML_DOCUMENTS_READ),
    // An `Option<error>` that holds a nil error is present: `task.timeout` of
    // a task that finished in time has its result, whatever it is. The VM,
    // which does not box an optional's payload, read every one of these as
    // absent; the other two box it and did not. The last of `ys` is the
    // documented case — an optional stored as nil is absent everywhere.
    (
        "an-optional-holding-a-nil-error-is-present",
        r#"use std/task

fn desc(o: Option<error>) -> str {
  if o == nil {
    return "absent"
  }
  if o == nil {
    return "nil"
  }
  return o.message()
}

fn some<T>(x: T) -> Option<T> {
  return x
}

struct Holder {
  e: Option<error>
}

enum Slot {
  Empty
  Full(e: Option<error>)
}

async fn save(fail: bool, ms: int) -> error {
  await task.sleep(ms)
  if fail {
    return errors.new("disk full")
  }
  return nil
}

async fn later(e: error) -> Option<error> {
  task.yield()
  return e
}

pub async fn main() {
  let none: error = nil
  io.print(desc(await task.timeout(save(false, 1), 50)))
  io.print(desc(await task.timeout(save(true, 1), 50)))
  io.print(desc(await task.timeout(save(false, 100), 5)))
  io.print(desc(await later(none)))
  let m: {str: error} = {"a": nil, "b": errors.new("b")}
  io.print(desc(m["a"]) + " " + desc(m["b"]) + " " + desc(m["z"]))
  let xs: [error] = [nil, errors.new("y")]
  io.print(desc(xs.get(0)) + " " + desc(xs.get(1)) + " " + desc(xs.get(9)))
  io.print(desc(find(xs, |e| e == nil)) + " " + desc(some(none)))
  let h = Holder{ e: xs[0] }
  let g = Holder{ e: nil }
  io.print(desc(h.e) + " " + desc(g.e) + " \(h.e == g.e) \(h.e == xs.get(0))")
  let s = Full(e: none)
  match s {
    Empty => io.print("empty"),
    Full(e) => io.print(desc(e)),
  }
  let ys: [Option<error>] = [none, nil]
  io.print(desc(ys[0]) + " " + desc(ys[1]) + " " + desc(ys.get(1)))
  match xs.get(0) {
    nil => io.print("absent"),
    e => io.print("present \(e == nil)"),
  }
}
"#,
        "nil\ndisk full\nabsent\nnil\nnil b absent\nnil y absent\nnil nil\nnil absent false true\nnil\nnil absent absent\npresent true\n",
    ),
    // A return type is the fallible form only as the pair `(T, error)`; with
    // more after the `error` it is a tuple, as it is in a `let` annotation.
    // `-> (int, error, int)` was E0100 `expected )`.
    (
        "a-returned-tuple-may-hold-an-error-in-the-middle",
        r#"fn g(fail: bool) -> (int, error, int) {
  if fail {
    return (1, errors.new("no"), 2)
  }
  return (1, nil, 2)
}
fn h() -> (int,
  error
) {
  return 7, nil
}
fn main() {
  let (a, e, b) = g(false)
  if e == nil {
    io.print(a + b)
  }
  let (_, e2, _) = g(true)
  if e2 != nil {
    io.print(e2.message())
  }
  let (v, e3) = h()
  if e3 == nil {
    io.print(v)
  }
}
"#,
        "3\nno\n7\n",
    ),
    // An arm of a `match` statement is in statement position (§9.2), so an
    // `if` written as one needs no `else`. It was E0100 "`if` used as a
    // value needs an `else`".
    (
        "an-arm-of-a-match-statement-may-be-an-if-with-no-else",
        r#"fn show(n: int, loud: bool) {
  match n {
    0 => if loud { io.print("ZERO") },
    1 => if loud { io.print("ONE") } else if n > 0 { io.print("one") },
    _ => io.print("many"),
  }
}
fn main() {
  show(0, true)
  show(0, false)
  show(1, false)
  show(5, true)
  let k = match 2 { 0 => if true { 1 } else { 5 } + 1, _ => 2 }
  io.print(k)
}
"#,
        "ZERO\none\nmany\n2\n",
    ),
    // A `break` is a `break` wherever it is written. One inside a `match`
    // used as a value — a `let` initializer, a call's argument, a tuple's
    // element, an arm of a match in an arm — leaves the loop, so what follows
    // it runs. The checker once looked for a `break` in statement positions
    // only, took each of these `for { }` loops for one nothing leaves and
    // dropped the code after it: the VM gave `()` from an `-> int`
    // function, native failed Cranelift's verifier and Wasm trapped.
    (
        "a-break-inside-an-expression-leaves-the-loop",
        r#"fn g(n: int) -> int {
  var i = 0
  for {
    i += 1
    let y = match i {
      3 => { break }
      _ => 1,
    }
    if y == n {
      return 0
    }
  }
  return 7
}
fn h() -> error {
  for {
    io.print(match 1 {
      1 => { break }
      _ => 0,
    })
  }
  return errors.new("after the loop")
}
fn main() {
  io.print(g(9))
  var i = 0
  for {
    i += 1
    match i {
      3 => match i {
        3 => { break }
        _ => {}
      },
      _ => io.print(i),
    }
  }
  io.print("after \(i)")
  var o = 0
  outer: for {
    for k in 0..2 {
      let p = (match o { 1 => { break outer }, _ => o + k }, 2)
      io.print(p.0)
    }
    o += 1
  }
  io.print("left at \(o)")
  let e = h()
  if e != nil {
    io.print(e.message())
  }
}
"#,
        "7\n1\n2\nafter 3\n0\n1\nleft at 1\nafter the loop\n",
    ),
    // A `match` tests one value. A guard that writes the local the
    // scrutinee names changes neither which later arm matches nor what its
    // bindings hold. The later arms read the local again, so the first
    // match fell past its last arm into the compiler's own `unreachable` and
    // the second chose `2`.
    (
        "a-match-guard-that-writes-the-scrutinee-does-not-change-what-is-tested",
        r#"enum E {
  P(n: int)
  Q(n: int)
}
struct S {
  kind: int
}
fn h(u: ()) -> int {
  return 1
}
fn main() {
  var e = P(n: 1)
  let r = match e {
    Q(n) => "q \(n)",
    P(n) if h(match 0 { _ => { e = Q(n: 9) } }) > 5 => "p, guarded",
    P(n) => "p \(n)",
  }
  io.print(r)
  var a = 1
  match a {
    1 if h(match 0 { _ => { a = 2 } }) > 5 => io.print("one, guarded"),
    2 => io.print("two"),
    k if k == 2 => io.print("bound two"),
    k => io.print("one \(k) \(a)"),
  }
  var b = 1
  let t = match (b, 5) {
    (1, y) if h(match 0 { _ => { b = 2 } }) > 5 => "guarded",
    (2, y) => "two",
    (x, y) => "\(x) \(y) \(b)",
  }
  io.print(t)
  var s = S{kind: 1}
  match s.kind {
    1 if h(match 0 { _ => { s = S{kind: 2} } }) > 5 => io.print("guarded"),
    2 => io.print("two"),
    k => io.print("\(k) \(s.kind)"),
  }
  var c = 1
  let u = match c {
    1 if h(match c { 1 if h(match 0 { _ => { c = 3 } }) > 5 => (), _ => () }) > 5 => "outer guarded",
    3 => "three",
    1 => match c {
      1 => "inner one",
      _ => "inner \(c)",
    },
    _ => "other",
  }
  io.print(u)
}
"#,
        "p 1\none 1 2\n1 5 2\n1 2\ninner 3\n",
    ),
    // `nil` written where an optional is held inside another type, with no
    // local of the optional's own type anywhere in the program. A `nil` is
    // a constant and needs none, and the Wasm backend declared an optional's
    // box only for such a local, so these were invalid modules there.
    (
        "nil-for-an-optional-inside-another-type-builds-on-wasm",
        r#"struct H {
  f: [Option<float>]
}
fn make() -> [Option<bool>] {
  return [nil]
}
fn main() {
  let a: [Option<int>] = [nil]
  io.print(a.len())
  let m: {str: Option<str>} = {"a": nil}
  io.print(m.len())
  let t: (Option<int>, int) = (nil, 3)
  io.print(t.1)
  var xs: [Option<str>] = []
  xs.push(nil)
  xs[0] = nil
  io.print(xs.len())
  var n: {str: Option<int>} = {}
  n["k"] = nil
  io.print(n.len())
  let h = H{f: [nil, nil]}
  io.print(h.f.len())
  io.print(make().len())
  let p: [Option<[int]>] = [nil]
  let q: [Option<[int]>] = [nil]
  io.print(p == q)
  let r: [Option<(int, str)>] = [nil]
  io.print(r == [nil])
}
"#,
        "1\n1\n3\n1\n1\n2\n1\ntrue\ntrue\n",
    ),
    // A jump in a loop's header does not belong to that loop, since the
    // header is not one of its iterations to leave or go on to, so a `break`
    // or `continue` written there leaves the loop around it:
    // a `continue` in a range's bound goes on to the outer loop's next turn,
    // a `break` in a `for cond` condition on its second evaluation leaves the
    // `for { }` around it, a `continue outer` in a map's sequence and a
    // `break` in a slice's element leave the loops they name. Every backend
    // has always run it so; the error analysis once credited the jump to the
    // loop whose header it is in.
    (
        "a-jump-in-a-loop-header-leaves-the-loop-around-it",
        r#"fn mk(n: int) -> {str: int} {
  return {"a": n}
}
fn main() {
  var i = 0
  for i < 3 {
    i += 1
    for k in 0..(match i { 2 => { continue }, _ => 1 }) {
      io.print("k \(k) at \(i)")
    }
    io.print("end of \(i)")
  }
  var n = 0
  for {
    n += 1
    var q = 0
    for q < (match q { 1 => { break }, _ => 3 }) {
      q += 1
      io.print("q \(q)")
    }
    io.print("not after a break")
  }
  io.print("left at \(n)")
  outer: for a in [1, 2] {
    for b in 0..2 {
      for (x, y) in mk(match b { 1 => { continue outer }, _ => a }) {
        io.print("\(a) \(b) \(x) \(y)")
      }
    }
    io.print("not after a continue")
  }
  for r in 0..3 {
    for s in [match r { 1 => { break }, _ => r }] {
      io.print("s \(s)")
    }
  }
  io.print("done")
}
"#,
        "k 0 at 1\nend of 1\nk 0 at 3\nend of 3\nq 1\nleft at 1\n1 0 a 1\n2 0 a 2\ns 0\ndone\n",
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
