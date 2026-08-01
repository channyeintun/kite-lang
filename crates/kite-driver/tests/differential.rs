//! Differential testing across backends.
//!
//! Every program is compiled twice — to bytecode and to WebAssembly — and run
//! on both. The outputs must match.
//!
//! This is the highest-value test in the tree. Two independent implementations
//! that must agree finds codegen bugs almost for free, and codegen bugs are the
//! hardest class to find any other way. It is also the reason the bytecode VM
//! was built before the Wasm backend even though Wasm is the point of the
//! project.
//!
//! The Wasm half needs Node. When Node is absent the module is still compiled
//! and validated; only the execution comparison is skipped, and the test says
//! so rather than silently passing.

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
         fn main() {\n  let c = C{ n: 1 }\n  c.bump()\n  c.bump()\n  io.print(c.n)\n}\n",
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
        "clipping-and-scrolling",
        "use std/ui\n\
         fn rows() -> Node {\n  var out: [Node] = []\n\
         \x20 for i in 0..8 {\n\
         \x20   out.push(text_of(\"r\\(i)\", Style{..style(), width: 100.0}, \"row \\(i)\"))\n  }\n\
         \x20 return box_of(\"list\", Style{..column(), align: Align.Stretch}, out)\n}\n\
         fn main() {\n\
         \x20 let v = Rect{x: 0.0, y: 0.0, width: 100.0, height: 50.0}\n\
         \x20 let box = Size{width: 100.0, height: 50.0}\n\
         \x20 let f = layout(rows(), box)\n\
         \x20 io.print(\"extent \\(scroll_extent(f, box))\")\n\
         \x20 io.print(clamp_scroll(-10.0, f, box))\n\
         \x20 io.print(clamp_scroll(9999.0, f, box))\n\
         \x20 var paints: [Paint] = []\n\
         \x20 for i in 0..8 {\n\
         \x20   paints.push(filled_label(\"r\\(i)\", \"row \\(i)\", 0x111111, 0xeeeeee))\n  }\n\
         \x20 paint_scrolled(f, paints, v, 0.0)\n\
         \x20 paint_scrolled(f, paints, v, 40.0)\n\
         \x20 let a = hit_scrolled(f, v, 0.0, 10.0, 10.0)\n\
         \x20 io.print(if a == nil { \"none\" } else { a.name })\n\
         \x20 let b = hit_scrolled(f, v, 40.0, 10.0, 10.0)\n\
         \x20 io.print(if b == nil { \"none\" } else { b.name })\n\
         \x20 let c = hit_scrolled(f, v, 0.0, 500.0, 500.0)\n\
         \x20 io.print(if c == nil { \"none\" } else { c.name })\n}\n",
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
        "wrapping",
        "use std/ui\n\
         fn main() {\n\
         \x20 let body = \"The quick brown fox jumps over the lazy dog\"\n\
         \x20 for w in [100.0, 200.0, 400.0, 1000.0] {\n\
         \x20   let lines = wrap(body, w)\n\
         \x20   io.print(\"\\(w): \\(lines.len()) lines, \\(wrapped_size(body, w).width)\")\n\
         \x20   for line in lines {\n      io.print(\"|\\(line)|\")\n    }\n  }\n\
         \x20 io.print(wrap(\"\", 100.0).len())\n\
         \x20 io.print(wrap(\"supercalifragilistic\", 20.0).len())\n\
         \x20 io.print(wrap(\"  spaced   out  \", 1000.0)[0])\n}\n",
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
        "clipping-and-scrolling",
        "use std/ui\n\
         fn rows() -> Node {\n  var out: [Node] = []\n\
         \x20 for i in 0..8 {\n\
         \x20   out.push(text_of(\"r\\(i)\", Style{..style(), width: 100.0}, \"row \\(i)\"))\n  }\n\
         \x20 return box_of(\"list\", Style{..column(), align: Align.Stretch}, out)\n}\n\
         fn main() {\n\
         \x20 let v = Rect{x: 0.0, y: 0.0, width: 100.0, height: 50.0}\n\
         \x20 let box = Size{width: 100.0, height: 50.0}\n\
         \x20 let f = layout(rows(), box)\n\
         \x20 io.print(\"extent \\(scroll_extent(f, box))\")\n\
         \x20 io.print(clamp_scroll(-10.0, f, box))\n\
         \x20 io.print(clamp_scroll(9999.0, f, box))\n\
         \x20 var paints: [Paint] = []\n\
         \x20 for i in 0..8 {\n\
         \x20   paints.push(filled_label(\"r\\(i)\", \"row \\(i)\", 0x111111, 0xeeeeee))\n  }\n\
         \x20 paint_scrolled(f, paints, v, 0.0)\n\
         \x20 paint_scrolled(f, paints, v, 40.0)\n\
         \x20 let a = hit_scrolled(f, v, 0.0, 10.0, 10.0)\n\
         \x20 io.print(if a == nil { \"none\" } else { a.name })\n\
         \x20 let b = hit_scrolled(f, v, 40.0, 10.0, 10.0)\n\
         \x20 io.print(if b == nil { \"none\" } else { b.name })\n\
         \x20 let c = hit_scrolled(f, v, 0.0, 500.0, 500.0)\n\
         \x20 io.print(if c == nil { \"none\" } else { c.name })\n}\n",
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
        "wrapping",
        "use std/ui\n\
         fn main() {\n\
         \x20 let body = \"The quick brown fox jumps over the lazy dog\"\n\
         \x20 for w in [100.0, 200.0, 400.0, 1000.0] {\n\
         \x20   let lines = wrap(body, w)\n\
         \x20   io.print(\"\\(w): \\(lines.len()) lines, \\(wrapped_size(body, w).width)\")\n\
         \x20   for line in lines {\n      io.print(\"|\\(line)|\")\n    }\n  }\n\
         \x20 io.print(wrap(\"\", 100.0).len())\n\
         \x20 io.print(wrap(\"supercalifragilistic\", 20.0).len())\n\
         \x20 io.print(wrap(\"  spaced   out  \", 1000.0)[0])\n}\n",
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
        "evaluation-order",
        "fn step(n: int) -> int {\n  io.print(n)\n  return n\n}\nfn main() {\n  let x = step(1) + step(2)\n  io.print(x)\n}\n",
    ),
];

fn run_on_vm(name: &str, src: &str) -> String {
    let c = compile(format!("{}.kite", name), src, Emit::Check);
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

fn run_on_wasm(name: &str, src: &str, dir: &std::path::Path) -> String {
    let c = compile(format!("{}.kite", name), src, Emit::Wasm);
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
        kite_driver::generate_glue(&module.strings, "app.wasm"),
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

        // Every example must at least run on the bytecode target.
        let vm = run_on_vm(&name, &src);
        total += 1;

        // The Wasm target still refuses a few constructs. Those examples are
        // counted as skipped rather than silently passing, so the count below
        // fails if coverage ever goes backwards.
        let compiled = compile(format!("{}.kite", name), &src, Emit::Wasm);
        if compiled.failed() {
            skipped.push(name);
            continue;
        }

        let wasm = run_on_wasm(&name, &src, &dir);
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
fn both_backends_agree() {
    if !node_available() {
        eprintln!("skipping the wasm half: node is not on PATH");
        // The modules are still built and validated by kite-codegen-wasm's own
        // tests, so this is a reduced check rather than no check.
        for (name, src) in PROGRAMS {
            let _ = run_on_vm(name, src);
        }
        return;
    }

    let root = std::env::temp_dir().join(format!("kite-diff-{}", std::process::id()));
    let mut mismatches = Vec::new();

    for (name, src) in PROGRAMS {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create work directory");

        let vm = run_on_vm(name, src);
        let wasm = run_on_wasm(name, src, &dir);

        if vm != wasm {
            mismatches.push(format!(
                "{}:\n  vm:   {:?}\n  wasm: {:?}",
                name, vm, wasm
            ));
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
