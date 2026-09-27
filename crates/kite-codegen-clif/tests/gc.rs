//! The collector, made to actually run.
//!
//! A GC that is never made to run is a GC that is not known to work, so these
//! tests shrink the nursery to a few kilobytes, allocate far more than it
//! holds, keep a live structure across the collections that forces, and then
//! check the structure is intact — against the bytecode VM's answer, which
//! shares none of the collector's machinery. The run's own collection counts
//! are asserted so a future change that quietly stops collections from
//! happening fails here rather than passing vacuously — and they are the
//! run's own, counted under the lock that serialises runs: a process-wide
//! counter read around the call once let a neighbouring test's collections
//! stand in for this one's.

mod common;

use kite_codegen_clif::RunConfig;

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
    let config = RunConfig { nursery_bytes: Some(SMALL_NURSERY), ..RunConfig::default() };
    let (native, stats) = common::run_native_with(src, config);
    assert_eq!(vm, native, "the VM and the collected native run disagree");
    assert!(
        stats.minor_collections > 10,
        "the nursery was sized to force collections, and only {} ran",
        stats.minor_collections
    );
}

/// The same, with the old generation's threshold lowered until mark-and-sweep
/// runs too. The default threshold is 8 MB, which no test program reaches, so
/// without this every major collection path — marking from the stack maps,
/// sweeping, the threshold's own arithmetic — went untested.
fn agree_with_major_gc(src: &str) {
    let vm = common::run_vm(src);
    let config = RunConfig {
        nursery_bytes: Some(SMALL_NURSERY),
        major_threshold: Some(32 << 10),
    };
    let (native, stats) = common::run_native_with(src, config);
    assert_eq!(vm, native, "the VM and the collected native run disagree");
    assert!(
        stats.major_collections > 3,
        "the threshold was lowered to force major collections, and only {} ran",
        stats.major_collections
    );
}

#[test]
fn a_live_list_survives_many_collections() {
    if unsupported_here() {
        return;
    }
    // The list is live from the first allocation to the last print, while
    // the loop churns through far more garbage than the nursery holds. The
    // list is promoted by an early collection and then pushed onto in place,
    // so most new elements are nursery objects stored into an old one, which
    // the next collection finds only through the remembered set.
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
         fn main() {\n  var h = Holder{latest: \"start\", count: 0}\n\
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

/// A loop of pushes, and then of index writes, is linear.
///
/// Every push and every `xs[i] = v` used to copy the whole slice, so building
/// one by pushing was quadratic: a hundred thousand pushes took four seconds
/// natively where the VM took sixteen milliseconds. The count of collections
/// is what this measures, because it does not depend on how fast the machine
/// is: once a slice outgrows this nursery, each copy of it is an allocation
/// the nursery cannot hold, and each of those is a collection — twenty
/// thousand pushes past that point were twenty thousand collections. Written
/// in place, the slice is reallocated only as its room doubles.
#[test]
fn a_loop_of_pushes_is_linear() {
    if unsupported_here() {
        return;
    }
    let src = "fn main() {\n  var xs: [int] = []\n\
               \x20 for i in 0..20000 {\n    xs.push(i)\n  }\n\
               \x20 for i in 0..20000 {\n    xs[i] = xs[i] * 3\n  }\n\
               \x20 var sum = 0\n  for x in xs {\n    sum = sum + x\n  }\n\
               \x20 io.print(xs.len())\n  io.print(sum)\n}\n";
    let vm = common::run_vm(src);
    let config = RunConfig { nursery_bytes: Some(SMALL_NURSERY), ..RunConfig::default() };
    let (native, stats) = common::run_native_with(src, config);
    assert_eq!(vm, native, "the VM and the collected native run disagree");
    assert!(
        stats.minor_collections < 100,
        "{} collections for 40,000 writes to one slice: each write is copying it",
        stats.minor_collections
    );
}

/// A slice written in place after it was promoted keeps the young values
/// written into it.
///
/// An in-place write is the second heap mutation the collector must be told
/// about, after a `var` field: a nursery object stored into an old slice is
/// reachable only through that slice, and a minor collection does not trace
/// the old generation. The first slice here is promoted by an early
/// collection; the second outgrows the nursery and is born old. Both then
/// have fresh structs and strings pushed and written into them while the
/// loop churns, and every one is read back at the end — a missed barrier is
/// a dangling element, and a wrong total or a crash here.
#[test]
fn slices_written_in_place_keep_their_young_elements() {
    if unsupported_here() {
        return;
    }
    let src = "struct P {\n  x: int\n  name: str\n}\n\
               fn main() {\n  var small: [P] = []\n  var big: [str] = []\n\
               \x20 for i in 0..3000 {\n\
               \x20   small.push(P{x: i, name: \"p\\(i)\"})\n\
               \x20   big.push(\"b\\(i)\")\n\
               \x20   if small.len() > 200 {\n      small[i % 200] = P{x: -i, name: \"q\\(i)\"}\n    }\n\
               \x20   var scratch: [int] = []\n\
               \x20   for j in 0..8 {\n      scratch.push(j)\n    }\n  }\n\
               \x20 for round in 0..3 {\n    for i in 0..3000 {\n\
               \x20     big[i] = \"r\\(round) \\(i)\"\n    }\n  }\n\
               \x20 var sum = 0\n  var chars = 0\n\
               \x20 for p in small {\n    sum = sum + p.x\n    chars = chars + p.name.len()\n  }\n\
               \x20 for s in big {\n    chars = chars + s.len()\n  }\n\
               \x20 io.print(small.len())\n  io.print(sum)\n  io.print(chars)\n\
               \x20 io.print(small[0].name)\n  io.print(small[2999].name)\n\
               \x20 io.print(big[0])\n  io.print(big[2999])\n}\n";
    agree_with_gc(src);
    agree_with_major_gc(src);
}

#[test]
fn major_collections_sweep_the_dead_and_keep_the_live() {
    if unsupported_here() {
        return;
    }
    // Everything here survives a minor collection long enough to be promoted
    // — the tree through the builder's frames, the list through `main`'s, the
    // holder's field through the remembered set — and then most of it dies in
    // the old generation: every push copies the list, because a snapshot of
    // it was taken first and a push may not write into what the snapshot
    // sees, and every copy but the last is garbage. Only mark-and-sweep can
    // reclaim that, and a mark that missed a root would free something still
    // printed below. (A push into a list nothing else holds writes in place,
    // so without the snapshot there is too little garbage to sweep.)
    agree_with_major_gc(
        "struct Holder {\n  var latest: str\n  var count: int\n}\n\
         enum Tree {\n  Leaf(int)\n  Node(left: Tree, right: Tree)\n}\n\
         fn build(depth: int, n: int) -> Tree {\n\
         \x20 if depth == 0 {\n    return Leaf(n)\n  }\n\
         \x20 let l = build(depth - 1, n * 2)\n\
         \x20 var waste: [str] = []\n\
         \x20 for i in 0..20 {\n    waste.push(\"pad \\(i)\")\n  }\n\
         \x20 let r = build(depth - 1, n * 2 + 1)\n\
         \x20 return Node(left: l, right: r)\n}\n\
         fn total(t: Tree) -> int {\n  return match t {\n    Leaf(n) => n,\n\
         \x20   Node(l, r) => total(l) + total(r),\n  }\n}\n\
         fn main() {\n  var h = Holder{latest: \"start\", count: 0}\n\
         \x20 var keep: [str] = []\n\
         \x20 for i in 0..400 {\n\
         \x20   let before = keep\n\
         \x20   keep.push(\"item \\(i)\")\n\
         \x20   if before.len() + 1 != keep.len() {\n      io.print(\"impossible\")\n    }\n\
         \x20   h.latest = \"value \\(i)\"\n\
         \x20   h.count = h.count + 1\n  }\n\
         \x20 let t = build(9, 1)\n  io.print(total(t))\n\
         \x20 io.print(keep.len())\n  io.print(keep[0])\n  io.print(keep[399])\n\
         \x20 io.print(h.latest)\n  io.print(h.count)\n}\n",
    );
}

/// A root the whole depth of the stack up is still a root.
///
/// The walk used to give up after a million frames, so a reference held by
/// `main` while a deep recursion collected was never updated: the nursery was
/// reset under it, and the next allocations wrote over the string. A native
/// recursion now stops where the VM's does, at `MAX_FRAMES` calls, so this
/// goes as deep as a program can — every frame but `main`'s and `churn`'s is
/// a `down` — collects at the bottom, and holds the VM to the same answer.
#[test]
fn a_root_at_the_top_of_the_deepest_stack_survives() {
    if unsupported_here() {
        return;
    }
    assert_eq!(kite_codegen_clif::MAX_FRAMES as usize, kite_vm::MAX_FRAMES, "the two limits differ");
    // `main`, then `down` from DEPTH to zero, then `churn`.
    let depth = kite_vm::MAX_FRAMES - 3;
    let src = format!(
        "fn churn() -> int {{\n  var n = 0\n\
         \x20 for i in 0..2000 {{\n    let s = \"garbage \\(i)\"\n    n = n + s.len()\n  }}\n\
         \x20 return n\n}}\n\
         fn down(d: int) -> int {{\n  if d == 0 {{\n    return churn()\n  }}\n\
         \x20 return down(d - 1) + 1\n}}\n\
         fn main() {{\n  let keep = \"kept \\(7)\"\n  io.print(down({}))\n  io.print(keep)\n}}\n",
        depth
    );
    let churned: usize = (0..2000).map(|i| format!("garbage {}", i).len()).sum();
    // The runtime runs the program on a stack of its own, deep enough for
    // every frame the limit allows; the test's thread needs none of its own.
    let config = RunConfig { nursery_bytes: Some(SMALL_NURSERY), ..RunConfig::default() };
    let (out, stats) = common::run_native_with(&src, config);
    assert_eq!(out, format!("{}\nkept 7\n", depth + churned));
    assert!(stats.minor_collections > 0, "the bottom of the recursion never collected");
    assert_eq!(common::run_vm(&src), out, "the VM and the native backend disagree at the limit");
}
