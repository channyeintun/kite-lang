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
    let mut hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
    kite_hir::mono::monomorphise(&mut hir).expect("specialisation terminates");
    let mut mir = kite_mir::lower(&hir);
    // As the driver does: a backend must never see an `await`.
    kite_mir::asyncify(&mut mir, &mut hir.types);
    Built {
        module: compile(&mir, &hir.types),
    }
}

/// Validate, which is the assertion that matters for every one of these.
fn valid(src: &str) -> Built {
    let b = build(src);
    b.validate();
    b
}

/// The debug-build overflow checks are written out by hand — several
/// instructions with nested blocks, where the validator is the only thing
/// standing between a stack mistake and a browser.
#[test]
fn checked_integer_arithmetic_validates() {
    valid("fn main() {\n  var x = 2\n  x = x + 3\n  x = x - 1\n  x = x * 4\n  io.print(x)\n}\n");
}

/// The same arithmetic reached through every operand shape the emitter has to
/// hold across the check.
#[test]
fn checked_arithmetic_on_call_results_validates() {
    valid(
        "fn f(n: int) -> int {\n  return n * n + n - 1\n}\n\
         fn main() {\n  io.print(f(3) * f(4))\n}\n",
    );
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
    valid(
        "fn main() {\n  let a = true\n  let b = false\n  io.print(a && b)\n  io.print(a || b)\n}\n",
    );
}

/// The MIR of a program, as `build` lowers it.
fn lowered(src: &str) -> kite_mir::Program {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(!diags.has_errors(), "{}", diags.render_all(&sources));
    kite_hir::mono::monomorphise(&mut hir).expect("specialisation terminates");
    kite_mir::lower(&hir)
}

/// How many jumps in `name` go back, laid out as the backend lays it out.
fn jumps_back(program: &kite_mir::Program, name: &str) -> usize {
    let f = program.fns.iter().find(|f| f.name == name).expect("the function is there");
    let order = emission_order(f);
    let mut place = vec![0; order.len()];
    for (at, &b) in order.iter().enumerate() {
        place[b] = at;
    }
    order
        .iter()
        .enumerate()
        .flat_map(|(at, &b)| f.blocks[b].term.successors().into_iter().map(move |s| (at, s)))
        .filter(|&(at, s)| place[s.index()] <= at)
        .count()
}

/// Code with no loop in it is laid out so that every jump goes forward, and
/// none goes round the dispatch loop.
///
/// Every jump used to, which made each block a successor of the loop's head
/// and each jump an edge back to it. V8's optimising compiler, given a chain
/// of two thousand `||`, merged every local at that head from every block,
/// and took gigabytes and most of a minute after the program had finished.
/// A value `else if` chain jumps back even in MIR's own numbering, which
/// makes each arm's join before the arms inside it.
#[test]
fn code_without_a_loop_only_jumps_forward() {
    let src = "fn main() {\n  let c = 7\n  let b = c == 0 || c == 1 || c == 2 || c == 3\n\
               \x20 let v = if c == 0 { 0 } else if c == 1 { 1 } else if c == 2 { 2 } else { 3 }\n\
               \x20 io.print(b)\n  io.print(v)\n}\n";
    let program = lowered(src);
    let main = program.fns.iter().find(|f| f.name == "main").expect("main");
    let numbered_back = main
        .blocks
        .iter()
        .enumerate()
        .any(|(i, b)| b.term.successors().iter().any(|s| s.index() <= i));
    assert!(numbered_back, "the value `else if` chain jumps back in MIR's numbering");
    assert_eq!(jumps_back(&program, "main"), 0);
    valid(src);
    // A loop still goes back to its head, and only there.
    let src = "fn main() {\n  var i = 0\n  for i < 3 {\n    if i == 1 {\n      io.print(i)\n    }\n\
               \x20   i = i + 1\n  }\n}\n";
    assert_eq!(jumps_back(&lowered(src), "main"), 1);
    valid(src);
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

/// String conversion uses one fixed imported page and can never grow it.
#[test]
fn the_module_has_only_fixed_scratch_memory() {
    let b = valid(HELLO);
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    assert!(
        printed.contains(r#"(import "kite" "scratch" (memory (;0;) 1 1))"#),
        "{}",
        printed
    );
}

#[test]
fn string_constants_are_language_owned_arrays() {
    let b = valid(HELLO);
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    for scalar in ['b' as u32, 'i' as u32, 'g' as u32] {
        assert!(
            printed.contains(&format!("i32.const {}", scalar)),
            "literal scalar missing:\n{}",
            printed
        );
    }
    assert!(printed.contains("array.new_fixed"), "{}", printed);
    let g = generate_glue("app.wasm");
    assert!(!g.contains(r#""big""#));
}

#[test]
fn strings_run_as_unicode_scalar_arrays_under_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: node is not installed");
        return;
    }
    let src = "fn main() {\n\
               \x20 let s = \"hé😀日\"\n\
               \x20 io.print(s.len())\n\
               \x20 io.print(s.code_at(2))\n\
               \x20 io.print(s.slice(1, 3))\n\
               \x20 io.print(s.index_of(\"😀\"))\n\
               \x20 io.print(\"x\" + s == \"xhé😀日\")\n\
               \x20 io.print(\" \\u{2003}trim\\u{3000} \".trim())\n\
               \x20 io.print(text.from_code(0x1F680))\n\
               \x20 io.print(text.from_code(0xD800))\n\
               }\n";
    let built = valid(src);
    let dir = std::env::temp_dir().join(format!("kite-scalar-strings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    std::fs::write(dir.join("app.wasm"), &built.module.bytes).expect("write wasm");
    std::fs::write(dir.join("app.js"), generate_glue("app.wasm")).expect("write glue");
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((line) => out.push(line));\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         process.stdout.write(out.map((line) => line + \"\\n\").join(\"\"));\n",
    )
    .expect("write runner");
    let output = std::process::Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf-8"),
        "4\n128512\né😀\n2\ntrue\ntrim\n🚀\n\n"
    );
}

#[test]
fn exported_strings_cross_the_boundary_in_bounded_chunks() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: node is not installed");
        return;
    }
    let built = valid("pub fn echo(body: str) -> str {\n  return body + \"!\"\n}\n");
    let dir = std::env::temp_dir().join(format!("kite-string-boundary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    std::fs::write(dir.join("app.wasm"), &built.module.bytes).expect("write wasm");
    std::fs::write(dir.join("app.js"), generate_glue("app.wasm")).expect("write glue");
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { instantiate, str, text } from \"./app.js\";\n\
         const bytes = new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url)));\n\
         const kite = await instantiate(bytes);\n\
         const input = \"😀\".repeat(5000);\n\
         const output = text(kite.echo(str(input)));\n\
         process.stdout.write(String([...output].length) + \":\" + String(output === input + \"!\"));\n",
    )
    .expect("write runner");
    let output = std::process::Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf-8"),
        "5001:true"
    );
}

#[test]
fn declared_hosts_receive_and_return_plain_javascript_strings() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: node is not installed");
        return;
    }
    let built = valid(
        "@host(\"paint\")\nextern fn surround(body: str) -> str\n\
         fn main() {\n  io.print(surround(\"hé😀\"))\n}\n",
    );
    let dir = std::env::temp_dir().join(format!("kite-string-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    std::fs::write(dir.join("app.wasm"), &built.module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        generate_glue_with_hosts("app.wasm", &built.module.hosts),
    )
    .expect("write glue");
    std::fs::write(
        dir.join("run.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { provide, run, setWriter } from \"./app.js\";\n\
         provide(\"paint\", { surround: (body) => \"[\" + body + \"]\" });\n\
         setWriter((line) => process.stdout.write(line));\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n",
    )
    .expect("write runner");
    let output = std::process::Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).expect("utf-8"), "[hé😀]");
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
        "{}\nfn main() {{\n  var r = Rect{{ width: 1, height: 1, scale: 1 }}\n  r.scale = 9\n  io.print(r.scale)\n}}\n",
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
    assert!(
        printed.contains("(mut i64)"),
        "no mutable field emitted:\n{}",
        printed
    );
    assert!(
        printed.contains("(struct"),
        "no struct type emitted:\n{}",
        printed
    );
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

/// An arm that `return`s never reaches the join, so nothing may be written to
/// the match's result there.
///
/// Lowering used to write the arm's `()` into the result anyway, in a block
/// nothing branches to. It cost nothing at run time and validated as nothing:
/// the result local carries the match's type, so the dead store put an `i32`
/// unit into an `i64`. `check` was clean, `build` succeeded, and the module
/// failed in the engine — reported from an application whose page came up
/// blank.
#[test]
fn an_arm_that_returns_validates() {
    valid(
        "enum E {\n  A\n  B\n}\n\
         fn f(e: E) -> int {\n  return match e {\n    A => {\n      return 1\n    },\n    B => 2,\n  }\n}\n\
         fn main() {\n  io.print(f(A))\n}\n",
    );
}

/// The same lowering, with the match bound rather than returned.
#[test]
fn a_returning_arm_in_a_bound_match_validates() {
    valid(
        "enum E {\n  A\n  B\n}\n\
         fn f(e: E) -> int {\n  let v = match e {\n    A => {\n      return 1\n    },\n    B => 2,\n  }\n  return v\n}\n\
         fn main() {\n  io.print(f(B))\n}\n",
    );
}

/// The mismatch the dead store caused followed the function's return type, so
/// a reference type is a distinct shape from `i64` — this is where it read as
/// `expected (ref null N), found i32`.
#[test]
fn a_returning_arm_validates_for_reference_results() {
    valid(
        "enum E {\n  A\n  B\n}\nstruct P {\n  x: int\n}\n\
         fn p(e: E) -> P {\n  return match e {\n    A => {\n      return P{ x: 1 }\n    },\n    B => P{ x: 2 },\n  }\n}\n\
         fn s(e: E) -> [int] {\n  return match e {\n    A => {\n      return [1]\n    },\n    B => [2],\n  }\n}\n\
         fn t(e: E) -> str {\n  return match e {\n    A => {\n      return \"a\"\n    },\n    B => \"b\",\n  }\n}\n\
         fn main() {\n  io.print(p(A).x)\n  io.print(s(B)[0])\n  io.print(t(A))\n}\n",
    );
}

/// An arm that leaves through a nested `if`, where both branches return.
///
/// Whether the arm diverges cannot be read from the lowerer's own state here:
/// lowering an `if` statement ends by switching to its join block, so the arm
/// looks like ordinary reachable code even though no path arrives. The
/// checker's `!` is the verdict that holds for every shape.
#[test]
fn an_arm_leaving_through_both_branches_of_an_if_validates() {
    valid(
        "enum E {\n  A\n  B\n}\n\
         fn f(e: E, k: bool) -> int {\n  return match e {\n    A => {\n      if k {\n        return 1\n      } else {\n        return 2\n      }\n    },\n    B => 3,\n  }\n}\n\
         fn main() {\n  io.print(f(A, true))\n}\n",
    );
}

/// An arm that leaves the enclosing loop rather than the function.
#[test]
fn an_arm_that_breaks_validates() {
    valid(
        "enum E {\n  A\n  B\n}\n\
         fn f(e: E) -> int {\n  var total = 0\n  for i in 0..3 {\n    total = total + match e {\n      A => {\n        break\n      },\n      B => 2,\n    }\n  }\n  return total\n}\n\
         fn main() {\n  io.print(f(B))\n}\n",
    );
}

/// Every arm a block that returns. A value block must be a single expression,
/// so this is how a multi-statement arm is written, and it has to reach codegen
/// rather than being turned back at the checker.
#[test]
fn a_match_whose_arms_all_return_validates() {
    valid(
        "enum E {\n  A\n  B\n}\n\
         fn f(e: E) -> int {\n  match e {\n    A => {\n      let n = 1\n      return n\n    },\n    B => {\n      return 2\n    },\n  }\n}\n\
         fn main() {\n  io.print(f(A))\n}\n",
    );
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
    assert!(
        printed.contains("(sub "),
        "variants are not subtypes:\n{}",
        printed
    );
}

// ---- honest about what is not lowered -------------------------------------

fn gaps(src: &str) -> Vec<String> {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(!diags.has_errors(), "{}", diags.render_all(&sources));
    kite_hir::mono::monomorphise(&mut hir).expect("specialisation terminates");
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
    assert!(
        gaps(everything).is_empty(),
        "unexpected gaps: {:?}",
        gaps(everything)
    );
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

/// Every source-map entry points at a real function body.
///
/// The offset arithmetic is the part of a source map that can be wrong while
/// everything still parses: a map whose entries are all one byte early is a
/// valid map, loads without complaint, and resolves every frame to the wrong
/// place. The first version of this was exactly that — it left out the
/// function-count LEB at the head of the code section's payload — so the test
/// walks the module's own body offsets and checks membership rather than
/// checking that a map was produced.
#[test]
fn every_source_map_entry_lands_on_a_function_body() {
    let src = "\
fn helper(n: int) -> int {
    return n * 2
}

fn other(n: int) -> int {
    return n + 1
}

fn main() {
    io.print(\"\\(helper(21))\")
    io.print(\"\\(other(1))\")
}
";
    let built = build(src);
    let bytes = &built.module.bytes;

    // The code section's body offsets, read out of the module itself.
    let mut bodies: Vec<usize> = Vec::new();
    let mut p = 8usize; // magic and version
    let leb = |bytes: &[u8], p: &mut usize| -> u64 {
        let (mut result, mut shift) = (0u64, 0u32);
        loop {
            let byte = bytes[*p];
            *p += 1;
            result |= ((byte & 0x7f) as u64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return result;
            }
        }
    };
    while p < bytes.len() {
        let id = bytes[p];
        p += 1;
        let size = leb(bytes, &mut p) as usize;
        let end = p + size;
        if id == 10 {
            let count = leb(bytes, &mut p);
            for _ in 0..count {
                bodies.push(p);
                let body = leb(bytes, &mut p) as usize;
                p += body;
            }
        }
        p = end;
    }
    assert!(!bodies.is_empty(), "the module has no code section");

    assert!(
        !built.module.source_spans.is_empty(),
        "no source spans were recorded"
    );
    for (offset, _) in &built.module.source_spans {
        assert!(
            bodies.contains(offset),
            "source map offset {} is not the start of any function body; bodies are {:?}",
            offset,
            bodies
        );
    }
}

/// The module names its map, and names it in the shape a browser reads.
#[test]
fn the_module_carries_a_source_mapping_url() {
    let built = build("fn main() {\n    io.print(1)\n}\n");
    let bytes = &built.module.bytes;
    let needle = b"sourceMappingURL";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("a sourceMappingURL section");
    // Name, then the URL as a length-prefixed string. Written raw instead, a
    // browser reads the first character as the length and fetches a file whose
    // name is missing its first letter.
    let after = at + needle.len();
    assert_eq!(
        bytes[after] as usize,
        SOURCE_MAP_NAME.len(),
        "the URL must be length-prefixed"
    );
    assert_eq!(
        &bytes[after + 1..after + 1 + SOURCE_MAP_NAME.len()],
        SOURCE_MAP_NAME.as_bytes()
    );
}

/// The text of the function the name section calls `name`, as printed: its
/// local declarations, then its body.
fn function_text(b: &Built, name: &str) -> String {
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    let start = printed
        .find(&format!("(func ${} ", name))
        .unwrap_or_else(|| panic!("no function `{}`", name));
    let rest = &printed[start..];
    let end = rest[1..].find("\n  (").map(|e| e + 1).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// How many locals a printed function declares, parameters aside.
fn declared_locals(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("(local "))
        .map(|l| l.matches(" i32").count() + l.matches(" i64").count() + l.matches(" f64").count()
            + l.matches(" (ref").count())
        .sum()
}

/// A module's own entry points are spelled so no `pub fn` can take them.
///
/// They were `kite_poll` and `kite_invoke_0`, beside the program's exports,
/// so a program naming a function either exported the name twice and the
/// module was refused as invalid — reported as a bug in the compiler.
#[test]
fn a_pub_fn_cannot_collide_with_the_module_s_own_exports() {
    let b = valid(
        "pub fn kite_poll(n: int) -> int {\n  return n + 1\n}\n\
         async fn later() -> int {\n  task.yield()\n  return kite_poll(1)\n}\n\
         async fn main() {\n  io.print(await later())\n}\n",
    );
    let printed = wasmprinter::print_bytes(&b.module.bytes).unwrap();
    assert!(printed.contains(&format!("(export \"{}\"", POLL_EXPORT)), "{}", printed);
    assert!(printed.contains("(export \"kite_poll\""), "{}", printed);
}

/// A debug build's checked arithmetic is a call in the function doing it,
/// not an `if` there.
///
/// V8's baseline compiler copies its record of every local at each `if`, and
/// keeps the copies until the function is done: twenty thousand `let v = i + 1`
/// were twenty thousand `if`s, and 9.3 GB to load.
#[test]
fn checked_arithmetic_leaves_its_caller_straight() {
    let mut src = String::from("fn main() {\n  var k = 1\n");
    for i in 0..500 {
        src.push_str(&format!("  let v{} = k * {} + {} - k\n", i, i, i));
    }
    src.push_str("  io.print(v499 << 2 >> 1)\n  io.print(-k)\n}\n");
    let b = valid(&src);
    let main = function_text(&b, "main");
    // The dispatch loop's own blocks aside, nothing in `main` branches.
    assert!(!main.contains(" if "), "{}", &main[..main.len().min(2000)]);
    assert!(!main.contains("\n      if"), "{}", &main[..main.len().min(2000)]);
}

/// A block's temporaries share the function's Wasm locals: a function of
/// many `if` statements, each computing its condition from scratch, needs a
/// few, not one per temporary.
///
/// V8's cost for a function grows with its locals times its branches, in
/// both of its compilers: 1,500 such `if`s were 4,500 locals, and took over a
/// minute and 2 GB to load, debug build or release.
#[test]
fn a_block_s_temporaries_share_locals() {
    let mut src = String::from("fn main() {\n  var n = 0\n  var t = 0\n");
    for i in 0..1500 {
        src.push_str(&format!("  if (n + {}) % 3 == 0 {{\n    t = t + {}\n  }}\n", i, i));
    }
    src.push_str("  io.print(t)\n}\n");
    let b = valid(&src);
    let locals = declared_locals(&function_text(&b, "main"));
    assert!(locals < 20, "{} locals", locals);
}

/// A long map literal of computed entries is a literal, then one `set` call
/// per entry: no loop in the function it is in. Each `m[k] = v` looped over
/// the keys inline, so a literal of nine thousand entries was a function of
/// nine thousand hot loops, which the engine recompiled whole: minutes.
#[test]
fn a_map_write_is_a_call_not_a_loop_in_its_caller() {
    let entries: Vec<String> = (0..2000).map(|i| format!("\"s{}\": [k, {}]", i, i)).collect();
    let b = valid(&format!(
        "fn main() {{\n  var k = 0\n  let m = {{{}}}\n  io.print(m.len())\n}}\n",
        entries.join(", ")
    ));
    let main = function_text(&b, "main");
    assert_eq!(main.matches("loop").count(), 1, "only the dispatch loop");
}

/// Constant keys are collapsed as the literal is compiled, at any length and
/// repeated or not, so no run-time pass is emitted for them; a key read from
/// a local gets one. The check gave up past 4,096 keys and ran the quadratic
/// pass on a table of distinct names.
#[test]
fn constant_keys_are_collapsed_as_the_literal_is_compiled() {
    let functions = |last: &str| {
        let mut entries: Vec<String> = (0..5000).map(|i| format!("\"s{}\": {}", i, i)).collect();
        entries.push(last.to_string());
        let b = valid(&format!(
            "fn main() {{\n  let k = \"s0\"\n  let m = {{{}}}\n  io.print(m.len())\n}}\n",
            entries.join(", ")
        ));
        wasmprinter::print_bytes(&b.module.bytes).unwrap().matches("\n  (func ").count()
    };
    let distinct = functions("\"s5000\": 9");
    assert_eq!(functions("\"s0\": 9"), distinct);
    assert_eq!(functions("k: 9"), distinct + 1);
}

/// A stack frame gets a name, which is the half of §16 the map cannot do.
#[test]
fn the_module_carries_a_name_section() {
    let built = build("fn helper() -> int {\n    return 1\n}\nfn main() {\n    io.print(helper())\n}\n");
    let bytes = &built.module.bytes;
    for wanted in ["helper", "main"] {
        assert!(
            bytes
                .windows(wanted.len())
                .any(|w| w == wanted.as_bytes()),
            "`{}` is not named in the module",
            wanted
        );
    }
}
