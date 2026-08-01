//! Every emitted module is validated with `wasmparser`.
//!
//! A codegen bug should fail here, in CI, rather than in a browser — which is
//! the whole reason the validator is a dependency.

use super::*;
use kite_span::SourceMap;

struct Built {
    module: WasmModule,
}

impl Built {
    /// Validate against the full Wasm feature set the language targets.
    fn validate(&self) {
        let mut validator = wasmparser::Validator::new_with_features(
            wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::GC,
        );
        if let Err(e) = validator.validate_all(&self.module.bytes) {
            panic!(
                "emitted module is invalid: {e}\n\n{}",
                wasmprinter::print_bytes(&self.module.bytes)
                    .unwrap_or_else(|_| "<unprintable>".into())
            );
        }
    }

    fn size(&self) -> usize {
        self.module.bytes.len()
    }
}

fn build(src: &str) -> Built {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check(&ast, &resolved, src, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
    kite_hir::mono::monomorphise(&mut hir);
    let mir = kite_mir::lower(&hir);
    Built { module: compile(&mir, &hir.types) }
}

/// Validate, which is the assertion that matters for every one of these.
fn valid(src: &str) -> Built {
    let b = build(src);
    b.validate();
    b
}

const HELLO: &str = "\
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
";

#[test]
fn the_phase_one_program_produces_a_valid_module() {
    valid(HELLO);
}

/// The Phase 4 exit criterion: hello world under 10 KB. Shipping no garbage
/// collector is what makes that reachable.
#[test]
fn hello_world_is_small() {
    let b = valid(HELLO);
    assert!(
        b.size() < 10_000,
        "module is {} bytes, over the 10 KB budget",
        b.size()
    );
}

#[test]
fn arithmetic_on_both_numeric_types_validates() {
    valid("fn main() {\n  io.print(1 + 2 * 3 - 4 / 2 % 3)\n  io.print(1.5 + 2.5 * 2.0)\n}\n");
}

#[test]
fn comparisons_and_bitwise_operators_validate() {
    valid(
        "fn main() {\n  io.print(1 < 2)\n  io.print(1.0 >= 2.0)\n  io.print(true != false)\n\
         \x20 io.print(12 & 10 | 3 ^ 1)\n  io.print(1 << 4)\n  io.print(256 >> 2)\n}\n",
    );
}

#[test]
fn unary_operators_validate() {
    valid("fn main() {\n  io.print(-5)\n  io.print(-1.5)\n  io.print(!true)\n}\n");
}

/// The dispatch loop must handle every control-flow shape MIR produces.
#[test]
fn every_loop_form_validates() {
    valid(
        "fn main() {\n  for i in 0..3 {\n    io.print(i)\n  }\n\
         \x20 var n = 0\n  for n < 3 {\n    n += 1\n  }\n\
         \x20 for {\n    break\n  }\n}\n",
    );
}

#[test]
fn nested_loops_with_labelled_jumps_validate() {
    valid(
        "fn main() {\n  outer: for i in 0..3 {\n    for j in 0..3 {\n\
         \x20     if j == 1 {\n        continue outer\n      }\n\
         \x20     if i == 2 {\n        break outer\n      }\n    }\n  }\n}\n",
    );
}

#[test]
fn if_else_chains_validate() {
    valid(
        "fn classify(n: int) -> int {\n  if n > 10 {\n    return 2\n  } else if n > 5 {\n\
         \x20   return 1\n  } else {\n    return 0\n  }\n}\nfn main() {\n  io.print(classify(7))\n}\n",
    );
}

#[test]
fn short_circuit_operators_validate() {
    valid("fn main() {\n  let a = true\n  let b = false\n  io.print(a && b)\n  io.print(a || b)\n}\n");
}

#[test]
fn an_if_expression_validates() {
    valid("fn main() {\n  let n = if 1 < 2 { 10 } else { 20 }\n  io.print(n)\n}\n");
}

#[test]
fn recursion_validates() {
    valid(
        "fn fact(n: int) -> int {\n  if n <= 1 {\n    return 1\n  }\n  return n * fact(n - 1)\n}\n\
         fn main() {\n  io.print(fact(10))\n}\n",
    );
}

#[test]
fn mutual_recursion_validates() {
    valid(
        "fn is_even(n: int) -> bool {\n  if n == 0 {\n    return true\n  }\n  return is_odd(n - 1)\n}\n\
         fn is_odd(n: int) -> bool {\n  if n == 0 {\n    return false\n  }\n  return is_even(n - 1)\n}\n\
         fn main() {\n  io.print(is_even(10))\n}\n",
    );
}

#[test]
fn a_unit_function_validates() {
    valid("fn shout(n: int) {\n  io.print(n)\n}\nfn main() {\n  shout(1)\n}\n");
}

#[test]
fn deferred_initialisation_validates() {
    valid("fn main() {\n  let z: int\n  if true {\n    z = 1\n  } else {\n    z = 2\n  }\n  io.print(z)\n}\n");
}

// ---- module shape ---------------------------------------------------------

#[test]
fn main_is_exported() {
    let b = valid(HELLO);
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    assert!(printed.contains("(export \"main\""), "{}", printed);
}

#[test]
fn host_functions_are_imported_not_defined() {
    let b = valid(HELLO);
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    for name in ["print_int", "print_float", "print_bool", "print_str"] {
        assert!(
            printed.contains(&format!("(import \"kite\" \"{}\"", name)),
            "missing import {}:\n{}",
            name,
            printed
        );
    }
}

/// String constants live in the glue, so the module needs no linear memory.
#[test]
fn the_module_has_no_linear_memory() {
    let b = valid(HELLO);
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    assert!(!printed.contains("(memory"), "{}", printed);
}

#[test]
fn string_constants_reach_the_glue() {
    let b = valid(HELLO);
    assert_eq!(b.module.strings, vec!["big".to_string()]);
    let g = generate_glue(&b.module.strings, "app.wasm");
    assert!(g.contains(r#""big""#));
}

// ---- WasmGC structs -------------------------------------------------------

const RECT: &str = "\
struct Rect {
    width: int
    height: int
    var scale: int
}

impl Rect {
    fn area(self) -> int {
        return self.width * self.height
    }
}
";

/// Kite ships no garbage collector: structs are WasmGC objects traced by the
/// host engine, which is the whole reason a module stays this small.
#[test]
fn struct_construction_and_field_reads_validate() {
    valid(&format!(
        "{}\nfn main() {{\n  let r = Rect{{ width: 3, height: 4, scale: 1 }}\n  io.print(r.width)\n  io.print(r.area())\n}}\n",
        RECT
    ));
}

#[test]
fn a_var_field_can_be_written() {
    valid(&format!(
        "{}\nfn main() {{\n  let r = Rect{{ width: 1, height: 1, scale: 1 }}\n  r.scale = 9\n  io.print(r.scale)\n}}\n",
        RECT
    ));
}

#[test]
fn structs_pass_through_functions() {
    valid(&format!(
        "{}\nfn wider(r: Rect) -> Rect {{\n  return Rect{{ ..r, width: r.width + 1 }}\n}}\nfn main() {{\n  let r = wider(Rect{{ width: 1, height: 2, scale: 1 }})\n  io.print(r.width)\n}}\n",
        RECT
    ));
}

#[test]
fn nested_structs_validate() {
    valid(
        "struct Inner {\n  n: int\n}\nstruct Outer {\n  inner: Inner\n}\n\
         fn main() {\n  let o = Outer{ inner: Inner{ n: 42 } }\n  io.print(o.inner.n)\n}\n",
    );
}

/// A struct that names itself needs no boxing annotation, because every Kite
/// aggregate is already a GC reference. One `rec` group is what makes the
/// emitted types accept it.
#[test]
fn a_recursive_struct_type_validates() {
    valid(
        "struct Node {\n  value: int\n  next: Node\n}\n\
         fn value_of(n: Node) -> int {\n  return n.value\n}\n\
         fn main() {\n  io.print(1)\n}\n",
    );
}

#[test]
fn mutually_recursive_structs_validate() {
    valid(
        "struct A {\n  b: B\n}\nstruct B {\n  a: A\n}\n\
         fn f(x: A) -> B {\n  return x.b\n}\n\
         fn main() {\n  io.print(1)\n}\n",
    );
}

/// Kite's per-field `var` marker is exactly WasmGC's per-field mutability flag.
#[test]
fn field_mutability_reaches_the_emitted_type() {
    let b = valid(&format!("{}\nfn main() {{\n  io.print(1)\n}}\n", RECT));
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    // `width` and `height` are immutable; `scale` is `var`.
    assert!(printed.contains("(mut i64)"), "no mutable field emitted:\n{}", printed);
    assert!(printed.contains("(struct"), "no struct type emitted:\n{}", printed);
}

#[test]
fn a_program_with_structs_stays_small() {
    let b = valid(&format!(
        "{}\nfn main() {{\n  let r = Rect{{ width: 3, height: 4, scale: 1 }}\n  io.print(r.area())\n}}\n",
        RECT
    ));
    assert!(b.size() < 10_000, "module is {} bytes", b.size());
}

// ---- WasmGC enums ---------------------------------------------------------

const SHAPE: &str = "\
enum Shape {
    Circle(radius: int)
    Rect(width: int, height: int)
    Point
}
";

/// An enum becomes a base record holding the tag plus one subtype per variant.
/// A `match` reads the tag; a payload read casts to the variant it has already
/// established.
#[test]
fn enum_construction_and_matching_validate() {
    valid(&format!(
        "{}\nfn area(s: Shape) -> int {{\n  return match s {{\n    Circle(r) => 3 * r * r,\n    Rect(w, h) => w * h,\n    Point => 0,\n  }}\n}}\nfn main() {{\n  io.print(area(Circle(radius: 2)))\n  io.print(area(Rect(width: 3, height: 4)))\n  io.print(area(Point))\n}}\n",
        SHAPE
    ));
}

#[test]
fn a_unit_variant_validates() {
    valid(&format!(
        "{}\nfn main() {{\n  let p = Point\n  io.print(match p {{\n    Point => 1,\n    _ => 0,\n  }})\n}}\n",
        SHAPE
    ));
}

#[test]
fn guards_and_literal_patterns_validate() {
    valid(&format!(
        "{}\nfn main() {{\n  io.print(match Rect(width: 2, height: 2) {{\n    Rect(w, h) if w == h => 1,\n    Rect(w, h) => 2,\n    _ => 0,\n  }})\n  io.print(match 7 {{\n    0 => \"zero\",\n    1 | 2 => \"small\",\n    3..=9 => \"medium\",\n    _ => \"large\",\n  }})\n}}\n",
        SHAPE
    ));
}

/// A recursive enum needs no boxing annotation: every Kite aggregate is already
/// a GC reference, and one `rec` group is what lets the emitted types say so.
#[test]
fn a_recursive_enum_validates() {
    valid(
        "enum Tree {\n  Leaf(int)\n  Node(left: Tree, right: Tree)\n}\n\
         fn total(t: Tree) -> int {\n  return match t {\n    Leaf(n) => n,\n\
         \x20   Node(l, r) => total(l) + total(r),\n  }\n}\n\
         fn main() {\n  io.print(total(Node(left: Leaf(1), right: Leaf(2))))\n}\n",
    );
}

#[test]
fn variants_are_subtypes_of_the_enum_base() {
    let b = valid(&format!("{}\nfn main() {{\n  io.print(1)\n}}\n", SHAPE));
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    assert!(printed.contains("(sub "), "variants are not subtypes:\n{}", printed);
}

// ---- honest about what is not lowered -------------------------------------

fn gaps(src: &str) -> Vec<String> {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check(&ast, &resolved, src, &mut diags);
    assert!(!diags.has_errors(), "{}", diags.render_all(&sources));
    kite_hir::mono::monomorphise(&mut hir);
    let mir = kite_mir::lower(&hir);
    unsupported(&mir, &hir.types)
        .into_iter()
        .map(|u| u.what.to_string())
        .collect()
}

/// Every construct the language can express now lowers.
///
/// The scan that would report a gap is still wired in — a backend that quietly
/// emits a trapping module for something it cannot do is the failure this
/// module exists to prevent — but it has nothing left to say.
#[test]
fn nothing_the_language_can_express_is_refused() {
    let everything = "\
trait Shape {
  fn area(self) -> int
}
struct Sq {
  s: int
}
enum E {
  A
  B(int)
}
impl Shape for Sq {
  fn area(self) -> int {
    return self.s * self.s
  }
}
fn pick<T>(xs: [T]) -> Option<T> {
  if xs.len() == 0 {
    return nil
  }
  return xs[0]
}
fn call(f: fn(int) -> int, x: int) -> int {
  return f(x)
}
fn main() {
  let base = 10
  io.print(call(|x: int| x + base, 5))
  io.print(pick([1, 2]) == pick([1, 2]))
  var m = {\"a\": 1}
  m[\"b\"] = 2
  io.print(m.len())
  let shapes: [dyn Shape] = [Sq{s: 3}]
  for s in shapes {
    io.print(\"area \\(s.area())\")
  }
  io.print(match E.B(4) {
    A => 0
    B(n) => n
  })
}
";
    valid(everything);
    assert!(gaps(everything).is_empty(), "unexpected gaps: {:?}", gaps(everything));
}

const SHAPES: &str = "\
trait Shape {
  fn area(self) -> int
}
struct Circle {
  r: int
}
struct Square {
  s: int
}
impl Shape for Circle {
  fn area(self) -> int {
    return self.r * self.r * 3
  }
}
impl Shape for Square {
  fn area(self) -> int {
    return self.s * self.s
  }
}
fn total(shapes: [dyn Shape]) -> int {
  var sum = 0
  for s in shapes {
    sum = sum + s.area()
  }
  return sum
}
fn main() {
  let xs: [dyn Shape] = [Circle{r: 2}, Square{s: 3}]
  io.print(total(xs))
}
";

/// Deep equality is a generated function per aggregate type. A type is only
/// given one when the program compares it, and the comparison recurses into
/// components — so a struct holding a slice needs the slice's function too.
#[test]
fn structural_equality_generates_one_function_per_type() {
    let src = "struct P {\n  x: int\n  tags: [str]\n}\n\
               fn main() {\n  let a = P{x: 1, tags: [\"a\"]}\n\
               \x20 let b = P{x: 1, tags: [\"a\"]}\n  io.print(a == b)\n  io.print(a != b)\n}\n";
    valid(src);
    assert!(gaps(src).is_empty());

    // Enums compare tags first, so different variants are never equal.
    valid(
        "enum E {\n  A\n  B(int)\n  C(str, int)\n}\n\
         fn main() {\n  io.print(E.A == E.B(1))\n  io.print(E.C(\"x\", 1) == E.C(\"x\", 1))\n}\n",
    );

    // Optionals: absent equals absent, present compares payloads.
    valid(
        "struct P {\n  x: int\n}\n\
         fn main() {\n  let a: Option<P> = P{x: 1}\n  let b: Option<P> = nil\n\
         \x20 io.print(a == b)\n}\n",
    );

    // A program that compares nothing deep generates nothing.
    let shallow = "fn main() {\n  io.print(1 == 2)\n}\n";
    let deep = "struct P {\n  x: int\n}\n\
                fn main() {\n  io.print(P{x: 1} == P{x: 2})\n}\n";
    assert!(valid(shallow).size() < valid(deep).size());
}

/// Trait objects lower to a tag comparison in a per-method dispatcher.
#[test]
fn trait_objects_validate() {
    valid(SHAPES);
    assert!(gaps(SHAPES).is_empty());
}

/// `Circle { r: int }` and `Square { s: int }` are *the same* WasmGC type —
/// types there are compared structurally, so a nominal difference in Kite is
/// no difference at all here. Dispatch therefore cannot use `ref.test` and
/// reads a stored identity tag instead. That the two dispatch apart is checked
/// by executing them, in the differential suite; this test pins the reason.
#[test]
fn dispatchable_types_carry_an_identity_tag() {
    let tagged = "trait T {\n  fn f(self) -> int\n}\nstruct P {\n  x: int\n}\n\
                  impl T for P {\n  fn f(self) -> int {\n    return self.x\n  }\n}\n\
                  fn g(v: dyn T) -> int {\n  return v.f()\n}\n\
                  fn main() {\n  io.print(g(P{x: 1}))\n}\n";
    valid(tagged);

    // A program that never forms a trait object pays nothing: no tag field, no
    // dispatcher, no root record extension.
    let plain = "struct P {\n  x: int\n}\n\
                 fn g(v: P) -> int {\n  return v.x\n}\n\
                 fn main() {\n  io.print(g(P{x: 1}))\n}\n";
    assert!(
        valid(plain).size() < valid(tagged).size(),
        "a program without `dyn` should be smaller than the same program with it"
    );
}

/// Everything the backend *does* lower must report no gaps, or the check would
/// be refusing working programs.
/// A `str` is a table index, so its operations are host calls. They lower.
#[test]
fn string_operations_lower() {
    valid("fn main() {\n  io.print(\"a\" + \"b\")\n  io.print(\"x\" == \"y\")\n  io.print(\"x\" != \"y\")\n}\n");
    assert!(gaps("fn main() {\n  io.print(\"a\" + \"b\")\n}\n").is_empty());
}

#[test]
fn tuples_validate() {
    valid(
        "fn pair() -> (int, str) {\n  return (7, \"seven\")\n}\n\
         fn main() {\n  io.print(match pair() {\n    (0, s) => \"zero\",\n    (n, s) => s,\n  })\n}\n",
    );
    assert!(gaps("fn f() -> (int, str) {\n  return (1, \"a\")\n}\nfn main() {\n}\n").is_empty());
}

#[test]
fn supported_programs_report_no_gaps() {
    assert!(gaps(HELLO).is_empty());
    assert!(gaps(&format!(
        "{}\nfn main() {{\n  let r = Rect{{ width: 1, height: 2, scale: 3 }}\n  io.print(r.area())\n}}\n",
        RECT
    ))
    .is_empty());
    assert!(gaps(&format!(
        "{}\nfn main() {{\n  io.print(match Point {{\n    Point => 1,\n    _ => 0,\n  }})\n}}\n",
        SHAPE
    ))
    .is_empty());
}

// ---- optionals ------------------------------------------------------------

/// `Option<T>` is a nullable reference to a one-field box, so `nil` is a null
/// reference and the payload keeps its own type rather than being erased.
#[test]
fn optionals_validate() {
    valid(
        "struct U {\n  name: str\n}\n\
         fn find(id: int) -> Option<U> {\n  if id == 1 {\n    return U{ name: \"ada\" }\n  }\n\
         \x20 return nil\n}\n\
         fn name_of(id: int) -> str {\n  let u = find(id)\n\
         \x20 return if u == nil { \"anon\" } else { u.name }\n}\n\
         fn main() {\n  io.print(name_of(1))\n  io.print(name_of(2))\n}\n",
    );
}

#[test]
fn optionals_of_primitives_validate() {
    valid(
        "fn maybe(n: int) -> Option<int> {\n  if n > 0 {\n    return n\n  }\n  return nil\n}\n\
         fn main() {\n  let a = maybe(5)\n  io.print(if a == nil { 0 } else { a })\n\
         \x20 let b = maybe(-1)\n  io.print(if b == nil { 0 } else { b })\n}\n",
    );
}

#[test]
fn a_nil_pattern_validates() {
    valid(
        "fn maybe(n: int) -> Option<int> {\n  return nil\n}\n\
         fn main() {\n  io.print(match maybe(1) {\n    nil => \"none\",\n    v => \"some\",\n  })\n}\n",
    );
}

#[test]
fn optionals_are_no_longer_an_unsupported_construct() {
    assert!(gaps("fn f() -> Option<int> {\n  return nil\n}\nfn main() {\n}\n").is_empty());
}

// ---- slices ---------------------------------------------------------------

/// A slice is a WasmGC `array`. `array.get` traps when out of range, which is
/// exactly Kite's rule for `xs[i]`.
#[test]
fn slice_literals_and_reads_validate() {
    valid("fn main() {\n  let xs = [10, 20, 30]\n  io.print(xs.len())\n  io.print(xs[0])\n}\n");
}

#[test]
fn iterating_a_slice_validates() {
    valid("fn main() {\n  var total = 0\n  for x in [1, 2, 3] {\n    total = total + x\n  }\n  io.print(total)\n}\n");
}

/// Mutation copies first, which is what gives `[T]` value semantics.
#[test]
fn slice_mutation_validates() {
    valid("fn main() {\n  var xs = [1, 2]\n  xs.push(3)\n  xs[0] = 9\n  io.print(xs.len())\n  io.print(xs[0])\n}\n");
}

#[test]
fn get_yielding_an_optional_validates() {
    valid("fn main() {\n  let xs = [1, 2]\n  let a = xs.get(5)\n  io.print(if a == nil { -1 } else { a })\n}\n");
}

#[test]
fn slices_of_structs_validate() {
    valid("struct P {\n  n: int\n}\nfn main() {\n  let ps = [P{ n: 1 }, P{ n: 2 }]\n  io.print(ps[1].n)\n}\n");
}

#[test]
fn slices_are_no_longer_an_unsupported_construct() {
    assert!(gaps("fn main() {\n  let xs = [1]\n  io.print(xs.len())\n}\n").is_empty());
}

// ---- error handling -------------------------------------------------------

/// A fallible result is one GC object holding both slots, so a function can
/// return the pair without multi-value plumbing.
#[test]
fn fallible_results_validate() {
    valid(
        "fn divide(a: int, b: int) -> (int, error) {\n  if b == 0 {\n\
         \x20   return _, errors.new(\"division by zero\")\n  }\n  return a / b, nil\n}\n\
         fn ratio(a: int, b: int) -> (int, error) {\n  let (q, err) = divide(a, b)\n\
         \x20 check err\n  return q * 100, nil\n}\n\
         fn main() {\n  let (r, err) = ratio(10, 2)\n  if err != nil {\n\
         \x20   io.print(err.message())\n  } else {\n    io.print(r)\n  }\n}\n",
    );
}

#[test]
fn error_handling_is_no_longer_an_unsupported_construct() {
    assert!(gaps("fn f() -> (int, error) {\n  return 1, nil\n}\nfn main() {\n}\n").is_empty());
}

// ---- maps -----------------------------------------------------------------

/// A map is a record holding parallel key and value arrays. Lookup is a linear
/// scan, which is what makes insertion order and first-match-wins obviously
/// right; a hash index is an optimisation for later.
#[test]
fn map_reads_validate() {
    valid(
        "fn main() {\n  let m = {\"a\": 1, \"b\": 2}\n  io.print(m.len())\n\
         \x20 let a = m[\"a\"]\n  io.print(if a == nil { -1 } else { a })\n}\n",
    );
}

#[test]
fn maps_with_integer_keys_validate() {
    valid("fn main() {\n  let m = {1: \"one\"}\n  let v = m[1]\n  io.print(if v == nil { \"?\" } else { v })\n}\n");
}

/// A map write builds new arrays and rebinds, which is what gives maps value
/// semantics. One code path covers replacing a key and appending one.
#[test]
fn map_writes_validate() {
    valid(
        "fn main() {\n  var m = {\"a\": 1}\n  m[\"b\"] = 2\n  m[\"a\"] = 9\n\
         \x20 io.print(m.len())\n}\n",
    );
    assert!(gaps("fn main() {\n  var m = {\"a\": 1}\n  m[\"b\"] = 2\n}\n").is_empty());
}
