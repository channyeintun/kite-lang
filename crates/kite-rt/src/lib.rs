//! The native runtime: the collector, the host functions, and the scheduler.
//!
//! This crate is the half of the native backend that runs at run time. The
//! other half, `kite-codegen-clif`, emits machine code that calls into the
//! `extern "C"` surface declared here — the same surface whether the code was
//! JIT-compiled into this process or linked into a standalone executable
//! against the `staticlib` build of this crate.
//!
//! # The object model
//!
//! Every Kite aggregate is a **self-describing** heap object: a two-word
//! header naming what it is, then its payload as 8-byte slots. The bytecode
//! VM's values carry their own tags and the Wasm backend leans on the engine's
//! typed GC records; a native heap has neither, so the header plus a set of
//! registered *shape tables* (which slots of a struct are references, and of
//! each enum variant, tuple and closure environment) is what stands in.
//! Self-description is what lets one collector trace everything, one routine
//! render any value the way the VM's `Display` does, and one routine compare
//! structurally the way the VM's `PartialEq` does — no per-type generated
//! code.
//!
//! # The collector
//!
//! Generational, precise, and non-moving in the old generation. New objects
//! are bump-allocated in a contiguous **nursery**; a minor collection
//! evacuates the live ones into the old generation (updating every reference,
//! which is why precision is not optional) and resets the bump pointer. Old
//! objects never move — each is a separate allocation, swept by an occasional
//! mark-and-sweep when the old generation has grown past a threshold.
//!
//! Roots are found **precisely** from Cranelift's user stack maps: the code
//! generator declares every reference-typed local as needing a stack map, so
//! at each safepoint (a call) the live references sit in stack slots whose
//! offsets from that frame's stack pointer are recorded. At collection time
//! the runtime walks the frame-pointer chain; a frame whose return address is
//! a registered safepoint has its recorded slots visited, and every other
//! frame — the runtime's own Rust frames included — is skipped, because
//! nothing in it can hold a Kite reference the maps do not already cover.
//! Cranelift spills stack-map values before each safepoint and reloads them
//! after, which is exactly what allows the nursery to move objects.
//!
//! The **write barrier** covers the one in-place heap mutation the language
//! has: a `var` field assignment. Everything else — slice writes, map writes,
//! pushes — is copy-on-write and allocates a fresh object, so the barrier
//! lives in `kite_rt_set_field` and nowhere else. An old object that has a
//! reference stored into it joins the remembered set, and the remembered set
//! is scanned as roots by the next minor collection.
//!
//! # Semantics
//!
//! The bytecode VM in `kite-vm` is the specification for everything
//! observable here: float formatting, character counting, map ordering, the
//! virtual clock, the trap messages. Where a function below looks like a
//! transcription of the VM's, that is because it is one, deliberately — a
//! difference in any of them is a differential-test failure.
//!
//! Single-threaded by design, like the VM and the Wasm host: the runtime's
//! state is process-global and one program runs at a time. The JIT test
//! harness serialises runs through [`run_lock`].

use std::alloc::{alloc as sys_alloc, dealloc as sys_dealloc, Layout};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

// ---------------------------------------------------------------------------
// Value kinds and object kinds
// ---------------------------------------------------------------------------

/// How a single 8-byte slot is to be read. This is what shape tables store,
/// and what the code generator passes for values whose type it knows
/// statically. `REF` is the only kind the collector acts on.
pub mod kind {
    pub const UNIT: u8 = 0;
    pub const BOOL: u8 = 1;
    pub const INT: u8 = 2;
    pub const FLOAT: u8 = 3;
    pub const REF: u8 = 4;
}

/// What a heap object is. Stored in the low byte of the first header word.
mod obj {
    pub const STR: u8 = 1;
    pub const STRUCT: u8 = 2;
    pub const ENUM: u8 = 3;
    pub const TUPLE: u8 = 4;
    pub const SLICE: u8 = 5;
    pub const MAP: u8 = 6;
    pub const PAIR: u8 = 7;
    pub const ERR: u8 = 8;
    pub const CLOSURE: u8 = 9;
    pub const BOX: u8 = 10;
    /// A nursery object that has been evacuated; the second header word is
    /// where it went. Only ever seen mid-collection.
    pub const FORWARDED: u8 = 0xFF;
}

const MARK_BIT: u64 = 1 << 8;
const REMEMBERED_BIT: u64 = 1 << 9;

/// Header: two words. Word 0 is `kind | gc bits | aux << 32`; word 1 is
/// kind-specific (a length, a variant, a capture count). Payload slots follow.
const HEADER: usize = 16;

#[inline]
fn word0(kind: u8, aux: u32) -> u64 {
    kind as u64 | ((aux as u64) << 32)
}

#[inline]
unsafe fn obj_kind(p: *const u8) -> u8 {
    (*(p as *const u64)) as u8
}

#[inline]
unsafe fn obj_aux(p: *const u8) -> u32 {
    ((*(p as *const u64)) >> 32) as u32
}

#[inline]
unsafe fn obj_word1(p: *const u8) -> u64 {
    *(p as *const u64).add(1)
}

#[inline]
unsafe fn slot(p: *const u8, i: usize) -> *mut u64 {
    (p as *mut u64).add(2 + i)
}

fn round8(n: usize) -> usize {
    (n + 7) & !7
}

// ---------------------------------------------------------------------------
// The staging window
// ---------------------------------------------------------------------------

/// Where compiled code puts the operands of a variadic construction — a
/// struct's fields, a slice's elements, a closure's captures — before calling
/// the runtime to allocate. The same idea as the bytecode VM's consecutive
/// argument window, and it exists for the same reason a window does there:
/// the C ABI has no variadic call the code generator could use safely.
///
/// The allocating call knows the count and the shape, so the collector can
/// treat the staged reference slots as roots if the allocation itself has to
/// collect.
pub const STAGE_WORDS: usize = 4096;

#[no_mangle]
pub static mut KITE_RT_STAGE: [u64; STAGE_WORDS] = [0; STAGE_WORDS];

#[inline]
fn stage_slot(i: usize) -> *mut u64 {
    unsafe { (&raw mut KITE_RT_STAGE).cast::<u64>().add(i) }
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

/// One live task, mirroring the VM's `Scheduled` field for field.
struct Scheduled {
    /// The resume closure, as a heap reference. A root: the scheduler may be
    /// the only thing keeping a suspended task's frame alive.
    poll: u64,
    wake_at: Option<i64>,
    parked: bool,
    waiting_on_host: bool,
}

#[derive(Default)]
struct Shapes {
    /// Per struct id: one kind byte per field.
    structs: Vec<Vec<u8>>,
    /// Per enum id, per variant: one kind byte per payload field.
    enums: Vec<Vec<Vec<u8>>>,
    /// Per tuple shape id: one kind byte per element.
    tuples: Vec<Vec<u8>>,
    /// Per lifted function id: the thunk's address and the capture kinds.
    closures: Vec<(usize, Vec<u8>)>,
    /// Function names, for the unreachable trap to point at.
    fn_names: Vec<String>,
    /// Declared host function names, for the no-host trap to name.
    externs: Vec<String>,
    /// Per dispatch table: rows of `(type tag, method addresses)`, sorted.
    vtables: Vec<Vec<(u32, Vec<usize>)>>,
}

struct Rt {
    // ---- the heap ------------------------------------------------------
    nursery: *mut u8,
    nursery_size: usize,
    nursery_top: usize,
    /// Every old-generation object, one allocation each, with the size it was
    /// allocated at. The size is recorded rather than recomputed because a
    /// header can honestly understate it — a map literal with repeated keys
    /// allocates room for every written pair and then records fewer — and
    /// freeing with a smaller layout than was allocated is undefined
    /// behaviour. Non-moving: an old object's address is stable for its whole
    /// life, which is what lets string constants be handed out as bare
    /// pointers.
    old: Vec<(*mut u8, usize)>,
    old_bytes: usize,
    /// Old-generation size that triggers a major collection.
    threshold: usize,
    /// Old objects a reference has been stored into since the last minor
    /// collection. Scanned as roots so the nursery never needs a full
    /// old-generation scan.
    remembered: Vec<*mut u8>,
    /// Slots inside the runtime's own Rust frames that hold references across
    /// a possible collection. Registered explicitly, because Rust frames have
    /// no stack maps.
    extra_roots: Vec<*mut u64>,
    /// `(safepoint pc, slot offsets from that frame's sp)`, sorted by pc.
    stack_maps: Vec<(usize, Vec<u32>)>,

    // ---- the program ---------------------------------------------------
    shapes: Shapes,
    /// String constants, as immortal old-generation objects.
    strings: Vec<u64>,

    // ---- the scheduler -------------------------------------------------
    tasks: Vec<Scheduled>,
    clock: i64,
    wake_request: Option<i64>,
    park_request: bool,
    host_wait_request: bool,

    // ---- output --------------------------------------------------------
    capture: Option<Vec<u8>>,
}

static mut RT: *mut Rt = std::ptr::null_mut();
static NURSERY_OVERRIDE: Mutex<Option<usize>> = Mutex::new(None);
static CAPTURE_NEXT: Mutex<bool> = Mutex::new(false);

/// The runtime is process-global, so two JIT-compiled programs must not run at
/// once. The test harness holds this around each run.
pub fn run_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        // A trap in an earlier run exits the process, so a poisoned lock can
        // only mean a test panicked mid-run; the state is reset on startup.
        Err(p) => p.into_inner(),
    }
}

/// Shrink the nursery for subsequent runs, so a test can force collections
/// without allocating gigabytes. Sticky rather than one-shot: tests in one
/// binary run concurrently, and a one-shot override could be consumed by a
/// neighbour's run before its setter's own.
pub fn set_nursery_bytes(n: usize) {
    *NURSERY_OVERRIDE.lock().unwrap() = Some(n);
}

/// Capture the next run's output instead of writing it to stdout.
pub fn begin_capture() {
    *CAPTURE_NEXT.lock().unwrap() = true;
}

/// The output the finished run produced under [`begin_capture`].
pub fn take_capture() -> Vec<u8> {
    let rt = rt();
    rt.capture.take().unwrap_or_default()
}

#[allow(static_mut_refs)]
fn rt() -> &'static mut Rt {
    unsafe {
        assert!(!RT.is_null(), "kite_rt_startup was never called");
        &mut *RT
    }
}

const DEFAULT_NURSERY: usize = 1 << 20;
const DEFAULT_THRESHOLD: usize = 8 << 20;

/// Initialise — or fully reset — the runtime. The wrapper the code generator
/// emits calls this before anything else, which is also what makes a second
/// JIT run in one process start clean.
#[no_mangle]
pub extern "C" fn kite_rt_startup() {
    // The highest stack address the Kite program owns. The compiled entry
    // calls this, so its own frame — and every frame the program makes below
    // it — is at an address at or under this local, the stack growing down.
    // The root walk stops here, which is what keeps it out of the frames of
    // whatever called the program: a test harness, `main`, a threading
    // runtime. None of those can hold a Kite reference, and walking into them
    // is how a garbage word gets read as an object header.
    let floor = 0usize;
    unsafe {
        STACK_TOP = (&floor as *const usize as usize) + STACK_TOP_MARGIN;
    }
    unsafe {
        if !RT.is_null() {
            let old = Box::from_raw(RT);
            RT = std::ptr::null_mut();
            release(old);
        }
        let nursery_size = (*NURSERY_OVERRIDE.lock().unwrap())
            .or_else(|| {
                std::env::var("KITE_NURSERY_BYTES")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(DEFAULT_NURSERY)
            .max(4096);
        let nursery = sys_alloc(Layout::from_size_align(nursery_size, 16).unwrap());
        assert!(!nursery.is_null(), "cannot allocate the nursery");
        let capture = if std::mem::take(&mut *CAPTURE_NEXT.lock().unwrap()) {
            Some(Vec::new())
        } else {
            None
        };
        RT = Box::into_raw(Box::new(Rt {
            nursery,
            nursery_size,
            nursery_top: 0,
            old: Vec::new(),
            old_bytes: 0,
            threshold: DEFAULT_THRESHOLD,
            remembered: Vec::new(),
            extra_roots: Vec::new(),
            stack_maps: Vec::new(),
            shapes: Shapes::default(),
            strings: Vec::new(),
            tasks: Vec::new(),
            clock: 0,
            wake_request: None,
            park_request: false,
            host_wait_request: false,
            capture,
        }));
    }
}

/// Free a previous run's heap. The capture buffer is intentionally not part of
/// this: `take_capture` reads it after the run.
fn release(rt: Box<Rt>) {
    unsafe {
        for &(p, size) in &rt.old {
            sys_dealloc(p, Layout::from_size_align(size, 16).unwrap());
        }
        sys_dealloc(
            rt.nursery,
            Layout::from_size_align(rt.nursery_size, 16).unwrap(),
        );
    }
}

// ---------------------------------------------------------------------------
// Traps
// ---------------------------------------------------------------------------

/// End the program with a message. Traps are not catchable — Kite has no
/// `recover` — and this runtime is the bottom of the stack, so the message
/// goes to stderr and the process exits. The wording matches the VM's
/// `Display for Trap` so `kitec run` and `kitec run --native` say the same
/// thing about the same bug.
fn trap(message: &str) -> ! {
    // Whatever the program printed so far should still reach the terminal.
    if let Some(buf) = rt().capture.take() {
        let _ = std::io::stdout().lock().write_all(&buf);
    }
    eprintln!("\nerror: {}", message);
    eprintln!("note: traps are not catchable; Kite has no `recover`");
    std::process::exit(1);
}

/// Trap codes the code generator raises for conditions it checks inline.
/// Everything the runtime detects itself calls [`trap`] directly.
#[no_mangle]
pub extern "C" fn kite_rt_trap(code: i64, a: i64, b: i64) {
    let op = |c: i64| match c {
        2 => "+",
        3 => "-",
        4 => "*",
        5 => "/",
        6 => "%",
        7 => "<<",
        _ => ">>",
    };
    match code {
        1 => trap("divide by zero"),
        2..=8 => trap(&format!("integer overflow in `{}`", op(code))),
        _ => {
            let name = rt()
                .shapes
                .fn_names
                .get(a as usize)
                .map(|s| s.as_str())
                .unwrap_or("?");
            trap(&format!(
                "reached unreachable code in `{}` at pc {}",
                name, b
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------
//
// The code generator emits one initialisation function per program that makes
// these calls before `main` runs. Registration is code rather than a
// serialised format because the addresses in it — thunks, vtable methods —
// are exactly what a linker or the JIT already knows how to relocate.

unsafe fn bytes_arg(ptr: *const u8, len: u64) -> Vec<u8> {
    std::slice::from_raw_parts(ptr, len as usize).to_vec()
}

fn ensure_len<T: Default>(v: &mut Vec<T>, n: usize) {
    if v.len() < n {
        v.resize_with(n, T::default);
    }
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_string(idx: u64, ptr: *const u8, len: u64) {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    // Straight into the old generation: a constant lives as long as the
    // program, and an immortal object in the nursery would be copied out on
    // the first collection anyway.
    let p = old_alloc(HEADER + round8(bytes.len()));
    unsafe {
        *(p as *mut u64) = word0(obj::STR, bytes.len() as u32);
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(HEADER), bytes.len());
    }
    let rt = rt();
    ensure_len(&mut rt.strings, idx as usize + 1);
    rt.strings[idx as usize] = p as u64;
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_struct_shape(id: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.structs, id as usize + 1);
    rt.shapes.structs[id as usize] = unsafe { bytes_arg(ptr, len) };
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_enum_shape(id: u64, variant: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.enums, id as usize + 1);
    let variants = &mut rt.shapes.enums[id as usize];
    ensure_len(variants, variant as usize + 1);
    variants[variant as usize] = unsafe { bytes_arg(ptr, len) };
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_tuple_shape(id: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.tuples, id as usize + 1);
    rt.shapes.tuples[id as usize] = unsafe { bytes_arg(ptr, len) };
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_closure(func: u64, thunk: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.closures, func as usize + 1);
    rt.shapes.closures[func as usize] = (thunk as usize, unsafe { bytes_arg(ptr, len) });
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_fn_name(idx: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.fn_names, idx as usize + 1);
    rt.shapes.fn_names[idx as usize] =
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
            .into_owned();
}

/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_extern(idx: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.externs, idx as usize + 1);
    rt.shapes.externs[idx as usize] =
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
            .into_owned();
}

#[no_mangle]
pub extern "C" fn kite_rt_register_vtable_method(
    table: u64,
    tag: u64,
    method: u64,
    addr: u64,
) {
    let rt = rt();
    ensure_len(&mut rt.shapes.vtables, table as usize + 1);
    let rows = &mut rt.shapes.vtables[table as usize];
    let tag = tag as u32;
    let row = match rows.binary_search_by_key(&tag, |(t, _)| *t) {
        Ok(i) => i,
        Err(i) => {
            rows.insert(i, (tag, Vec::new()));
            i
        }
    };
    let methods = &mut rows[row].1;
    ensure_len(methods, method as usize + 1);
    methods[method as usize] = addr as usize;
}

/// The stack-map table, as words: `[fn count]`, then per function
/// `[address][map count]` and per map `[return-address offset][entry count]`
/// followed by the entry offsets. The addresses are relocated function
/// addresses, which is why the table is data the code generator emits rather
/// than something serialised on the side.
/// # Safety
///
/// `ptr` must point at a well-formed table in the format above, which is
/// what the code generator emits and nothing else produces.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_stack_maps(ptr: *const u64) {
    let rt = rt();
    unsafe {
        let mut at = ptr;
        let mut next = || {
            let v = *at;
            at = at.add(1);
            v
        };
        let fn_count = next();
        for _ in 0..fn_count {
            let base = next() as usize;
            let maps = next();
            for _ in 0..maps {
                let ret_off = next() as usize;
                let entries = next();
                let offsets: Vec<u32> = (0..entries).map(|_| next() as u32).collect();
                rt.stack_maps.push((base + ret_off, offsets));
            }
        }
    }
    rt.stack_maps.sort_by_key(|(pc, _)| *pc);
}

// ---------------------------------------------------------------------------
// Allocation and collection
// ---------------------------------------------------------------------------

#[inline]
fn in_nursery(p: u64) -> bool {
    let rt = rt();
    let base = rt.nursery as u64;
    p >= base && p < base + rt.nursery_size as u64
}

fn old_alloc(size: usize) -> *mut u8 {
    let size = round8(size).max(HEADER);
    let p = unsafe { sys_alloc(Layout::from_size_align(size, 16).unwrap()) };
    assert!(!p.is_null(), "out of memory");
    let rt = rt();
    rt.old.push((p, size));
    rt.old_bytes += size;
    p
}

/// Bump-allocate in the nursery, collecting when it is full. An object larger
/// than the nursery itself goes straight to the old generation.
fn alloc(size: usize) -> *mut u8 {
    let size = round8(size).max(HEADER);
    let rt_ = rt();
    if rt_.nursery_top + size <= rt_.nursery_size {
        let p = unsafe { rt_.nursery.add(rt_.nursery_top) };
        rt_.nursery_top += size;
        return p;
    }
    collect_minor();
    let rt = rt();
    if rt.nursery_top + size <= rt.nursery_size {
        let p = unsafe { rt.nursery.add(rt.nursery_top) };
        rt.nursery_top += size;
        return p;
    }
    // Too big for the nursery at all. A fresh old object is about to have
    // nursery references stored into it, which is the edge the write barrier
    // exists for — so it joins the remembered set at birth.
    let p = old_alloc(size);
    unsafe { *(p as *mut u64) = REMEMBERED_BIT };
    rt.remembered.push(p);
    p
}

/// Push a slot the collector must treat as a root — a Rust local in a runtime
/// function that holds a reference across an allocation. Popped in LIFO order
/// by [`unroot`].
fn root(slot: *mut u64) {
    rt().extra_roots.push(slot);
}

fn unroot(n: usize) {
    let rt = rt();
    let len = rt.extra_roots.len() - n;
    rt.extra_roots.truncate(len);
}

/// The size in bytes of an object, recomputed from its header — which is why
/// the header carries a count even where the language already knows it.
unsafe fn object_size_of(p: *const u8, shapes: &Shapes) -> usize {
    let kind = obj_kind(p);
    let aux = obj_aux(p) as usize;
    let payload = match kind {
        obj::STR => round8(aux),
        obj::STRUCT => 8 * shapes.structs[aux].len(),
        obj::ENUM => 8 * ((obj_word1(p) >> 32) as usize),
        obj::TUPLE => 8 * shapes.tuples[aux].len(),
        obj::SLICE => 8 * obj_word1(p) as usize,
        obj::MAP => 16 * obj_word1(p) as usize,
        obj::PAIR => 16,
        obj::ERR => 32,
        obj::CLOSURE => 8 + 8 * obj_word1(p) as usize,
        obj::BOX => 8,
        other => unreachable!("sizing an object of kind {}", other),
    };
    HEADER + payload
}

/// Visit every payload slot that holds a reference. The shapes say which; the
/// collector, the renderer and structural equality all go through the same
/// answer, which is what keeps the three from disagreeing about what a value
/// contains.
///
/// The shape row is copied out before the callback runs: the callback is the
/// collector, and the collector mutates the runtime state the shapes live in.
unsafe fn for_each_ref_slot(p: *mut u8, f: &mut dyn FnMut(*mut u64)) {
    let kind = obj_kind(p);
    let aux = obj_aux(p) as usize;
    let shaped = |kinds: Vec<u8>, base: usize, f: &mut dyn FnMut(*mut u64)| {
        for (i, k) in kinds.iter().enumerate() {
            if *k == kind::REF {
                f(slot(p, base + i));
            }
        }
    };
    match kind {
        obj::STR => {}
        obj::STRUCT => shaped(rt().shapes.structs[aux].clone(), 0, f),
        obj::ENUM => {
            let variant = (obj_word1(p) & 0xFFFF_FFFF) as usize;
            shaped(rt().shapes.enums[aux][variant].clone(), 0, f);
        }
        obj::TUPLE => shaped(rt().shapes.tuples[aux].clone(), 0, f),
        obj::SLICE => {
            if aux as u8 == kind::REF {
                for i in 0..obj_word1(p) as usize {
                    f(slot(p, i));
                }
            }
        }
        obj::MAP => {
            let key_ref = (aux & 0xFF) as u8 == kind::REF;
            let val_ref = ((aux >> 8) & 0xFF) as u8 == kind::REF;
            for i in 0..obj_word1(p) as usize {
                if key_ref {
                    f(slot(p, 2 * i));
                }
                if val_ref {
                    f(slot(p, 2 * i + 1));
                }
            }
        }
        obj::PAIR => {
            if (aux & 0xFF) as u8 == kind::REF {
                f(slot(p, 0));
            }
            f(slot(p, 1));
        }
        // Message, carried value and cause are references; the tag at slot 2
        // is a plain integer and must not be handed to the collector.
        obj::ERR => {
            f(slot(p, 0));
            f(slot(p, 1));
            f(slot(p, 3));
        }
        // Slot 0 is the thunk's code address, which is not a heap object.
        obj::CLOSURE => shaped(rt().shapes.closures[aux].1.clone(), 1, f),
        obj::BOX => {
            if aux as u8 == kind::REF {
                f(slot(p, 0));
            }
        }
        other => unreachable!("tracing an object of kind {}", other),
    }
}

/// The highest stack address the root walk may reach, set by
/// `kite_rt_startup`. Zero before a program has started, which makes the walk
/// find nothing rather than guess.
static mut STACK_TOP: usize = 0;

/// How far above `kite_rt_startup`'s own frame the program's entry frame may
/// sit. The compiled wrapper called into here, so its frame is immediately
/// above; a page is far more room than that needs and still stops well short
/// of a harness's frames.
const STACK_TOP_MARGIN: usize = 4096;

/// The current frame pointer, for starting a stack walk.
#[inline(always)]
fn current_fp() -> usize {
    let fp: usize;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("mov {}, x29", out(reg) fp)
    };
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!("mov {}, rbp", out(reg) fp)
    };
    fp
}

/// Walk the frame-pointer chain and visit every stack-map slot.
///
/// A frame record holds the caller's frame pointer and the return address
/// into the caller; on both x86-64 and AArch64 the caller's stack pointer at
/// the moment of the call is the record's address plus 16, which is exactly
/// what the stack-map offsets are relative to. Frames whose return address is
/// not a registered safepoint — this crate's own frames, and every compiled
/// frame with nothing live — are skipped rather than scanned.
///
/// **This requires every frame between here and the program's entry to keep a
/// frame pointer**, and that is not a default: Cranelift is told to
/// (`preserve_frame_pointers`), and the workspace's `.cargo/config.toml`
/// passes `-C force-frame-pointers=yes` so this crate's own frames do too. On
/// Apple targets the ABI demands it anyway, which is exactly why the first
/// version of this walk passed on macOS and corrupted the heap on x86-64
/// Linux and Windows: `rbp` there was an ordinary callee-saved register
/// holding whatever the optimiser put in it, and the walk read it as a frame.
///
/// The walk is bounded at both ends rather than trusted. It stops at
/// `STACK_TOP` — the program's own entry — because nothing above that can
/// hold a Kite reference; it requires the chain to climb, so a repeated or
/// descending pointer ends it; and it ignores a frame whose recorded slots
/// would fall outside the stack between here and there. A bad frame therefore
/// costs a missed root at worst, and a missed root is a use-after-free, so it
/// is *also* checked: a walk that reaches `STACK_TOP` without seeing the
/// entry's own frame would mean the chain broke, and the debug assertion
/// below says so on the build where anyone is looking.
fn stack_root_slots() -> Vec<*mut u64> {
    let top = unsafe { STACK_TOP };
    let maps = &rt().stack_maps;
    let mut slots = Vec::new();
    let mut fp = current_fp();
    let floor = fp;
    let mut hops = 0;
    // Nothing has started, so nothing is rooted. Better to say that than to
    // walk an unbounded chain.
    if top == 0 {
        return slots;
    }
    while fp != 0 && fp & 7 == 0 && fp < top && hops < 1_000_000 {
        hops += 1;
        let ret = unsafe { *((fp + 8) as *const usize) };
        let caller_fp = unsafe { *(fp as *const usize) };
        if let Ok(i) = maps.binary_search_by_key(&ret, |(pc, _)| *pc) {
            let sp = fp + 16;
            for off in &maps[i].1 {
                let slot = sp + *off as usize;
                // A slot outside the live stack is not a slot. This cannot
                // happen with a well-formed chain, and silently skipping it
                // is what keeps a malformed one from being fatal.
                if slot >= floor && slot < top {
                    slots.push(slot as *mut u64);
                }
            }
        }
        // The chain must climb; a repeated or descending pointer means the
        // walk has left well-formed frames behind.
        if caller_fp <= fp {
            break;
        }
        fp = caller_fp;
    }
    slots
}

/// Evacuate one nursery object to the old generation, leaving a forwarding
/// pointer, and return its new address.
unsafe fn evacuate(p: u64, queue: &mut Vec<*mut u8>) -> u64 {
    let p = p as *mut u8;
    if obj_kind(p) == obj::FORWARDED {
        return obj_word1(p);
    }
    let size = object_size_of(p, &rt().shapes);
    let new = old_alloc(size);
    std::ptr::copy_nonoverlapping(p, new, size);
    *(p as *mut u64) = word0(obj::FORWARDED, 0);
    *(p as *mut u64).add(1) = new as u64;
    queue.push(new);
    new as u64
}

fn evac_slot(s: *mut u64, queue: &mut Vec<*mut u8>) {
    let v = unsafe { *s };
    if v != 0 && in_nursery(v) {
        unsafe { *s = evacuate(v, queue) };
    }
}

/// A minor collection: evacuate everything live out of the nursery, then
/// reset the bump pointer. Every live nursery object is promoted on its first
/// collection — the simplest generational policy, and an honest one: objects
/// that die young never get copied at all, which is the bet a nursery makes.
fn collect_minor() {
    GC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut queue: Vec<*mut u8> = Vec::new();

    // Roots: compiled frames via the stack maps, the runtime's own registered
    // slots, and the scheduler's suspended tasks.
    for s in stack_root_slots() {
        evac_slot(s, &mut queue);
    }
    let extra = rt().extra_roots.clone();
    for s in extra {
        evac_slot(s, &mut queue);
    }
    for i in 0..rt().tasks.len() {
        let s = &mut rt().tasks[i].poll as *mut u64;
        evac_slot(s, &mut queue);
    }
    // The remembered set: old objects that had a reference stored into them
    // may be the only path to a nursery object.
    let remembered = std::mem::take(&mut rt().remembered);
    for p in remembered {
        unsafe {
            *(p as *mut u64) &= !REMEMBERED_BIT;
            for_each_ref_slot(p, &mut |s| evac_slot(s, &mut queue));
        }
    }
    while let Some(p) = queue.pop() {
        unsafe { for_each_ref_slot(p, &mut |s| evac_slot(s, &mut queue)) };
    }
    rt().nursery_top = 0;

    if rt().old_bytes > rt().threshold {
        collect_major();
    }
}

/// A major collection: mark from the same roots, sweep the old generation.
/// Runs only immediately after a minor collection, when the nursery is empty,
/// so everything reachable is old and nothing moves — the old generation is
/// non-moving by design, which is what spares the runtime a read barrier.
fn collect_major() {
    let mut stack: Vec<*mut u8> = Vec::new();
    let mark_slot = |s: *mut u64, stack: &mut Vec<*mut u8>| {
        let v = unsafe { *s };
        if v != 0 {
            let p = v as *mut u8;
            unsafe {
                if *(p as *const u64) & MARK_BIT == 0 {
                    *(p as *mut u64) |= MARK_BIT;
                    stack.push(p);
                }
            }
        }
    };

    for s in stack_root_slots() {
        mark_slot(s, &mut stack);
    }
    let extra = rt().extra_roots.clone();
    for s in extra {
        mark_slot(s, &mut stack);
    }
    for i in 0..rt().tasks.len() {
        let s = &mut rt().tasks[i].poll as *mut u64;
        mark_slot(s, &mut stack);
    }
    // String constants are reachable from compiled code by index, which no
    // heap trace can see; the table itself is a root.
    for i in 0..rt().strings.len() {
        let s = &mut rt().strings[i] as *mut u64;
        mark_slot(s, &mut stack);
    }
    while let Some(p) = stack.pop() {
        unsafe { for_each_ref_slot(p, &mut |s| mark_slot(s, &mut stack)) };
    }

    let rt_ = rt();
    let mut live: Vec<(*mut u8, usize)> = Vec::with_capacity(rt_.old.len());
    let mut live_bytes = 0;
    for &(p, size) in &rt_.old {
        unsafe {
            if *(p as *const u64) & MARK_BIT != 0 {
                *(p as *mut u64) &= !MARK_BIT;
                live.push((p, size));
                live_bytes += size;
            } else {
                sys_dealloc(p, Layout::from_size_align(size, 16).unwrap());
            }
        }
    }
    rt_.old = live;
    rt_.old_bytes = live_bytes;
    rt_.threshold = (2 * live_bytes).max(DEFAULT_THRESHOLD);
}

/// How many minor collections have run — for the collector's own tests, which
/// must fail if the GC they mean to exercise never actually ran.
#[no_mangle]
pub extern "C" fn kite_rt_gc_count() -> i64 {
    GC_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`kite_rt_gc_count`], for Rust callers. Cumulative across runs.
pub fn gc_runs() -> i64 {
    kite_rt_gc_count()
}

static GC_COUNT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// Root the staged reference slots for an allocation that reads `argc` staged
/// words whose kinds are given per-slot by `kinds`.
fn root_stage(argc: usize, ref_at: impl Fn(usize) -> bool) -> usize {
    let mut n = 0;
    for i in 0..argc {
        if ref_at(i) {
            root(stage_slot(i));
            n += 1;
        }
    }
    n
}

unsafe fn fill_from_stage(p: *mut u8, argc: usize) {
    for i in 0..argc {
        *slot(p, i) = *stage_slot(i);
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_struct_new(struct_id: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let kinds = rt().shapes.structs[struct_id as usize].clone();
    let n = root_stage(argc, |i| kinds[i] == kind::REF);
    let p = alloc(HEADER + 8 * argc);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::STRUCT, struct_id as u32);
        *(p as *mut u64).add(1) = 0;
        fill_from_stage(p, argc);
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_enum_new(enum_id: u64, variant: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let kinds = rt().shapes.enums[enum_id as usize][variant as usize].clone();
    let n = root_stage(argc, |i| kinds[i] == kind::REF);
    let p = alloc(HEADER + 8 * argc);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::ENUM, enum_id as u32);
        *(p as *mut u64).add(1) = variant | ((argc as u64) << 32);
        fill_from_stage(p, argc);
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_tuple_new(shape: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let kinds = rt().shapes.tuples[shape as usize].clone();
    let n = root_stage(argc, |i| kinds[i] == kind::REF);
    let p = alloc(HEADER + 8 * argc);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::TUPLE, shape as u32);
        *(p as *mut u64).add(1) = 0;
        fill_from_stage(p, argc);
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_slice_new(elem_kind: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let n = root_stage(argc, |_| elem_kind as u8 == kind::REF);
    let p = alloc(HEADER + 8 * argc);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::SLICE, elem_kind as u32);
        *(p as *mut u64).add(1) = argc as u64;
        fill_from_stage(p, argc);
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_closure_new(func: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let (thunk, kinds) = rt().shapes.closures[func as usize].clone();
    let n = root_stage(argc, |i| kinds[i] == kind::REF);
    let p = alloc(HEADER + 8 + 8 * argc);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::CLOSURE, func as u32);
        *(p as *mut u64).add(1) = argc as u64;
        *slot(p, 0) = thunk as u64;
        for i in 0..argc {
            *slot(p, 1 + i) = *stage_slot(i);
        }
    }
    p as u64
}

/// Build a map from `argc` staged words — alternating keys and values. A
/// repeated key keeps its original position, exactly as the VM does, so
/// insertion order stays well defined.
#[no_mangle]
pub extern "C" fn kite_rt_map_new(key_kind: u64, val_kind: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let pairs = argc / 2;
    let n = root_stage(argc, |i| {
        (if i % 2 == 0 { key_kind } else { val_kind }) as u8 == kind::REF
    });
    // Allocate room for every staged pair; duplicates leave the tail unused,
    // and the header's length is what says how much of it is real.
    let p = alloc(HEADER + 16 * pairs);
    unroot(n);
    let aux = (key_kind as u32 & 0xFF) | ((val_kind as u32 & 0xFF) << 8);
    unsafe {
        *(p as *mut u64) = word0(obj::MAP, aux);
        *(p as *mut u64).add(1) = 0;
        let mut len = 0usize;
        for e in 0..pairs {
            let k = *stage_slot(2 * e);
            let v = *stage_slot(2 * e + 1);
            let mut replaced = false;
            for i in 0..len {
                if value_eq(*slot(p, 2 * i), k, key_kind as u8) {
                    *slot(p, 2 * i + 1) = v;
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                *slot(p, 2 * len) = k;
                *slot(p, 2 * len + 1) = v;
                len += 1;
            }
        }
        *(p as *mut u64).add(1) = len as u64;
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_box_new(value: u64, value_kind: u64) -> u64 {
    let mut value = value;
    let is_ref = value_kind as u8 == kind::REF;
    if is_ref {
        root(&mut value);
    }
    let p = alloc(HEADER + 8);
    if is_ref {
        unroot(1);
    }
    unsafe {
        *(p as *mut u64) = word0(obj::BOX, value_kind as u32);
        *(p as *mut u64).add(1) = 0;
        *slot(p, 0) = value;
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_pair_new(value: u64, value_kind: u64, error: u64) -> u64 {
    let mut value = value;
    let mut error = error;
    let is_ref = value_kind as u8 == kind::REF;
    let mut n = 1;
    root(&mut error);
    if is_ref {
        root(&mut value);
        n += 1;
    }
    let p = alloc(HEADER + 16);
    unroot(n);
    unsafe {
        *(p as *mut u64) = word0(obj::PAIR, value_kind as u32);
        *(p as *mut u64).add(1) = 0;
        *slot(p, 0) = value;
        *slot(p, 1) = error;
    }
    p as u64
}

/// An error: what it says, the value it was rendered from, that value's type
/// tag, and the error it wrapped.
///
/// The last three are how `cause`, `T.is` and `T.as` are answered. A plain
/// `errors.new` passes zero for all of them, and no type's tag is zero, so a
/// downcast against one is false without a test in front of it.
#[no_mangle]
pub extern "C" fn kite_rt_error_new(message: u64, value: u64, tag: i64, cause: u64) -> u64 {
    let mut message = message;
    let mut value = value;
    let mut cause = cause;
    root(&mut message);
    root(&mut value);
    root(&mut cause);
    let p = alloc(HEADER + 32);
    unroot(3);
    unsafe {
        *(p as *mut u64) = word0(obj::ERR, 0);
        *(p as *mut u64).add(1) = 0;
        *slot(p, 0) = message;
        *slot(p, 1) = value;
        *slot(p, 2) = tag as u64;
        *slot(p, 3) = cause;
    }
    p as u64
}

/// The error this one wrapped, or nil. Nil for a nil error too, so this needs
/// no nil test in front of it — the same courtesy `message` extends.
///
/// No boxing: `cause` answers an `error`, which is already the nil-able type.
#[no_mangle]
pub extern "C" fn kite_rt_error_cause(e: u64) -> u64 {
    if e == 0 {
        return 0;
    }
    unsafe { *slot(e as *const u8, 3) }
}

/// The type tag of the carried value, or zero.
#[no_mangle]
pub extern "C" fn kite_rt_error_tag(e: u64) -> i64 {
    if e == 0 {
        return 0;
    }
    unsafe { *slot(e as *const u8, 2) as i64 }
}

/// The carried value when the error's tag is `tag`, and nil when it is not. A
/// nil error answers nil, since no type's tag is zero.
///
/// `wrap_kind` boxes the result, because an optional is a box here.
#[no_mangle]
pub extern "C" fn kite_rt_error_as(e: u64, tag: i64, wrap_kind: i64) -> u64 {
    if e == 0 {
        return 0;
    }
    let value = unsafe {
        if *slot(e as *const u8, 2) as i64 == tag {
            *slot(e as *const u8, 1)
        } else {
            return 0;
        }
    };
    if wrap_kind < 0 {
        return value;
    }
    kite_rt_box_new(value, wrap_kind as u64)
}

/// The message of an error — or `""` for `nil`, which is what the VM answers
/// and what lets `err.message()` be written without a nil check.
#[no_mangle]
pub extern "C" fn kite_rt_error_message(err: u64) -> u64 {
    if err == 0 {
        return make_str(b"");
    }
    unsafe { *slot(err as *mut u8, 0) }
}

// ---------------------------------------------------------------------------
// Reads and writes
// ---------------------------------------------------------------------------

/// The one in-place heap mutation the language has, and therefore the one
/// place the write barrier lives.
#[no_mangle]
pub extern "C" fn kite_rt_set_field(base: u64, index: u64, value: u64, is_ref: u64) {
    let p = base as *mut u8;
    unsafe {
        *slot(p, index as usize) = value;
        if is_ref != 0 && !in_nursery(base) && *(p as *const u64) & REMEMBERED_BIT == 0 {
            *(p as *mut u64) |= REMEMBERED_BIT;
            rt().remembered.push(p);
        }
    }
}

unsafe fn slice_parts(s: u64) -> (*mut u8, usize) {
    let p = s as *mut u8;
    (p, obj_word1(p) as usize)
}

/// Bounds-checked; traps on failure, because an index bug is a program bug.
#[no_mangle]
pub extern "C" fn kite_rt_index_get(s: u64, index: i64) -> u64 {
    unsafe {
        let (p, len) = slice_parts(s);
        match usize::try_from(index).ok().filter(|u| *u < len) {
            Some(u) => *slot(p, u),
            None => trap(&format!(
                "index {} is out of range for a slice of length {}",
                index, len
            )),
        }
    }
}

/// Copy-on-write: the whole slice is copied and the copy mutated, and the
/// code generator rebinds the local — the same observable behaviour as the
/// VM's clone-if-shared, bought with a copy instead of a reference count.
#[no_mangle]
pub extern "C" fn kite_rt_set_index(s: u64, index: i64, value: u64) -> u64 {
    let mut s = s;
    let mut value = value;
    unsafe {
        let (_, len) = slice_parts(s);
        if usize::try_from(index).ok().filter(|u| *u < len).is_none() {
            trap(&format!(
                "index {} is out of range for a slice of length {}",
                index, len
            ));
        }
        root(&mut s);
        let elem_ref = obj_aux(s as *const u8) as u8 == kind::REF;
        if elem_ref {
            root(&mut value);
        }
        let p = alloc(HEADER + 8 * len);
        unroot(if elem_ref { 2 } else { 1 });
        std::ptr::copy_nonoverlapping(s as *const u8, p, HEADER + 8 * len);
        *slot(p, index as usize) = value;
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_slice_push(s: u64, value: u64) -> u64 {
    let mut s = s;
    let mut value = value;
    unsafe {
        let (_, len) = slice_parts(s);
        root(&mut s);
        let elem_ref = obj_aux(s as *const u8) as u8 == kind::REF;
        if elem_ref {
            root(&mut value);
        }
        let p = alloc(HEADER + 8 * (len + 1));
        unroot(if elem_ref { 2 } else { 1 });
        std::ptr::copy_nonoverlapping(s as *const u8, p, HEADER + 8 * len);
        *(p as *mut u64).add(1) = (len + 1) as u64;
        *slot(p, len) = value;
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_slice_len(s: u64) -> i64 {
    unsafe { obj_word1(s as *const u8) as i64 }
}

/// `xs[a..b]` — a fresh slice of the half-open window.
///
/// **Clamped rather than trapping**, which is the opposite of
/// [`kite_rt_index_get`] two functions up. An index names an element the
/// program believes is there; a range names a window, and a window wider than
/// the data is what a last page looks like. The bytecode VM defines this
/// answer and the other two backends reproduce it.
#[no_mangle]
pub extern "C" fn kite_rt_slice_range(s: u64, start: i64, end: i64) -> u64 {
    let mut s = s;
    unsafe {
        let (_, len) = slice_parts(s);
        let len = len as i64;
        let lo = start.clamp(0, len);
        // Clamped up to `lo`, so a backwards range is empty rather than a
        // negative length handed to the allocator.
        let hi = end.clamp(lo, len);
        let count = (hi - lo) as usize;

        root(&mut s);
        let p = alloc(HEADER + 8 * count);
        unroot(1);
        // The header carries the element kind, which the copy must preserve
        // for the collector to trace the new slice at all.
        *(p as *mut u64) = *(s as *const u64);
        *(p as *mut u64).add(1) = count as u64;
        std::ptr::copy_nonoverlapping(
            slot(s as *const u8, lo as usize) as *const u8,
            slot(p, 0) as *mut u8,
            8 * count,
        );
        p as u64
    }
}

/// `.get()` — an optional rather than a trap. `wrap_kind` is the element's
/// kind when the result must be boxed, or -1 when the element type is itself
/// optional and the stored reference already is the answer.
#[no_mangle]
pub extern "C" fn kite_rt_slice_get(s: u64, index: i64, wrap_kind: i64) -> u64 {
    unsafe {
        let (p, len) = slice_parts(s);
        match usize::try_from(index).ok().filter(|u| *u < len) {
            Some(u) => {
                let v = *slot(p, u);
                if wrap_kind < 0 {
                    v
                } else {
                    kite_rt_box_new(v, wrap_kind as u64)
                }
            }
            None => 0,
        }
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_map_len(m: u64) -> i64 {
    unsafe { obj_word1(m as *const u8) as i64 }
}

#[no_mangle]
pub extern "C" fn kite_rt_map_get(m: u64, key: u64, key_kind: u64, wrap_kind: i64) -> u64 {
    unsafe {
        let p = m as *mut u8;
        let len = obj_word1(p) as usize;
        for i in 0..len {
            if value_eq(*slot(p, 2 * i), key, key_kind as u8) {
                let v = *slot(p, 2 * i + 1);
                return if wrap_kind < 0 {
                    v
                } else {
                    kite_rt_box_new(v, wrap_kind as u64)
                };
            }
        }
        0
    }
}

/// Copy the map and write through the copy; an existing key keeps its
/// position, a new one is appended — the VM's rule, verbatim.
#[no_mangle]
pub extern "C" fn kite_rt_map_set(m: u64, key: u64, key_kind: u64, value: u64) -> u64 {
    let mut m = m;
    let mut key = key;
    let mut value = value;
    unsafe {
        let len = obj_word1(m as *const u8) as usize;
        let aux = obj_aux(m as *const u8);
        let key_ref = (aux & 0xFF) as u8 == kind::REF;
        let val_ref = ((aux >> 8) & 0xFF) as u8 == kind::REF;
        let mut n = 1;
        root(&mut m);
        if key_ref {
            root(&mut key);
            n += 1;
        }
        if val_ref {
            root(&mut value);
            n += 1;
        }
        let p = alloc(HEADER + 16 * (len + 1));
        unroot(n);
        std::ptr::copy_nonoverlapping(m as *const u8, p, HEADER + 16 * len);
        for i in 0..len {
            if value_eq(*slot(p, 2 * i), key, key_kind as u8) {
                *slot(p, 2 * i + 1) = value;
                return p as u64;
            }
        }
        *(p as *mut u64).add(1) = (len + 1) as u64;
        *slot(p, 2 * len) = key;
        *slot(p, 2 * len + 1) = value;
        p as u64
    }
}

/// A map's keys or values as a slice, in insertion order, so the two line up
/// element for element.
fn map_side(m: u64, values: bool) -> u64 {
    let mut m = m;
    unsafe {
        let len = obj_word1(m as *const u8) as usize;
        let aux = obj_aux(m as *const u8);
        let elem_kind = if values { (aux >> 8) & 0xFF } else { aux & 0xFF };
        root(&mut m);
        let p = alloc(HEADER + 8 * len);
        unroot(1);
        *(p as *mut u64) = word0(obj::SLICE, elem_kind);
        *(p as *mut u64).add(1) = len as u64;
        for i in 0..len {
            *slot(p, i) = *slot(m as *const u8, 2 * i + usize::from(values));
        }
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_map_keys(m: u64) -> u64 {
    map_side(m, false)
}

#[no_mangle]
pub extern "C" fn kite_rt_map_values(m: u64) -> u64 {
    map_side(m, true)
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

unsafe fn str_bytes<'a>(s: u64) -> &'a [u8] {
    let p = s as *const u8;
    std::slice::from_raw_parts(p.add(HEADER), obj_aux(p) as usize)
}

unsafe fn str_str<'a>(s: u64) -> &'a str {
    std::str::from_utf8_unchecked(str_bytes(s))
}

fn make_str(bytes: &[u8]) -> u64 {
    // The bytes must not point into the heap — every caller below builds them
    // in Rust-owned memory first, precisely so this allocation cannot move
    // its own input.
    let p = alloc(HEADER + round8(bytes.len()));
    unsafe {
        *(p as *mut u64) = word0(obj::STR, bytes.len() as u32);
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(HEADER), bytes.len());
    }
    p as u64
}

#[no_mangle]
pub extern "C" fn kite_rt_str_const(idx: u64) -> u64 {
    rt().strings[idx as usize]
}

#[no_mangle]
pub extern "C" fn kite_rt_str_concat(a: u64, b: u64) -> u64 {
    let mut a = a;
    let mut b = b;
    unsafe {
        let total = str_bytes(a).len() + str_bytes(b).len();
        root(&mut a);
        root(&mut b);
        let p = alloc(HEADER + round8(total));
        unroot(2);
        let (abytes, bbytes) = (str_bytes(a), str_bytes(b));
        *(p as *mut u64) = word0(obj::STR, total as u32);
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(abytes.as_ptr(), p.add(HEADER), abytes.len());
        std::ptr::copy_nonoverlapping(
            bbytes.as_ptr(),
            p.add(HEADER + abytes.len()),
            bbytes.len(),
        );
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_str_eq(a: u64, b: u64) -> u8 {
    unsafe { u8::from(str_bytes(a) == str_bytes(b)) }
}

/// -1, 0 or 1, by code point — which is exactly what Rust's `str` ordering
/// is, so both native and bytecode sort the same way.
#[no_mangle]
pub extern "C" fn kite_rt_str_compare(a: u64, b: u64) -> i64 {
    unsafe {
        match str_str(a).cmp(str_str(b)) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

// Every string operation counts characters, not bytes — the same walk the VM
// does, at the same linear cost, because a string is text rather than
// storage.

#[no_mangle]
pub extern "C" fn kite_rt_str_len(s: u64) -> i64 {
    unsafe { str_str(s).chars().count() as i64 }
}

#[no_mangle]
pub extern "C" fn kite_rt_str_trim(s: u64) -> u64 {
    let mut s = s;
    unsafe {
        let text = str_str(s);
        let trimmed = text.trim();
        // Offsets survive a collection; borrows into the heap do not.
        let start = trimmed.as_ptr() as usize - text.as_ptr() as usize;
        let len = trimmed.len();
        root(&mut s);
        let p = alloc(HEADER + round8(len));
        unroot(1);
        *(p as *mut u64) = word0(obj::STR, len as u32);
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(str_bytes(s).as_ptr().add(start), p.add(HEADER), len);
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_str_slice(s: u64, from: i64, to: i64) -> u64 {
    let mut s = s;
    unsafe {
        let text = str_str(s);
        // Clamped rather than trapping: an out-of-range slice is an ordinary
        // condition in text processing.
        let char_len = text.chars().count() as i64;
        let from_c = from.clamp(0, char_len) as usize;
        let to_c = to.clamp(from.clamp(0, char_len), char_len) as usize;
        let byte_at = |c: usize| {
            text.char_indices()
                .nth(c)
                .map(|(b, _)| b)
                .unwrap_or(text.len())
        };
        let start = byte_at(from_c);
        let end = byte_at(to_c);
        let len = end - start;
        root(&mut s);
        let p = alloc(HEADER + round8(len));
        unroot(1);
        *(p as *mut u64) = word0(obj::STR, len as u32);
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(str_bytes(s).as_ptr().add(start), p.add(HEADER), len);
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_str_index_of(s: u64, needle: u64) -> i64 {
    unsafe {
        let text = str_str(s);
        // A byte offset from `find` means nothing to a caller counting
        // characters, so it is converted rather than returned.
        match text.find(str_str(needle)) {
            Some(byte) => text[..byte].chars().count() as i64,
            None => -1,
        }
    }
}

/// The code point at a character index, or -1 past the end — an ordinary
/// condition in text processing, like an out-of-range slice.
#[no_mangle]
pub extern "C" fn kite_rt_str_code_at(s: u64, at: i64) -> i64 {
    unsafe {
        match usize::try_from(at)
            .ok()
            .and_then(|i| str_str(s).chars().nth(i))
        {
            Some(c) => c as i64,
            None => -1,
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering — the VM's `Display`, transcribed
// ---------------------------------------------------------------------------

/// Print a float so it reads back as a Kite float: `1.0`, not `1`.
fn float_text(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

fn render(word: u64, k: u8, out: &mut String) {
    match k {
        kind::UNIT => out.push_str("()"),
        kind::BOOL => out.push_str(if word != 0 { "true" } else { "false" }),
        kind::INT => out.push_str(&(word as i64).to_string()),
        kind::FLOAT => out.push_str(&float_text(f64::from_bits(word))),
        _ => render_ref(word, out),
    }
}

fn render_ref(p: u64, out: &mut String) {
    if p == 0 {
        out.push_str("nil");
        return;
    }
    unsafe {
        let o = p as *const u8;
        let aux = obj_aux(o) as usize;
        let fields = |kinds: &[u8], base: usize, open: &str, close: &str, out: &mut String| {
            out.push_str(open);
            for (i, k) in kinds.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render(*slot(o, base + i), *k, out);
            }
            out.push_str(close);
        };
        match obj_kind(o) {
            obj::STR => out.push_str(str_str(p)),
            // Debug-shaped output until the `Display` trait lands — matching
            // the VM's, which is the point.
            obj::STRUCT => fields(&rt().shapes.structs[aux].clone(), 0, "{", "}", out),
            obj::ENUM => {
                let variant = (obj_word1(o) & 0xFFFF_FFFF) as usize;
                out.push('#');
                out.push_str(&variant.to_string());
                let kinds = rt().shapes.enums[aux][variant].clone();
                if !kinds.is_empty() {
                    fields(&kinds, 0, "(", ")", out);
                }
            }
            obj::TUPLE => fields(&rt().shapes.tuples[aux].clone(), 0, "(", ")", out),
            obj::SLICE => {
                let len = obj_word1(o) as usize;
                out.push('[');
                for i in 0..len {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render(*slot(o, i), aux as u8, out);
                }
                out.push(']');
            }
            obj::MAP => {
                let len = obj_word1(o) as usize;
                let (kk, vk) = ((aux & 0xFF) as u8, ((aux >> 8) & 0xFF) as u8);
                out.push('{');
                for i in 0..len {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    render(*slot(o, 2 * i), kk, out);
                    out.push_str(": ");
                    render(*slot(o, 2 * i + 1), vk, out);
                }
                out.push('}');
            }
            obj::PAIR => {
                out.push('(');
                render(*slot(o, 0), aux as u8, out);
                out.push_str(", ");
                render_ref(*slot(o, 1), out);
                out.push(')');
            }
            obj::ERR => render_ref(*slot(o, 0), out),
            // A closure has no text form; this only ever appears in a debug
            // dump, exactly as in the VM.
            obj::CLOSURE => {
                out.push_str("closure#");
                out.push_str(&aux.to_string());
            }
            // An optional is transparent: the payload's own rendering, or nil
            // — the box is representation, not value.
            obj::BOX => render(*slot(o, 0), aux as u8, out),
            other => unreachable!("rendering an object of kind {}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// Structural equality — the VM's `PartialEq`, transcribed
// ---------------------------------------------------------------------------

fn value_eq(a: u64, b: u64, k: u8) -> bool {
    match k {
        kind::FLOAT => f64::from_bits(a) == f64::from_bits(b),
        kind::REF => ref_eq(a, b),
        _ => a == b,
    }
}

fn ref_eq(a: u64, b: u64) -> bool {
    if a == 0 || b == 0 {
        return a == b;
    }
    unsafe {
        let (pa, pb) = (a as *const u8, b as *const u8);
        let (ka, kb) = (obj_kind(pa), obj_kind(pb));
        if ka != kb {
            return false;
        }
        let each = |kinds: &[u8], base: usize| {
            kinds
                .iter()
                .enumerate()
                .all(|(i, k)| value_eq(*slot(pa, base + i), *slot(pb, base + i), *k))
        };
        match ka {
            obj::STR => str_bytes(a) == str_bytes(b),
            obj::STRUCT => {
                obj_aux(pa) == obj_aux(pb)
                    && each(&rt().shapes.structs[obj_aux(pa) as usize].clone(), 0)
            }
            obj::ENUM => {
                if obj_aux(pa) != obj_aux(pb) || obj_word1(pa) != obj_word1(pb) {
                    return false;
                }
                let variant = (obj_word1(pa) & 0xFFFF_FFFF) as usize;
                each(&rt().shapes.enums[obj_aux(pa) as usize][variant].clone(), 0)
            }
            obj::TUPLE => each(&rt().shapes.tuples[obj_aux(pa) as usize].clone(), 0),
            obj::SLICE => {
                let (la, lb) = (obj_word1(pa) as usize, obj_word1(pb) as usize);
                la == lb
                    && (0..la).all(|i| value_eq(*slot(pa, i), *slot(pb, i), obj_aux(pa) as u8))
            }
            obj::MAP => {
                // In order: the VM compares the entry vectors directly, so two
                // maps built in different orders are different values.
                let (la, lb) = (obj_word1(pa) as usize, obj_word1(pb) as usize);
                let (kk, vk) = ((obj_aux(pa) & 0xFF) as u8, ((obj_aux(pa) >> 8) & 0xFF) as u8);
                la == lb
                    && (0..la).all(|i| {
                        value_eq(*slot(pa, 2 * i), *slot(pb, 2 * i), kk)
                            && value_eq(*slot(pa, 2 * i + 1), *slot(pb, 2 * i + 1), vk)
                    })
            }
            obj::PAIR => {
                value_eq(*slot(pa, 0), *slot(pb, 0), obj_aux(pa) as u8)
                    && ref_eq(*slot(pa, 1), *slot(pb, 1))
            }
            obj::ERR => ref_eq(*slot(pa, 0), *slot(pb, 0)),
            obj::BOX => value_eq(*slot(pa, 0), *slot(pb, 0), obj_aux(pa) as u8),
            // The VM's equality has no closure arm, so closures — even the
            // same closure — compare unequal.
            obj::CLOSURE => false,
            other => unreachable!("comparing objects of kind {}", other),
        }
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_value_eq(a: u64, b: u64, k: u64) -> u8 {
    u8::from(value_eq(a, b, k as u8))
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn write_line(line: &str) {
    let rt = rt();
    match &mut rt.capture {
        Some(buf) => {
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        None => {
            // A closed pipe is the host's problem, not the program's.
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(line.as_bytes());
            let _ = out.write_all(b"\n");
        }
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_print_int(v: i64) {
    write_line(&v.to_string());
}

#[no_mangle]
pub extern "C" fn kite_rt_print_float(v: f64) {
    write_line(&float_text(v));
}

#[no_mangle]
pub extern "C" fn kite_rt_print_bool(v: u8) {
    write_line(if v != 0 { "true" } else { "false" });
}

#[no_mangle]
pub extern "C" fn kite_rt_print_str(s: u64) {
    unsafe { write_line(str_str(s)) };
}

#[no_mangle]
pub extern "C" fn kite_rt_print_unit() {
    write_line("()");
}

/// `io.print` of anything else — a reference rendered the way the VM renders
/// it. The checker steers programs towards `Display`, so this is a debug door
/// rather than a common path.
#[no_mangle]
pub extern "C" fn kite_rt_print_ref(p: u64) {
    let mut s = String::new();
    render_ref(p, &mut s);
    write_line(&s);
}

#[no_mangle]
pub extern "C" fn kite_rt_str_of_int(v: i64) -> u64 {
    make_str(v.to_string().as_bytes())
}

/// A one-character string from a code point, or the empty string when the
/// value is not a character — a surrogate, or past the end of Unicode.
#[no_mangle]
pub extern "C" fn kite_rt_text_from_code(code: i64) -> u64 {
    let text = u32::try_from(code)
        .ok()
        .and_then(char::from_u32)
        .map(String::from)
        .unwrap_or_default();
    make_str(text.as_bytes())
}

#[no_mangle]
pub extern "C" fn kite_rt_str_of_float(v: f64) -> u64 {
    make_str(float_text(v).as_bytes())
}

#[no_mangle]
pub extern "C" fn kite_rt_str_of_bool(v: u8) -> u64 {
    make_str(if v != 0 { b"true" } else { b"false" as &[u8] })
}

#[no_mangle]
pub extern "C" fn kite_rt_str_of_ref(p: u64) -> u64 {
    let mut s = String::new();
    render_ref(p, &mut s);
    make_str(s.as_bytes())
}

// ---------------------------------------------------------------------------
// Drawing and measurement
// ---------------------------------------------------------------------------
//
// The native runtime has no window, exactly like the VM: each call is written
// out, which is what lets the differential suite compare drawing across three
// backends without a browser.

/// The width of one character with no font to ask, and the line height.
/// The same numbers the VM and the generated glue use, which is what keeps a
/// layout comparable across backends under test.
const NOMINAL_ADVANCE: f64 = 8.0;
const NOMINAL_LINE_HEIGHT: f64 = 16.0;

/// The size `draw.font` last selected, over the nominal one. Global because
/// the drawing boundary is: there is one host, and `draw.clip` is already kept
/// this way.
static FONT_SCALE: AtomicU64 = AtomicU64::new(0x3ff0_0000_0000_0000); // 1.0

fn font_scale() -> f64 {
    f64::from_bits(FONT_SCALE.load(Ordering::Relaxed))
}

/// `draw.font(size, weight)`. Nothing here has a font, so the weight is
/// recorded in the transcript and only the size changes measurement.
#[no_mangle]
pub extern "C" fn kite_rt_draw_font(size: f64, _weight: i64) {
    FONT_SCALE.store((size / NOMINAL_LINE_HEIGHT).to_bits(), Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn kite_rt_draw_rect(x: f64, y: f64, w: f64, h: f64, colour: i64) {
    write_line(&format!(
        "rect {} {} {} {} {}",
        float_text(x),
        float_text(y),
        float_text(w),
        float_text(h),
        colour
    ));
}

/// A rounded rectangle. The radius rides between the size and the colour, so
/// the transcript reads the same as `rect` with one more number in it.
#[no_mangle]
pub extern "C" fn kite_rt_draw_rrect(x: f64, y: f64, w: f64, h: f64, r: f64, colour: i64) {
    write_line(&format!(
        "rrect {} {} {} {} {} {}",
        float_text(x),
        float_text(y),
        float_text(w),
        float_text(h),
        float_text(r),
        colour
    ));
}

#[no_mangle]
pub extern "C" fn kite_rt_draw_drrect(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    width: f64,
    colour: i64,
) {
    write_line(&format!(
        "drrect {} {} {} {} {} {} {}",
        float_text(x),
        float_text(y),
        float_text(w),
        float_text(h),
        float_text(r),
        float_text(width),
        colour
    ));
}

/// Written, unlike a font selection: alpha changes the pixels a call produces
/// and shows up nowhere else in the transcript.
#[no_mangle]
pub extern "C" fn kite_rt_draw_alpha(a: f64) {
    write_line(&format!("alpha {}", float_text(a)));
}

#[no_mangle]
pub extern "C" fn kite_rt_draw_text(x: f64, y: f64, body: u64, colour: i64) {
    unsafe {
        write_line(&format!(
            "text {} {} {} {}",
            float_text(x),
            float_text(y),
            str_str(body),
            colour
        ));
    }
}

/// A text input. Native has no DOM to put a real one in, so it writes what the
/// field says — the same degradation the canvas renderer makes, and the reason
/// this call is safe to add to a boundary three backends have to agree on.
#[no_mangle]
pub extern "C" fn kite_rt_draw_field(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: u64,
    hint: u64,
    colour: i64,
    id: u64,
    multiline: i8,
) {
    unsafe {
        // A value may contain a newline, and a transcript is read a line at a
        // time. Escaping keeps one call to one line.
        let show = |s: &str| s.replace('\\', "\\\\").replace('\n', "\\n");
        write_line(&format!(
            "field {} {} {} {} {} {} {} {} {}",
            float_text(x),
            float_text(y),
            float_text(w),
            float_text(h),
            show(str_str(value)),
            show(str_str(hint)),
            colour,
            show(str_str(id)),
            multiline != 0
        ));
    }
}

/// A picture. Native has no surface to decode one onto, so it records the box
/// and the source — the geometry being the part anything reading a transcript
/// can check.
#[no_mangle]
pub extern "C" fn kite_rt_draw_image(x: f64, y: f64, w: f64, h: f64, src: u64) {
    unsafe {
        write_line(&format!(
            "image {} {} {} {} {}",
            float_text(x),
            float_text(y),
            float_text(w),
            float_text(h),
            str_str(src)
        ));
    }
}

/// The parallel tree, written down. Nothing here paints; the point is that a
/// transcript can be audited. Nothing in the toolchain audits it now — the
/// reader went with `std/ui` — so this is what a canvas program has to say
/// what it drew.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn kite_rt_draw_semantics(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    role: i64,
    label: u64,
    flags: i64,
    id: u64,
) {
    unsafe {
        let show = |s: &str| s.replace('\\', "\\\\").replace('\n', "\\n");
        // Label last: it is the only field that may contain a space.
        write_line(&format!(
            "semantics {} {} {} {} {} {} {} {}",
            float_text(x),
            float_text(y),
            float_text(w),
            float_text(h),
            role,
            flags,
            show(str_str(id)),
            show(str_str(label))
        ));
    }
}


// ---- standard error, and standard input -------------------------------------
//
// A diagnostic goes to a different stream from what a program produces, which
// is what lets output be piped somewhere while a person still reads the
// complaints.

#[no_mangle]
pub extern "C" fn kite_rt_error_int(v: i64) {
    eprintln!("{}", v);
}

#[no_mangle]
pub extern "C" fn kite_rt_error_float(v: f64) {
    eprintln!("{}", float_text(v));
}

#[no_mangle]
pub extern "C" fn kite_rt_error_bool(v: i8) {
    eprintln!("{}", v != 0);
}

#[no_mangle]
pub extern "C" fn kite_rt_error_str(v: u64) {
    unsafe {
        eprintln!("{}", str_str(v));
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_error_unit() {
    eprintln!("()");
}

/// One line from standard input, without its newline. The empty string at end
/// of input.
#[no_mangle]
pub extern "C" fn kite_rt_read_line() -> u64 {
    use std::io::BufRead;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if read == 0 {
        return make_str(b"");
    }
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    make_str(trimmed.as_bytes())
}

#[no_mangle]
pub extern "C" fn kite_rt_draw_clip(x: f64, y: f64, w: f64, h: f64) {
    write_line(&format!(
        "clip {} {} {} {}",
        float_text(x),
        float_text(y),
        float_text(w),
        float_text(h)
    ));
}

#[no_mangle]
pub extern "C" fn kite_rt_draw_unclip() {
    write_line("unclip");
}

#[no_mangle]
pub extern "C" fn kite_rt_text_width(s: u64) -> f64 {
    unsafe { str_str(s).chars().count() as f64 * NOMINAL_ADVANCE * font_scale() }
}

#[no_mangle]
pub extern "C" fn kite_rt_text_height() -> f64 {
    NOMINAL_LINE_HEIGHT * font_scale()
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn kite_rt_require(cond: u8, message: u64) {
    if cond != 0 {
        return;
    }
    let text = if message == 0 {
        "a claim about this program does not hold".to_string()
    } else {
        unsafe { str_str(message).to_string() }
    };
    trap(&text);
}

// ---------------------------------------------------------------------------
// The host boundary
// ---------------------------------------------------------------------------

/// A call across the declared host boundary. The native runtime is its own
/// host and supplies nothing beyond the builtins — no DOM, no network — so
/// reaching one of these is a statement about where the program is running,
/// exactly as it is on the bytecode VM with no embedder.
#[no_mangle]
pub extern "C" fn kite_rt_call_extern(index: u64, _argc: u64) -> u64 {
    let name = rt()
        .shapes
        .externs
        .get(index as usize)
        .cloned()
        .unwrap_or_else(|| "?".to_string());
    trap(&format!(
        "`{}` is a host function, and this runtime supplies no host",
        name
    ));
}

// ---------------------------------------------------------------------------
// The scheduler — the VM's `drive`, transcribed
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn kite_rt_task_spawn(poll: u64) {
    rt().tasks.push(Scheduled {
        poll,
        wake_at: None,
        parked: false,
        waiting_on_host: false,
    });
}

/// A hint, not a promise: the code that asked re-checks the clock.
#[no_mangle]
pub extern "C" fn kite_rt_task_wake_at(ms: i64) {
    rt().wake_request = Some(ms);
}

#[no_mangle]
pub extern "C" fn kite_rt_task_park() {
    rt().park_request = true;
}

#[no_mangle]
pub extern "C" fn kite_rt_task_wait_host() {
    rt().host_wait_request = true;
}

#[no_mangle]
pub extern "C" fn kite_rt_time_now() -> i64 {
    rt().clock
}

/// Run one task's resume closure to its own return. The closure's first
/// payload slot is its thunk, compiled with the shared `fn() -> bool`
/// signature: the closure itself, then the answer.
fn poll_task(poll: u64) -> bool {
    unsafe {
        let thunk = *slot(poll as *const u8, 0);
        let f: extern "C" fn(u64) -> u8 = std::mem::transmute(thunk as usize);
        f(poll) != 0
    }
}

/// Poll every live task until none is left. Round-robin, in spawn order, on a
/// **virtual** clock that jumps to the earliest deadline when everything is
/// waiting — the properties that make the interleaving deterministic and
/// differentially comparable, ported from the VM unchanged.
#[no_mangle]
pub extern "C" fn kite_rt_drive() {
    while !rt().tasks.is_empty() {
        let mut polled = false;
        let mut completed = false;
        let mut i = 0;
        while i < rt().tasks.len() {
            {
                let t = &rt().tasks[i];
                if t.parked || t.waiting_on_host || t.wake_at.is_some_and(|w| w > rt().clock) {
                    i += 1;
                    continue;
                }
            }
            polled = true;
            rt().tasks[i].wake_at = None;
            rt().wake_request = None;
            rt().park_request = false;
            rt().host_wait_request = false;
            let poll = rt().tasks[i].poll;
            let done = poll_task(poll);
            if i < rt().tasks.len() {
                let wake = rt().wake_request.take();
                let parked = std::mem::take(&mut rt().park_request);
                let hosted = std::mem::take(&mut rt().host_wait_request);
                let t = &mut rt().tasks[i];
                t.wake_at = wake;
                t.parked = parked;
                t.waiting_on_host = hosted;
            }
            if done {
                rt().tasks.remove(i);
                completed = true;
            } else {
                i += 1;
            }
        }
        // A completion wakes everything and lets each task decide for itself.
        if completed {
            for t in rt().tasks.iter_mut() {
                t.parked = false;
                t.wake_at = None;
            }
        }
        if !polled {
            // Everything is waiting. There is no host here, so the clock is
            // the only thing that can move — and if nothing is waiting on it,
            // nothing will ever happen.
            match rt().tasks.iter().filter_map(|t| t.wake_at).min() {
                Some(next) if next > rt().clock => {
                    rt().clock = next;
                    for t in rt().tasks.iter_mut() {
                        t.waiting_on_host = false;
                    }
                }
                _ => {
                    let waiting = rt().tasks.len();
                    trap(&format!(
                        "{} task{} can never make progress",
                        waiting,
                        if waiting == 1 { "" } else { "s" }
                    ));
                }
            }
        }
    }
    // The run is over; what was captured is the program's output.
    if rt().capture.is_none() {
        let _ = std::io::stdout().lock().flush();
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The address of the implementation a virtual call should enter, from the
/// receiver's own tag — a struct id, or an enum id shifted clear of them.
#[no_mangle]
pub extern "C" fn kite_rt_virtual_lookup(receiver: u64, table: u64, method: u64) -> u64 {
    unsafe {
        let p = receiver as *const u8;
        let tag = match obj_kind(p) {
            obj::STRUCT => obj_aux(p),
            obj::ENUM => 0x8000_0000 | obj_aux(p),
            other => unreachable!("virtual call on an object of kind {}", other),
        };
        let rows = &rt().shapes.vtables[table as usize];
        match rows.binary_search_by_key(&tag, |(t, _)| *t) {
            Ok(row) => rows[row].1[method as usize] as u64,
            // The checker proved the receiver implements the trait, so this
            // is a compiler bug rather than a program one.
            Err(_) => trap("virtual dispatch found no implementation"),
        }
    }
}

// ---------------------------------------------------------------------------
// The JIT's symbol table
// ---------------------------------------------------------------------------

/// Every symbol compiled code may reference, for the JIT to resolve. The
/// object-file path needs none of this: there the linker reads the same names
/// out of the `staticlib` build of this crate.
pub fn jit_symbols() -> Vec<(&'static str, *const u8)> {
    macro_rules! syms {
        ($($name:ident),* $(,)?) => {
            vec![$((stringify!($name), $name as *const u8)),*]
        };
    }
    let mut v: Vec<(&'static str, *const u8)> = syms![
        kite_rt_startup,
        kite_rt_trap,
        kite_rt_register_string,
        kite_rt_register_struct_shape,
        kite_rt_register_enum_shape,
        kite_rt_register_tuple_shape,
        kite_rt_register_closure,
        kite_rt_register_fn_name,
        kite_rt_register_extern,
        kite_rt_register_vtable_method,
        kite_rt_register_stack_maps,
        kite_rt_gc_count,
        kite_rt_struct_new,
        kite_rt_enum_new,
        kite_rt_tuple_new,
        kite_rt_slice_new,
        kite_rt_closure_new,
        kite_rt_map_new,
        kite_rt_box_new,
        kite_rt_pair_new,
        kite_rt_error_new,
        kite_rt_error_message,
        kite_rt_error_cause,
        kite_rt_error_tag,
        kite_rt_error_as,
        kite_rt_set_field,
        kite_rt_index_get,
        kite_rt_set_index,
        kite_rt_slice_push,
        kite_rt_slice_len,
        kite_rt_slice_get,
        kite_rt_slice_range,
        kite_rt_map_len,
        kite_rt_map_get,
        kite_rt_map_set,
        kite_rt_map_keys,
        kite_rt_map_values,
        kite_rt_str_const,
        kite_rt_str_concat,
        kite_rt_str_eq,
        kite_rt_str_compare,
        kite_rt_str_len,
        kite_rt_str_trim,
        kite_rt_str_slice,
        kite_rt_str_index_of,
        kite_rt_str_code_at,
        kite_rt_value_eq,
        kite_rt_virtual_lookup,
        kite_rt_print_int,
        kite_rt_print_float,
        kite_rt_print_bool,
        kite_rt_print_str,
        kite_rt_print_unit,
        kite_rt_print_ref,
        kite_rt_str_of_int,
        kite_rt_text_from_code,
        kite_rt_read_line,
        kite_rt_error_int,
        kite_rt_error_float,
        kite_rt_error_bool,
        kite_rt_error_str,
        kite_rt_error_unit,
        kite_rt_str_of_float,
        kite_rt_str_of_bool,
        kite_rt_str_of_ref,
        kite_rt_draw_rect,
        kite_rt_draw_rrect,
        kite_rt_draw_font,
        kite_rt_draw_drrect,
        kite_rt_draw_alpha,
        kite_rt_draw_text,
        kite_rt_draw_field,
        kite_rt_draw_image,
        kite_rt_draw_semantics,
        kite_rt_draw_clip,
        kite_rt_draw_unclip,
        kite_rt_text_width,
        kite_rt_text_height,
        kite_rt_require,
        kite_rt_call_extern,
        kite_rt_task_spawn,
        kite_rt_task_wake_at,
        kite_rt_task_park,
        kite_rt_task_wait_host,
        kite_rt_time_now,
        kite_rt_drive,
    ];
    v.push(("KITE_RT_STAGE", (&raw const KITE_RT_STAGE).cast::<u8>()));
    v
}
