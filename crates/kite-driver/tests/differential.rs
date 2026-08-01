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
