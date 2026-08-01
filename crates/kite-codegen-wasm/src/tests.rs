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
    let hir = kite_types::check(&ast, &resolved, src, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
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
