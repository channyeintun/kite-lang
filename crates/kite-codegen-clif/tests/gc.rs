//! The collector, made to actually run.
//!
//! A GC that is never made to run is a GC that is not known to work, so these
//! tests shrink the nursery to a few kilobytes, allocate far more than it
//! holds, keep a live structure across the collections that forces, and then
//! check the structure is intact — against the bytecode VM's answer, which
//! shares none of the collector's machinery. `kite_rt_gc_count` is asserted
//! so a future change that quietly stops collections from happening fails
//! here rather than passing vacuously.

mod common;

/// The backend refuses Windows — its own `supported_here` says why — so its
/// tests say so rather than failing there.
fn unsupported_here() -> bool {
    if let Err(why) = kite_codegen_clif::supported_here() {
        eprintln!("skipping: {}", why);
        return true;
    }
    false
}

/// Nursery small enough that every test below must collect many times.
const SMALL_NURSERY: usize = 16 << 10;

fn agree_with_gc(src: &str) {
    let vm = common::run_vm(src);
    kite_rt::set_nursery_bytes(SMALL_NURSERY);
    let before = kite_rt::gc_runs();
    let native = common::run_native(src);
    let collections = kite_rt::gc_runs() - before;
    assert_eq!(vm, native, "the VM and the collected native run disagree");
    assert!(
        collections > 10,
        "the nursery was sized to force collections, and only {} ran",
        collections
    );
}

#[test]
fn a_live_list_survives_many_collections() {
    if unsupported_here() {
        return;
    }
    // The list is live from the first allocation to the last print, while
    // the loop churns through far more garbage than the nursery holds —
    // every element is re-boxed on each push because the slice is
    // copy-on-write, so the survivors are re-reached through fresh copies.
    agree_with_gc(
        "struct P {\n  x: int\n  y: str\n}\n\
         fn main() {\n  var keep: [P] = []\n  var junk = 0\n\
         \x20 for i in 0..500 {\n\
         \x20   keep.push(P{x: i, y: \"n\\(i)\"})\n\
         \x20   var scratch: [int] = []\n\
         \x20   for j in 0..50 {\n      scratch.push(i * j)\n    }\n\
         \x20   junk = junk + scratch[49]\n  }\n\
         \x20 io.print(keep.len())\n  io.print(keep[0].x)\n  io.print(keep[0].y)\n\
         \x20 io.print(keep[499].x)\n  io.print(keep[499].y)\n\
         \x20 var sum = 0\n  for p in keep {\n    sum = sum + p.x\n  }\n\
         \x20 io.print(sum)\n  io.print(junk)\n}\n",
    );
}

#[test]
fn a_deep_structure_survives_via_stack_roots() {
    if unsupported_here() {
        return;
    }
    // The tree under construction is reachable only through locals of the
    // recursive builder — precisely the frames the stack maps must describe.
    // A missed root shows up as a corrupt total or a crash, not a quiet pass.
    agree_with_gc(
        "enum Tree {\n  Leaf(int)\n  Node(left: Tree, right: Tree)\n}\n\
         fn build(depth: int, n: int) -> Tree {\n\
         \x20 if depth == 0 {\n    return Leaf(n)\n  }\n\
         \x20 let l = build(depth - 1, n * 2)\n\
         \x20 var waste: [str] = []\n\
         \x20 for i in 0..20 {\n    waste.push(\"pad \\(i)\")\n  }\n\
         \x20 let r = build(depth - 1, n * 2 + 1)\n\
         \x20 io.print(waste.len())\n\
         \x20 return Node(left: l, right: r)\n}\n\
         fn total(t: Tree) -> int {\n  return match t {\n    Leaf(n) => n,\n\
         \x20   Node(l, r) => total(l) + total(r),\n  }\n}\n\
         fn main() {\n  let t = build(8, 1)\n  io.print(total(t))\n}\n",
    );
}

#[test]
fn old_objects_written_after_promotion_are_remembered() {
    if unsupported_here() {
        return;
    }
    // The holder is promoted early, then keeps having young values stored
    // into its `var` field — the old-to-nursery edges only the write barrier
    // can see. If the remembered set were broken, the field would be read
    // back as a dangling or stale reference after the next collection.
    agree_with_gc(
        "struct Holder {\n  var latest: str\n  var count: int\n}\n\
         fn main() {\n  let h = Holder{latest: \"start\", count: 0}\n\
         \x20 for i in 0..800 {\n\
         \x20   h.latest = \"value \\(i)\"\n\
         \x20   h.count = h.count + 1\n\
         \x20   var churn: [int] = []\n\
         \x20   for j in 0..40 {\n      churn.push(j)\n    }\n\
         \x20   if churn.len() != 40 {\n      io.print(\"impossible\")\n    }\n  }\n\
         \x20 io.print(h.latest)\n  io.print(h.count)\n}\n",
    );
}

#[test]
fn maps_and_closures_survive_collections() {
    if unsupported_here() {
        return;
    }
    agree_with_gc(
        "fn main() {\n  var m: {str: int} = {}\n\
         \x20 for i in 0..300 {\n    m[\"k\\(i)\"] = i * i\n  }\n\
         \x20 io.print(m.len())\n\
         \x20 let a = m[\"k7\"]\n  io.print(if a == nil { -1 } else { a })\n\
         \x20 let z = m[\"k299\"]\n  io.print(if z == nil { -1 } else { z })\n\
         \x20 let base = 1000\n  let f = |x: int| x + base\n\
         \x20 var noise = 0\n  for i in 0..200 {\n\
         \x20   var pad: [str] = []\n    for j in 0..10 {\n      pad.push(\"x\\(j)\")\n    }\n\
         \x20   noise = noise + pad.len()\n  }\n\
         \x20 io.print(f(1))\n  io.print(noise)\n}\n",
    );
}
