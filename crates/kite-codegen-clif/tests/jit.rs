//! The native backend against the bytecode oracle, construct by construct.
//!
//! The full corpus runs three ways in `kite-driver`'s differential suite;
//! these are the narrower probes that make a lowering bug point at the
//! construct that broke rather than at a whole program.

mod common;
use common::agree;

#[test]
fn integers_and_control_flow() {
    agree(
        "fn add(a: int, b: int) -> int {\n  return a + b\n}\n\
         fn main() {\n  let x = add(2, 3)\n  if x > 4 {\n    io.print(x)\n  }\n\
         \x20 for i in 0..x {\n    io.print(i)\n  }\n}\n",
    );
}

#[test]
fn arithmetic_and_comparison() {
    agree(
        "fn main() {\n  io.print(2 + 3 * 4 - 10 / 2 % 3)\n  io.print(1.5 + 2.5 * 2.0)\n\
         \x20 io.print(-7)\n  io.print(7 < 9)\n  io.print(2.0 >= 3.0)\n  io.print(!false)\n\
         \x20 io.print(12 & 10)\n  io.print(1 << 4)\n  io.print(256 >> 4)\n}\n",
    );
}

#[test]
fn floats_render_like_the_vm() {
    agree("fn main() {\n  io.print(1.5)\n  io.print(4.0)\n  io.print(1.0 / 4.0)\n  io.print(0.1 + 0.2)\n}\n");
}

#[test]
fn strings_concat_compare_and_ops() {
    agree(
        "fn main() {\n  io.print(\"a\" + \"b\" + \"c\")\n  io.print(\"x\" == \"x\")\n\
         \x20 io.print(\"x\" != \"y\")\n  io.print(\"a\" < \"b\")\n\
         \x20 let s = \"  héllo日本  \"\n  io.print(s.len())\n  io.print(s.trim())\n\
         \x20 io.print(s.slice(2, 7))\n  io.print(s.index_of(\"日\"))\n\
         \x20 io.print(s.code_at(2))\n  io.print(s.code_at(99))\n}\n",
    );
}

#[test]
fn structs_and_mutation() {
    agree(
        "struct C {\n  var n: int\n}\n\
         impl C {\n  fn bump(var self) {\n    self.n = self.n + 1\n  }\n}\n\
         fn main() {\n  let c = C{ n: 1 }\n  c.bump()\n  c.bump()\n  io.print(c.n)\n}\n",
    );
}

#[test]
fn enums_and_match() {
    agree(
        "enum Shape {\n  Circle(radius: int)\n  Rect(width: int, height: int)\n  Point\n}\n\
         fn area(s: Shape) -> int {\n  return match s {\n    Circle(r) => 3 * r * r,\n\
         \x20   Rect(w, h) => w * h,\n    Point => 0,\n  }\n}\n\
         fn main() {\n  io.print(area(Circle(radius: 2)))\n\
         \x20 io.print(area(Rect(width: 3, height: 4)))\n  io.print(area(Point))\n}\n",
    );
}

#[test]
fn slices_maps_and_tuples() {
    agree(
        "fn main() {\n  var xs = [1, 2, 3]\n  xs.push(4)\n  io.print(xs.len())\n\
         \x20 io.print(xs[3])\n  var ys = xs\n  ys[0] = 9\n  io.print(xs[0])\n  io.print(ys[0])\n\
         \x20 var m = {\"a\": 1}\n  m[\"b\"] = 2\n  io.print(m.len())\n\
         \x20 let a = m[\"a\"]\n  io.print(if a == nil { -1 } else { a })\n\
         \x20 let t = (1, \"one\")\n  io.print(match t {\n    (n, s) => s,\n  })\n}\n",
    );
}

#[test]
fn optionals_boxed_and_nil() {
    agree(
        "fn maybe(n: int) -> Option<int> {\n  if n > 0 {\n    return n\n  }\n  return nil\n}\n\
         fn main() {\n  let a = maybe(5)\n  io.print(if a == nil { 0 } else { a })\n\
         \x20 let b = maybe(-1)\n  io.print(if b == nil { 0 } else { b })\n}\n",
    );
}

#[test]
fn errors_and_pairs() {
    agree(
        "fn divide(a: int, b: int) -> (int, error) {\n  if b == 0 {\n\
         \x20   return _, errors.new(\"division by zero\")\n  }\n  return a / b, nil\n}\n\
         fn report(a: int, b: int) {\n  let (q, err) = divide(a, b)\n  if err != nil {\n\
         \x20   io.print(\"failed: \" + err.message())\n  } else {\n    io.print(q)\n  }\n}\n\
         fn main() {\n  report(10, 2)\n  report(1, 0)\n}\n",
    );
}

#[test]
fn closures_capture_and_call() {
    agree(
        "fn apply(f: fn(int) -> int, x: int) -> int {\n  return f(x)\n}\n\
         fn make_adder(n: int) -> fn(int) -> int {\n  return |x: int| x + n\n}\n\
         fn main() {\n  let double = |x: int| x * 2\n  io.print(apply(double, 21))\n\
         \x20 let add5 = make_adder(5)\n  io.print(apply(add5, 1))\n}\n",
    );
}

#[test]
fn trait_objects_dispatch() {
    agree(
        "trait Shape {\n  fn area(self) -> int\n}\n\
         struct Circle {\n  r: int\n}\nstruct Square {\n  s: int\n}\n\
         impl Shape for Circle {\n  fn area(self) -> int {\n    return self.r * self.r * 3\n  }\n}\n\
         impl Shape for Square {\n  fn area(self) -> int {\n    return self.s * self.s\n  }\n}\n\
         fn main() {\n  let xs: [dyn Shape] = [Circle{r: 1}, Square{s: 2}]\n\
         \x20 var sum = 0\n  for x in xs {\n    sum = sum + x.area()\n  }\n  io.print(sum)\n}\n",
    );
}

#[test]
fn structural_equality() {
    agree(
        "struct Point {\n  x: int\n  y: int\n}\n\
         fn main() {\n  let p = Point{x: 1, y: 2}\n  let q = Point{x: 1, y: 2}\n\
         \x20 io.print(p == q)\n  io.print(p == Point{x: 9, y: 2})\n\
         \x20 io.print([1, 2] == [1, 2])\n  io.print([1, 2] == [1, 3])\n\
         \x20 io.print((1, \"a\") == (1, \"a\"))\n}\n",
    );
}

#[test]
fn interpolation_matches_print() {
    agree(
        "fn main() {\n  let n = 42\n  let pi = 2.5\n  let ok = true\n\
         \x20 io.print(\"n=\\(n) pi=\\(pi) ok=\\(ok)\")\n  io.print(\"whole: \\(3.0)\")\n}\n",
    );
}

#[test]
fn tasks_interleave_deterministically() {
    agree(
        "async fn work(n: int) -> int {\n  io.print(\"start \\(n)\")\n  task.yield()\n\
         \x20 io.print(\"end \\(n)\")\n  return n * 10\n}\n\
         async fn main() {\n  let a = work(1)\n  let b = work(2)\n\
         \x20 io.print(\"results \\(await a) \\(await b)\")\n}\n",
    );
}

#[test]
fn drawing_writes_the_same_lines() {
    agree(
        "fn main() {\n  draw.rect(0.0, 0.0, 640.0, 360.0, 0x14161a)\n\
         \x20 draw.text(12.0, 12.0, \"Kite\", 0xf5f7fa)\n\
         \x20 io.print(text.width(\"abc\"))\n  io.print(text.height())\n}\n",
    );
}
