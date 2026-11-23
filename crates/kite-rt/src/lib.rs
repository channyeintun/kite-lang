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
//! distances below that frame's own frame pointer are recorded. At collection
//! time the runtime walks the frame-pointer chain; a frame whose return
//! address is a registered safepoint has its caller's recorded slots visited,
//! and every other frame — the runtime's own Rust frames included — is
//! skipped, because nothing in it can hold a Kite reference the maps do not
//! already cover.
//! Cranelift spills stack-map values before each safepoint and reloads them
//! after, which is exactly what allows the nursery to move objects.
//!
//! The **write barrier** covers the in-place heap mutations: a `var` field
//! assignment, and a slice write or push into a slice the compiled code owns
//! (see [`kite_rt_slice_push`]). Map writes are copy-on-write and allocate a
//! fresh object. Every in-place store of a reference goes through
//! [`remember`], so the barrier is one function. An old object that has a
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

mod sched;

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

/// On a slice or a map: a struct field is the one thing that holds it.
///
/// A write through a field (`q.items.push(v)`) takes the field's slice into
/// a local, changes it and puts it back (§5.4). The compiled code keeps an
/// owned flag for a local, but a field has nowhere to keep one, so it is kept
/// on the object: the write-back sets this bit when the local owned what it
/// put back, the next take reads it as the local's flag, and every read of
/// the field that could keep what it read clears it first. The object is
/// copied — a fresh header — wherever a copy is made, so a copy never has
/// it. See `kite-codegen-clif`'s `slices` module for the whole rule.
pub const HELD_BIT: u64 = 1 << 10;

/// Header: two words. Word 0 is `kind | gc bits | aux << 32`; word 1 is
/// kind-specific (a length, a variant, a capture count; for a slice, the
/// length and the capacity — see [`slice_len`]; for a string, its count of
/// characters once something has counted them — see [`str_chars`]). Payload
/// slots follow.
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

/// A slice's length: the low half of its second header word.
///
/// A slice is one object with room to spare. The high half of word 1 is its
/// capacity — how many payload slots the object was allocated with — and only
/// the first `len` of them are elements; the rest have never been written, so
/// nothing reads them. The collector traces, and the renderer and `==` walk,
/// the length; only the object's size comes from the capacity. The room is
/// what lets a push go straight into a slice the compiled code owns (see
/// [`kite_rt_slice_push`]).
///
/// The code generator reads the length inline, as a 32-bit load of word 1 —
/// the low half on both little-endian targets the backend supports.
#[inline]
unsafe fn slice_len(p: *const u8) -> usize {
    (obj_word1(p) & 0xFFFF_FFFF) as usize
}

#[inline]
unsafe fn slice_cap(p: *const u8) -> usize {
    (obj_word1(p) >> 32) as usize
}

#[inline]
unsafe fn set_slice_len(p: *mut u8, len: usize) {
    *(p as *mut u64).add(1) = len as u64 | ((slice_cap(p) as u64) << 32);
}

/// The most elements a slice can hold here: its length and capacity share
/// one header word. Eight bytes a slot makes that 32 GiB, which no program has
/// reached; a trap says so rather than a length that wrapped.
const SLICE_MAX: usize = u32::MAX as usize;

/// Allocate a slice of element kind `elem_kind` with `len` elements and room
/// for `cap`, header written and payload not. May collect, so the caller
/// roots whatever it holds across it.
fn slice_alloc(elem_kind: u32, len: usize, cap: usize) -> *mut u8 {
    debug_assert!(len <= cap);
    if cap > SLICE_MAX {
        trap(&format!("a slice cannot hold more than {} elements", SLICE_MAX));
    }
    let p = alloc(HEADER + 8 * cap);
    unsafe {
        *(p as *mut u64) = word0(obj::SLICE, elem_kind);
        *(p as *mut u64).add(1) = len as u64 | ((cap as u64) << 32);
    }
    p
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
/// collect. A slice or map literal longer than the window is staged a window
/// at a time, each one added by [`kite_rt_slice_extend`] or
/// [`kite_rt_map_extend`] to what the ones before it built.
pub const STAGE_WORDS: usize = 4096;

#[no_mangle]
pub static mut KITE_RT_STAGE: [u64; STAGE_WORDS] = [0; STAGE_WORDS];

#[inline]
fn stage_slot(i: usize) -> *mut u64 {
    unsafe { (&raw mut KITE_RT_STAGE).cast::<u64>().add(i) }
}

/// How deep a chain of calls may go: the bytecode VM's `MAX_FRAMES`.
///
/// The two must agree, because a recursion one of them finishes and the
/// other traps on is a program that means two things. Natively the chain
/// used to be as deep as the stack allowed — past a hundred thousand frames
/// where the VM trapped, and then a Rust stack overflow and an abort rather
/// than a trap. Every compiled function counts itself into
/// [`KITE_RT_DEPTH`] on entry and out on return, and the call past this one
/// traps with the VM's message; the program runs on a stack
/// ([`PROGRAM_STACK`]) that holds that many frames of any ordinary size.
pub const MAX_FRAMES: u64 = 100_000;

/// How many compiled Kite calls are in progress. See [`MAX_FRAMES`].
#[no_mangle]
pub static mut KITE_RT_DEPTH: u64 = 0;

/// The stack a program runs on: [`MAX_FRAMES`] frames of five kilobytes.
///
/// The thread that starts a program has what its host gave it — eight
/// megabytes for a process's main thread on Linux, which a recursion of a
/// function with a few dozen values live across its call outgrew at a few
/// tens of thousands of frames. The memory is reserved, not used: what a
/// program does not recurse into is never touched.
const PROGRAM_STACK: usize = 512 << 20;

/// Run a compiled program's entry on a stack of [`PROGRAM_STACK`] and answer
/// what it answers. The exported `main` of every build calls this with the
/// function that starts the runtime and runs the program, so the frame
/// pointer [`kite_rt_startup`] records, and every frame the collector walks,
/// is on that stack.
///
/// A host that will not make such a thread still runs the program, on the
/// stack it has, which is how things were before.
#[no_mangle]
pub extern "C" fn kite_rt_run(program: extern "C" fn() -> i32) -> i32 {
    let spawned = std::thread::Builder::new()
        .name("kite".to_string())
        .stack_size(PROGRAM_STACK)
        .spawn(move || program());
    match spawned {
        // A panic cannot cross the `extern "C"` functions it would start in,
        // so it aborts the process there; a thread that nonetheless ended in
        // one ends the program the way a trap does.
        Ok(thread) => thread.join().unwrap_or(1),
        Err(_) => program(),
    }
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

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
    /// Per declared host function, its signature as the program declared it
    /// (encoded as the host boundary describes), so a declaration that does
    /// not match what the host implements is a trap rather than a string read
    /// out of an integer.
    extern_sigs: Vec<Vec<u8>>,
    /// Per dispatch table: rows of `(type tag, method addresses)`, sorted.
    vtables: Vec<Vec<(u32, Vec<usize>)>>,
    /// Per struct id, whether it has a `var` field: a map hashes such a
    /// struct by its type alone (see [`key_hash`]).
    mutable_structs: Vec<bool>,
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
    /// The least `threshold` is ever set to. A test lowers it to make major
    /// collections happen at all; everything else gets the default.
    min_threshold: usize,
    /// Collections this run, for a harness that must know the collector it
    /// meant to exercise actually ran.
    minor_collections: u64,
    major_collections: u64,
    /// Old objects a reference has been stored into since the last minor
    /// collection. Scanned as roots so the nursery never needs a full
    /// old-generation scan.
    remembered: Vec<*mut u8>,
    /// Slots inside the runtime's own Rust frames that hold references across
    /// a possible collection. Registered explicitly, because Rust frames have
    /// no stack maps.
    extra_roots: Vec<*mut u64>,
    /// `(safepoint pc, slot distances below that frame's fp)`, sorted by pc.
    stack_maps: Vec<(usize, Vec<u32>)>,

    // ---- the program ---------------------------------------------------
    shapes: Shapes,
    /// String constants, as immortal old-generation objects.
    strings: Vec<u64>,
    /// Where every [`CHAR_STRIDE`]-th character starts, in the few long
    /// strings that are not ASCII most recently used, keyed by address —
    /// see [`str_byte_at`]. An address names one object only until a
    /// collection moves or frees it, so a minor collection drops the
    /// nursery's entries and a major one drops them all.
    char_marks: Vec<(u64, Vec<u32>)>,

    // ---- the scheduler -------------------------------------------------
    /// The live tasks. Each one's resume closure is a heap reference and a
    /// root: the scheduler may be the only thing keeping a suspended task's
    /// frame alive. A task's number is a plain integer, so what an `await`
    /// parks on is no reference at all, and nothing new needs tracing.
    tasks: sched::Tasks<u64>,
    clock: i64,
    wake_request: Option<i64>,
    park_request: bool,
    park_on_request: Option<u64>,
    host_wait_request: bool,

    // ---- output --------------------------------------------------------
    capture: Option<Vec<u8>>,
}

static mut RT: *mut Rt = std::ptr::null_mut();

/// How the next run is set up, when something other than a linked executable
/// starts it. Consumed by `kite_rt_startup`.
static NEXT_RUN: Mutex<Option<(RunConfig, bool)>> = Mutex::new(None);

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

/// What a harness running a program in this process may change about the
/// collector. The defaults are what a linked executable gets.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunConfig {
    /// The nursery's size in bytes, in place of `KITE_NURSERY_BYTES` and the
    /// default — small enough, and a test forces collections without
    /// allocating gigabytes.
    pub nursery_bytes: Option<usize>,
    /// The old-generation size that triggers a major collection, and the
    /// least it is ever raised back to. The default is 8 MB, which no test
    /// program reaches; lowering it is how a test makes mark-and-sweep run.
    pub major_threshold: Option<usize>,
}

/// What the collector did during a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub minor_collections: u64,
    pub major_collections: u64,
}

/// Set up the next run, which starts when the compiled wrapper calls
/// `kite_rt_startup`. With `capture`, what the program prints is kept for
/// [`finish_run`] instead of being written to stdout — which is for a harness
/// comparing output, and nothing else: captured output is lost if the program
/// crashes, and it cannot interleave with standard error.
///
/// One-shot, and meant to be called under [`run_lock`] immediately before the
/// entry, so that neither a neighbouring test nor a compilation that failed
/// after asking can leave it set for a run it was not meant for.
pub fn prepare_run(config: RunConfig, capture: bool) {
    *NEXT_RUN.lock().unwrap_or_else(|p| p.into_inner()) = Some((config, capture));
}

/// After a run started by the compiled wrapper returns: what it printed under
/// capture, and what the collector did. Then free its heap — nothing can
/// reach it once the entry has returned, and holding it until the next
/// startup would keep the whole of the last program's memory alive for as
/// long as the process lasts.
pub fn finish_run() -> (Vec<u8>, RunStats) {
    unsafe {
        if RT.is_null() {
            return (Vec::new(), RunStats::default());
        }
        let mut done = Box::from_raw(RT);
        RT = std::ptr::null_mut();
        ENTRY_FP = 0;
        let captured = done.capture.take().unwrap_or_default();
        let stats = RunStats {
            minor_collections: done.minor_collections,
            major_collections: done.major_collections,
        };
        release(done);
        (captured, stats)
    }
}

#[allow(static_mut_refs)]
fn rt() -> &'static mut Rt {
    unsafe {
        assert!(!RT.is_null(), "kite_rt_startup was never called");
        &mut *RT
    }
}

/// The runtime's state, if a run is in progress — for the one caller that
/// must work either way.
#[allow(static_mut_refs)]
fn rt_if_started() -> Option<&'static mut Rt> {
    unsafe { RT.as_mut() }
}

const DEFAULT_NURSERY: usize = 1 << 20;
const DEFAULT_THRESHOLD: usize = 8 << 20;

/// The least and the most a nursery may be. The least is one page, which is
/// what a stress test asks for; the most is far past any use for one — a
/// nursery is collected when full, and a larger one only collects less often
/// — and keeps an unchecked number from `KITE_NURSERY_BYTES` well inside what
/// a `Layout` can describe. `18446744073709551615` there was a panic about a
/// `LayoutError`, and a few terabytes an assertion, both inside the runtime,
/// where a trap is the only way out that says what happened.
const MIN_NURSERY: usize = 4096;
const MAX_NURSERY: usize = 1 << 30;

/// Initialise — or fully reset — the runtime. The wrapper the code generator
/// emits calls this before anything else, which is also what makes a second
/// JIT run in one process start clean.
#[no_mangle]
pub extern "C" fn kite_rt_startup() {
    // The compiled entry's frame pointer. The entry is what called this, so
    // this function's own frame record holds it — the saved frame pointer is
    // the record's first word on both x86-64 and AArch64, wherever in the
    // frame the record sits. Every frame the program makes is below the
    // entry's, and the root walk must arrive at exactly this one: that is
    // what keeps it out of the frames of whatever called the program — a test
    // harness, `main`, a threading runtime, none of which can hold a Kite
    // reference — and what makes a broken chain a trap rather than a missed
    // root.
    //
    // SAFETY: this crate is built with frame pointers forced (see `build.rs`),
    // so `current_fp` is this function's frame record, and its first word is
    // readable stack memory holding the caller's frame pointer.
    unsafe {
        ENTRY_FP = *(current_fp() as *const usize);
        KITE_RT_DEPTH = 0;
    }
    unsafe {
        if !RT.is_null() {
            let old = Box::from_raw(RT);
            RT = std::ptr::null_mut();
            release(old);
        }
        // A harness's settings, when a harness started this run; otherwise
        // what a linked executable gets, and the environment may shrink the
        // nursery for anyone stress-testing the collector from outside.
        let (config, capture) = NEXT_RUN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .unwrap_or_default();
        let nursery_size = config
            .nursery_bytes
            .or_else(|| {
                std::env::var("KITE_NURSERY_BYTES")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(DEFAULT_NURSERY)
            .clamp(MIN_NURSERY, MAX_NURSERY);
        let threshold = config.major_threshold.unwrap_or(DEFAULT_THRESHOLD);
        let layout = Layout::from_size_align(nursery_size, 16)
            .expect("a nursery of at most a gigabyte is a valid layout");
        let nursery = sys_alloc(layout);
        if nursery.is_null() {
            trap(&format!("cannot allocate a nursery of {} bytes", nursery_size));
        }
        let capture = if capture { Some(Vec::new()) } else { None };
        RT = Box::into_raw(Box::new(Rt {
            nursery,
            nursery_size,
            nursery_top: 0,
            old: Vec::new(),
            old_bytes: 0,
            threshold,
            min_threshold: threshold,
            minor_collections: 0,
            major_collections: 0,
            remembered: Vec::new(),
            extra_roots: Vec::new(),
            stack_maps: Vec::new(),
            shapes: Shapes::default(),
            strings: Vec::new(),
            char_marks: Vec::new(),
            tasks: sched::Tasks::default(),
            clock: 0,
            wake_request: None,
            park_request: false,
            park_on_request: None,
            host_wait_request: false,
            capture,
        }));
    }
}

/// Free a run's heap. The capture buffer goes with the rest of the state, so
/// [`finish_run`] takes it out first.
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
    // There may be no run to have printed anything — a check can fire before
    // startup, under a unit test — and that is no reason to panic instead.
    if let Some(buf) = rt_if_started().and_then(|rt| rt.capture.take()) {
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
        // The VM's words, for the reason `MAX_FRAMES` gives.
        11 => trap(&format!("call depth exceeded {} frames", MAX_FRAMES)),
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
    let header = str_word0(bytes.len());
    // Straight into the old generation: a constant lives as long as the
    // program, and an immortal object in the nursery would be copied out on
    // the first collection anyway.
    let p = old_alloc(HEADER + round8(bytes.len()));
    unsafe {
        *(p as *mut u64) = header;
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

/// A host function's declared signature, in the encoding the host boundary
/// below describes.
///
/// # Safety
///
/// `ptr` must point at `len` readable bytes. The generated registration
/// function always passes a data symbol of exactly that length.
#[no_mangle]
pub unsafe extern "C" fn kite_rt_register_extern_sig(idx: u64, ptr: *const u8, len: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.extern_sigs, idx as usize + 1);
    rt.shapes.extern_sigs[idx as usize] = unsafe { bytes_arg(ptr, len) };
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
/// followed by the entries. Each entry is how far *below that function's own
/// frame pointer* a live reference is spilled — measured from the frame the
/// map describes, never from whatever frame it happens to have called, which
/// is what `stack_root_slots` relies on. The addresses are relocated
/// function addresses, which is why the table is data the code generator
/// emits rather than something serialised on the side.
///
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
        obj::SLICE => 8 * slice_cap(p),
        obj::MAP => 8 * map_words(map_room(p)),
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
        // The elements only: the slots past the length are unwritten room.
        obj::SLICE => {
            if aux as u8 == kind::REF {
                for i in 0..slice_len(p) {
                    f(slot(p, i));
                }
            }
        }
        // The used slots: a hole's are zero, which the collector passes over,
        // and the index after the entries holds no references at all.
        obj::MAP => {
            let key_ref = (aux & 0xFF) as u8 == kind::REF;
            let val_ref = ((aux >> 8) & 0xFF) as u8 == kind::REF;
            for i in 0..map_used(p) {
                if key_ref {
                    f(map_key(p, i));
                }
                if val_ref {
                    f(map_value(p, i));
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

/// The frame pointer of the compiled entry — the wrapper the code generator
/// emits, which calls `kite_rt_startup` first — recorded there. Zero when no
/// program is running, which makes the walk find nothing rather than guess.
static mut ENTRY_FP: usize = 0;

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

/// Walk the frame-pointer chain and gather every stack-map slot.
///
/// A frame record holds the caller's frame pointer and the return address
/// into the caller. When that return address is a registered safepoint, the
/// caller is a compiled Kite frame, and its map says how far below *its own*
/// frame pointer each live reference was spilled — so the slots are found
/// from the caller's frame pointer, which the record holds, and nothing is
/// assumed about the callee's frame at all. Frames whose return address is
/// not a registered safepoint — this crate's own frames, and every compiled
/// frame with nothing live — are skipped rather than scanned.
///
/// That independence is the point. The callee is usually a Rust function in
/// this crate, and where Rust puts its frame record inside its own frame is
/// the platform's business: at the top on x86-64 and Apple AArch64, but on
/// AArch64 Linux *below* the callee-saved registers, so the caller's stack
/// pointer is the record plus 16 plus however many registers that function
/// happened to save. An earlier version of this walk assumed "record plus 16"
/// everywhere, which read every slot 32 to 80 bytes too low on AArch64 Linux —
/// updating saved registers as if they were references, and missing the real
/// ones. A compiled frame's layout, by contrast, is Cranelift's and is known
/// when the code is generated: its frame record sits at the top of the frame,
/// and the distance from it to each spill slot is what the code generator
/// registers.
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
/// So the chain is checked rather than trusted, and a chain that fails a
/// check is a trap. The walk must climb — a repeated or descending pointer is
/// not a frame — and it must arrive at *exactly* the entry's frame, which
/// `kite_rt_startup` recorded; a chain that jumps past it or ends short of it
/// broke somewhere, and a collection that carried on would be a collection
/// with roots missing, which is a use-after-free. There is no cap on the
/// number of frames: the chain is bounded by the entry, and a deep recursion
/// is exactly where a program holds the most references on its stack.
fn stack_root_slots() -> Vec<*mut u64> {
    let entry = unsafe { ENTRY_FP };
    let mut slots = Vec::new();
    // Nothing has started, so nothing is rooted. Better to say that than to
    // walk an unbounded chain.
    if entry == 0 {
        return slots;
    }
    let maps = &rt().stack_maps;
    // SAFETY: the walk starts at this frame, and `walk_frames` only reads a
    // frame record whose address is above the previous one and below
    // `entry` — memory between this frame and the program's entry, which is
    // this thread's own live stack.
    let walked = unsafe { walk_frames(current_fp(), entry, maps, &mut |s| slots.push(s)) };
    if let Err(why) = walked {
        trap(&format!(
            "the collector could not find the program's roots: {}\n\
             note: this is a bug in the native backend, not in the program",
            why
        ));
    }
    slots
}

/// The walk itself, over any chain of frame records — separated from where
/// the chain comes from so the arithmetic can be tested against a stack laid
/// out the way a platform this crate cannot run on lays it out.
///
/// `maps` is sorted by return address, each entry a distance below the
/// calling frame's frame pointer.
///
/// # Safety
///
/// Every address in `[fp, entry)` must be readable, and `fp` must be a frame
/// record or `entry` itself. The walk reads only records strictly above the
/// previous one and strictly below `entry`, so it stays inside that range.
unsafe fn walk_frames(
    mut fp: usize,
    entry: usize,
    maps: &[(usize, Vec<u32>)],
    visit: &mut dyn FnMut(*mut u64),
) -> Result<(), String> {
    while fp != entry {
        if fp == 0 || fp & 7 != 0 || fp > entry {
            return Err(format!(
                "the frame-pointer chain left the stack at {:#x}, short of the entry at {:#x}",
                fp, entry
            ));
        }
        let ret = *((fp + 8) as *const usize);
        let caller_fp = *(fp as *const usize);
        // The chain must climb; a repeated or descending pointer means the
        // walk has left well-formed frames behind.
        if caller_fp <= fp {
            return Err(format!(
                "the frame-pointer chain stopped climbing at {:#x}, short of the entry at {:#x}",
                fp, entry
            ));
        }
        if let Ok(i) = maps.binary_search_by_key(&ret, |(pc, _)| *pc) {
            for below in &maps[i].1 {
                let slot = caller_fp.wrapping_sub(*below as usize);
                // The caller's frame is everything from its stack pointer up
                // to its frame record, and the callee's record lies below that
                // stack pointer — so a slot is above the callee's record and
                // below the caller's. A slot anywhere else means the map and
                // the frame disagree, and visiting it would be writing to a
                // word that is not a reference.
                if slot < fp + 16 || slot >= caller_fp {
                    return Err(format!(
                        "a stack map put a slot at {:#x}, outside the frame between {:#x} and {:#x}",
                        slot, fp, caller_fp
                    ));
                }
                visit(slot as *mut u64);
            }
        }
        fp = caller_fp;
    }
    Ok(())
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
    rt().minor_collections += 1;
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
    for s in rt().tasks.polls_mut() {
        evac_slot(s as *mut u64, &mut queue);
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
    rt().char_marks.retain(|(p, _)| !in_nursery(*p));

    if rt().old_bytes > rt().threshold {
        collect_major();
    }
}

/// A major collection: mark from the same roots, sweep the old generation.
/// Runs only immediately after a minor collection, when the nursery is empty,
/// so everything reachable is old and nothing moves — the old generation is
/// non-moving by design, which is what spares the runtime a read barrier.
fn collect_major() {
    rt().major_collections += 1;
    rt().char_marks.clear();
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
    for s in rt().tasks.polls_mut() {
        mark_slot(s as *mut u64, &mut stack);
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
    rt_.threshold = (2 * live_bytes).max(rt_.min_threshold);
}

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

/// A slice of the `argc` staged elements, with room for `cap`.
///
/// The room is for a literal longer than the staging window, which the code
/// generator builds a window at a time: the whole length is allocated here,
/// and [`kite_rt_slice_extend`] fills the rest in place.
#[no_mangle]
pub extern "C" fn kite_rt_slice_new(elem_kind: u64, argc: u64, cap: u64) -> u64 {
    let argc = argc as usize;
    let n = root_stage(argc, |_| elem_kind as u8 == kind::REF);
    let p = slice_alloc(elem_kind as u32, argc, (cap as usize).max(argc));
    unroot(n);
    unsafe { fill_from_stage(p, argc) };
    p as u64
}

/// Append the `argc` staged elements to a slice the compiled code has just
/// made and nothing else has seen — the next window of a literal too long for
/// one. In place when the room is there, which it is when [`kite_rt_slice_new`]
/// was asked for the literal's length; a copy with enough room otherwise.
#[no_mangle]
pub extern "C" fn kite_rt_slice_extend(s: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let mut s = s;
    unsafe {
        let len = slice_len(s as *const u8);
        let aux = obj_aux(s as *const u8);
        let elem_ref = aux as u8 == kind::REF;
        if len + argc > slice_cap(s as *const u8) {
            root(&mut s);
            let n = root_stage(argc, |_| elem_ref);
            let p = slice_alloc(aux, len, len + argc);
            unroot(n + 1);
            std::ptr::copy_nonoverlapping(slot(s as *const u8, 0), slot(p, 0), len);
            s = p as u64;
        }
        let p = s as *mut u8;
        for i in 0..argc {
            *slot(p, len + i) = *stage_slot(i);
        }
        set_slice_len(p, len + argc);
        if elem_ref {
            remember(p);
        }
    }
    s
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

// ---------------------------------------------------------------------------
// Maps
// ---------------------------------------------------------------------------
//
// A map is one object: its entries in insertion order, and — past
// [`MAP_SMALL`] of room — a hash index after them.
//
//   word 1       the slots used (low half) and the room (high half)
//   slot 0       how many of the used slots are entries
//   slots 1..    `room` pairs of key and value
//   then         `2 * room` bucket heads of 32 bits, and per slot one word:
//                its key's hash (low half) and the next slot in its bucket
//
// The index holds hashes and slot numbers, never an address, so it means the
// same thing wherever the object is: the nursery's evacuation copies it with
// the entries and it is still right, and so is a copy made for a write. The
// hash is of a key's *content*, which is why that holds — see [`key_hash`].
//
// The entries are what the map is. Iteration, `keys()`, `==`, rendering and
// the collector read them and nothing else; the index only says where a key
// is. A removal from an indexed map leaves a hole — its slots zeroed, so the
// collector sees nothing there, and its index word marked — so that removing
// costs a constant rather than moving every later entry down. Everything that
// reads the entries skips the holes, so what remains is in the order it was,
// which is exactly what moving the entries down gave; once the holes
// outnumber the entries the map closes up and lays its index out again.
//
// A map is a value, and a write goes straight into it only when the local
// that holds it is the only thing that can see it — the owned flag the code
// generator keeps for slices, kept for maps too (`kite-codegen-clif`'s
// `slices` module). Otherwise the write lands in a copy, which the local then
// owns. With room to spare and an index, a loop of inserts is linear, where
// copying the map for every one of them, and scanning it, made it quadratic.
//
// The VM's map (`kite-vm`'s `map` module) is the specification for all of
// it: the hash agrees with `==`, a struct with a `var` field hashes by its
// type alone, and each bucket lists its slots in insertion order so that of
// two equal keys — which only a `var` field can make — the earlier wins.

/// The most room a map has with no index. A scan of this many is as fast as
/// hashing.
const MAP_SMALL: usize = 8;

/// An empty bucket, or the end of one.
const MAP_NONE: u32 = u32::MAX;

/// The "next" of a slot that is a hole.
const MAP_HOLE: u32 = u32::MAX - 1;

#[inline]
unsafe fn map_used(p: *const u8) -> usize {
    (obj_word1(p) & 0xFFFF_FFFF) as usize
}

#[inline]
unsafe fn map_room(p: *const u8) -> usize {
    (obj_word1(p) >> 32) as usize
}

#[inline]
unsafe fn set_map_used(p: *mut u8, used: usize) {
    *(p as *mut u64).add(1) = used as u64 | ((map_room(p) as u64) << 32);
}

#[inline]
unsafe fn map_live(p: *const u8) -> usize {
    *slot(p, 0) as usize
}

#[inline]
unsafe fn set_map_live(p: *mut u8, live: usize) {
    *slot(p, 0) = live as u64;
}

#[inline]
unsafe fn map_key(p: *const u8, i: usize) -> *mut u64 {
    slot(p, 1 + 2 * i)
}

#[inline]
unsafe fn map_value(p: *const u8, i: usize) -> *mut u64 {
    slot(p, 2 + 2 * i)
}

#[inline]
fn map_indexed(room: usize) -> bool {
    room > MAP_SMALL
}

/// The payload words of a map with room for `room` entries.
fn map_words(room: usize) -> usize {
    1 + 2 * room + if map_indexed(room) { 2 * room } else { 0 }
}

/// The bucket heads: `2 * room` of them, a power of two.
#[inline]
unsafe fn map_heads(p: *const u8) -> *mut u32 {
    slot(p, 1 + 2 * map_room(p)) as *mut u32
}

/// Slot `i`'s index word: its key's hash, and the next slot in its bucket.
#[inline]
unsafe fn map_link_word(p: *const u8, i: usize) -> *mut u64 {
    slot(p, 1 + 3 * map_room(p) + i)
}

#[inline]
unsafe fn map_bucket(p: *const u8, h: u32) -> *mut u32 {
    map_heads(p).add(h as usize & (2 * map_room(p) - 1))
}

#[inline]
unsafe fn map_is_hole(p: *const u8, i: usize) -> bool {
    map_indexed(map_room(p)) && (*map_link_word(p, i) >> 32) as u32 == MAP_HOLE
}

/// The slots holding entries, in insertion order.
unsafe fn map_entries(p: *const u8) -> impl DoubleEndedIterator<Item = usize> {
    let holes = map_used(p) != map_live(p);
    (0..map_used(p)).filter(move |&i| !holes || !map_is_hole(p, i))
}

/// The room a full map of `room` is given: twice as much, a power of two, so
/// that a loop of inserts copies each entry a constant number of times. It
/// once took twice the entries plus the new one, rounded up — which for a
/// full map of a power of two is four times the room, and with the index
/// sixteen words an entry.
fn map_room_for_growth(room: usize) -> usize {
    (2 * room).next_power_of_two().max(MAP_SMALL)
}

/// Exactly enough room for `n` entries, as a literal needs: an indexed map's
/// room is a power of two.
fn map_room_for(n: usize) -> usize {
    if map_indexed(n) {
        n.next_power_of_two()
    } else {
        n
    }
}

/// A map with room for `room` entries and none in it, of the element kinds
/// `aux` packs. May collect, so the caller roots what it holds.
fn map_alloc(aux: u32, room: usize) -> *mut u8 {
    if room > u32::MAX as usize / 2 {
        trap(&format!("a map cannot hold more than {} entries", u32::MAX / 2));
    }
    let p = alloc(HEADER + 8 * map_words(room));
    unsafe {
        *(p as *mut u64) = word0(obj::MAP, aux);
        *(p as *mut u64).add(1) = (room as u64) << 32;
        set_map_live(p, 0);
        if map_indexed(room) {
            std::ptr::write_bytes(map_heads(p), 0xFF, 2 * room);
        }
    }
    p
}

/// Fill the empty map `p` with `src`'s entries. Into the same room it is the
/// whole of `src` as it is, holes and index included, so slot numbers mean
/// what they meant; into other room the holes are closed up and the index,
/// if `p` has one, laid out afresh. Allocates nothing.
unsafe fn map_fill_from(p: *mut u8, src: *const u8) {
    let room = map_room(p);
    if room == map_room(src) {
        std::ptr::copy_nonoverlapping(slot(src, 0), slot(p, 0), map_words(room));
        set_map_used(p, map_used(src));
        return;
    }
    let indexed = map_indexed(room);
    let known = map_indexed(map_room(src));
    let mut n = 0;
    for i in map_entries(src) {
        *map_key(p, n) = *map_key(src, i);
        *map_value(p, n) = *map_value(src, i);
        if indexed && known {
            *map_link_word(p, n) = *map_link_word(src, i) & 0xFFFF_FFFF;
        }
        n += 1;
    }
    set_map_used(p, n);
    set_map_live(p, n);
    if indexed {
        let kk = (obj_aux(p) & 0xFF) as u8;
        map_relink(p, if known { None } else { Some(kk) });
    }
}

/// Lay the index of `p`, which has no holes, out afresh: from the hashes its
/// index words already hold, or, given the key kind, from the keys. Walking
/// the slots from the back and putting each at the front of its bucket leaves
/// every bucket in insertion order.
unsafe fn map_relink(p: *mut u8, rehash: Option<u8>) {
    std::ptr::write_bytes(map_heads(p), 0xFF, 2 * map_room(p));
    for i in (0..map_used(p)).rev() {
        let h = match rehash {
            Some(kk) => key_hash(*map_key(p, i), kk),
            None => *map_link_word(p, i) as u32,
        };
        let head = map_bucket(p, h);
        *map_link_word(p, i) = h as u64 | ((*head as u64) << 32);
        *head = i as u32;
    }
}

/// Where `key` is among `p`'s entries — the earliest, should two hold equal
/// keys. For an indexed map, also the last slot of the key's bucket (where a
/// new entry is linked; [`MAP_NONE`] for an empty bucket) and the key's hash.
///
/// A map with no index is scanned, and a key that is a number is compared as
/// one, in a loop the compiler can see through, rather than through a call to
/// [`value_eq`] per entry that asks the key's kind each time.
unsafe fn map_find(p: *const u8, key: u64, kk: u8) -> (Option<usize>, u32, u32) {
    if !map_indexed(map_room(p)) {
        let used = map_used(p);
        let found = match kk {
            kind::REF => (0..used).find(|&i| value_eq(*map_key(p, i), key, kind::REF)),
            kind::FLOAT => {
                let key = f64::from_bits(key);
                (0..used).find(|&i| f64::from_bits(*map_key(p, i)) == key)
            }
            _ => (0..used).find(|&i| *map_key(p, i) == key),
        };
        return (found, MAP_NONE, 0);
    }
    let h = key_hash(key, kk);
    let mut i = *map_bucket(p, h);
    let mut last = MAP_NONE;
    while i != MAP_NONE {
        let link = *map_link_word(p, i as usize);
        if link as u32 == h && value_eq(*map_key(p, i as usize), key, kk) {
            return (Some(i as usize), last, h);
        }
        last = i;
        i = (link >> 32) as u32;
    }
    (None, last, h)
}

/// Put a new entry in the next slot of `p`, which has room for it; `h` and
/// `last` are what [`map_find`] answered for its key. No write barrier: the
/// caller decides that.
unsafe fn map_push(p: *mut u8, key: u64, value: u64, h: u32, last: u32) {
    let at = map_used(p);
    *map_key(p, at) = key;
    *map_value(p, at) = value;
    if map_indexed(map_room(p)) {
        *map_link_word(p, at) = h as u64 | ((MAP_NONE as u64) << 32);
        if last == MAP_NONE {
            *map_bucket(p, h) = at as u32;
        } else {
            let w = map_link_word(p, last as usize);
            *w = (*w & 0xFFFF_FFFF) | ((at as u64) << 32);
        }
    }
    set_map_used(p, at + 1);
    set_map_live(p, map_live(p) + 1);
}

/// Take the entry in slot `i` out of `p`, in place.
unsafe fn map_take(p: *mut u8, i: usize) {
    let used = map_used(p);
    let live = map_live(p) - 1;
    set_map_live(p, live);
    if !map_indexed(map_room(p)) {
        // The entries after it move down by one: what keeps insertion order
        // meaning what it says.
        std::ptr::copy(map_key(p, i + 1), map_key(p, i), 2 * (used - i - 1));
        set_map_used(p, used - 1);
        return;
    }
    let link = *map_link_word(p, i);
    let after = (link >> 32) as u32;
    let head = map_bucket(p, link as u32);
    if *head == i as u32 {
        *head = after;
    } else {
        let mut j = *head;
        loop {
            let w = map_link_word(p, j as usize);
            let next = (*w >> 32) as u32;
            if next == i as u32 {
                *w = (*w & 0xFFFF_FFFF) | ((after as u64) << 32);
                break;
            }
            j = next;
        }
    }
    *map_link_word(p, i) = (link & 0xFFFF_FFFF) | ((MAP_HOLE as u64) << 32);
    *map_key(p, i) = 0;
    *map_value(p, i) = 0;
    let holes = used - live;
    if holes > MAP_SMALL && holes > live {
        // Close up in place, keeping each entry's hash, and relink.
        let mut n = 0;
        for j in 0..used {
            if (*map_link_word(p, j) >> 32) as u32 == MAP_HOLE {
                continue;
            }
            *map_key(p, n) = *map_key(p, j);
            *map_value(p, n) = *map_value(p, j);
            *map_link_word(p, n) = *map_link_word(p, j);
            n += 1;
        }
        set_map_used(p, n);
        map_relink(p, None);
    }
}

/// Whether a map of the element kinds `aux` packs holds references, which a
/// write into an old one must tell the collector about.
fn map_holds_refs(aux: u32) -> bool {
    (aux & 0xFF) as u8 == kind::REF || ((aux >> 8) & 0xFF) as u8 == kind::REF
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
    // Room for every staged pair; duplicates leave some unused.
    let aux = (key_kind as u32 & 0xFF) | ((val_kind as u32 & 0xFF) << 8);
    let p = map_alloc(aux, map_room_for(pairs));
    unroot(n);
    unsafe { insert_staged(p, pairs, key_kind as u8) };
    p as u64
}

/// The next window of a map literal too long for one: a copy of `m` with the
/// `argc` staged words — alternating keys and values, as for
/// [`kite_rt_map_new`] — added by the same rule, so a key repeated across two
/// windows is one entry exactly as it is within one.
#[no_mangle]
pub extern "C" fn kite_rt_map_extend(m: u64, argc: u64) -> u64 {
    let argc = argc as usize;
    let pairs = argc / 2;
    let mut m = m;
    unsafe {
        let aux = obj_aux(m as *const u8);
        let key_kind = (aux & 0xFF) as u8;
        let val_kind = ((aux >> 8) & 0xFF) as u8;
        root(&mut m);
        let n = root_stage(argc, |i| {
            (if i % 2 == 0 { key_kind } else { val_kind }) == kind::REF
        });
        let p = map_alloc(aux, map_room_for(map_live(m as *const u8) + pairs));
        unroot(n + 1);
        map_fill_from(p, m as *const u8);
        insert_staged(p, pairs, key_kind);
        p as u64
    }
}

/// Add `pairs` staged key-value pairs to the map at `p`, which has room for
/// all of them. A key already present keeps its position and takes the later
/// value — the VM's rule for a literal that names a key twice. `p` is the
/// newest object there is, so no barrier is needed: if it is old, it was
/// born remembered (see [`alloc`]).
unsafe fn insert_staged(p: *mut u8, pairs: usize, key_kind: u8) {
    for e in 0..pairs {
        let k = *stage_slot(2 * e);
        let v = *stage_slot(2 * e + 1);
        match map_find(p, k, key_kind) {
            (Some(i), _, _) => *map_value(p, i) = v,
            (None, last, h) => map_push(p, k, v, h, last),
        }
    }
}

/// Record that struct `id` has a `var` field, so a map hashes it by its type
/// alone: the field can change while the struct is a key.
#[no_mangle]
pub extern "C" fn kite_rt_register_mutable_struct(id: u64) {
    let rt = rt();
    ensure_len(&mut rt.shapes.mutable_structs, id as usize + 1);
    rt.shapes.mutable_structs[id as usize] = true;
}

/// A key's hash, from its content alone and never an address — the objects
/// it is made of move — and equal for keys `==` calls equal.
///
/// The VM's rules: `-0.0` hashes as `0.0`, an error by its message, and a
/// struct with a `var` field by its type, since the field can change while
/// the struct is a key. All of the key is looked at, in an order equal keys
/// share, with a worklist rather than recursion: keys that differ only deep
/// inside would otherwise share a bucket (see the VM's `map` module). The
/// walk ends because a key is cyclic only through a `var` field, which is not
/// looked inside. Allocates nothing on the Kite heap.
fn key_hash(key: u64, kk: u8) -> u32 {
    let mut h = KeyHasher(0);
    let mut work: Vec<(u64, u8)> = Vec::new();
    let mut next = Some((key, kk));
    while let Some((v, k)) = next.take().or_else(|| work.pop()) {
        match k {
            kind::FLOAT => {
                h.word(if f64::from_bits(v) == 0.0 { 0 } else { v });
                continue;
            }
            kind::REF => {}
            _ => {
                h.word(v);
                continue;
            }
        }
        if v == 0 {
            h.word(1);
            continue;
        }
        unsafe {
            let p = v as *const u8;
            let aux = obj_aux(p);
            // Children go on in reverse so they are hashed in order.
            let fields = |kinds: &[u8], work: &mut Vec<(u64, u8)>| {
                for (i, k) in kinds.iter().enumerate().rev() {
                    work.push((*slot(p, i), *k));
                }
            };
            match obj_kind(p) {
                obj::STR => h.bytes(str_bytes(v)),
                obj::STRUCT => {
                    h.word(5);
                    h.word(aux as u64);
                    let shapes = &rt().shapes;
                    if !shapes.mutable_structs.get(aux as usize).copied().unwrap_or(false) {
                        fields(&shapes.structs[aux as usize], &mut work);
                    }
                }
                obj::ENUM => {
                    h.word(6);
                    h.word(aux as u64);
                    h.word(obj_word1(p));
                    let variant = (obj_word1(p) & 0xFFFF_FFFF) as usize;
                    fields(&rt().shapes.enums[aux as usize][variant], &mut work);
                }
                obj::TUPLE => {
                    h.word(7);
                    fields(&rt().shapes.tuples[aux as usize], &mut work);
                }
                obj::SLICE => {
                    h.word(8);
                    h.word(slice_len(p) as u64);
                    for i in (0..slice_len(p)).rev() {
                        work.push((*slot(p, i), aux as u8));
                    }
                }
                obj::MAP => {
                    h.word(9);
                    h.word(map_live(p) as u64);
                    let (kk, vk) = ((aux & 0xFF) as u8, ((aux >> 8) & 0xFF) as u8);
                    for i in map_entries(p).rev() {
                        work.push((*map_value(p, i), vk));
                        work.push((*map_key(p, i), kk));
                    }
                }
                obj::PAIR => {
                    work.push((*slot(p, 1), kind::REF));
                    work.push((*slot(p, 0), aux as u8));
                }
                // Two errors are equal when their messages are.
                obj::ERR => work.push((*slot(p, 0), kind::REF)),
                obj::BOX => work.push((*slot(p, 0), aux as u8)),
                _ => h.word(4),
            }
        }
    }
    h.finish() as u32
}

/// The VM's hasher: FxHash's step with a finishing mix, so the low bits a
/// bucket is chosen by depend on all of the input.
struct KeyHasher(u64);

impl KeyHasher {
    #[inline]
    fn word(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        let mut chunks = b.chunks_exact(8);
        for c in &mut chunks {
            self.word(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let mut tail = [0u8; 8];
        let rest = chunks.remainder();
        tail[..rest.len()].copy_from_slice(rest);
        self.word(u64::from_le_bytes(tail));
    }

    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }
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
    // Nothing carried is nil, never a present optional holding a null: a box
    // around zero passes the nil test and then faults on the first field
    // read. No type's tag is zero, so a match with nothing in the slot cannot
    // happen for a well-formed error — this is the backstop for one that is
    // not.
    if value == 0 {
        return 0;
    }
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

/// A `var` field write: one of the two in-place heap mutations the language
/// has. The other is a write into an owned slice, below; both store through
/// [`remember`].
#[no_mangle]
pub extern "C" fn kite_rt_set_field(base: u64, index: u64, value: u64, is_ref: u64) {
    let p = base as *mut u8;
    unsafe {
        *slot(p, index as usize) = value;
        if is_ref != 0 {
            remember(p);
        }
    }
}

/// The write barrier, for an object that has just had a reference stored into
/// it in place: an old one joins the remembered set, because it may now be
/// the only path to a nursery object and a minor collection does not trace
/// the old generation.
///
/// The bit is how a second store into the same object stays cheap, so it
/// must never be set on an object that is not in the set — which is why a
/// copy of a slice writes a fresh first header word rather than copying the
/// original's, collector bits and all.
unsafe fn remember(p: *mut u8) {
    if !in_nursery(p as u64) && *(p as *const u64) & REMEMBERED_BIT == 0 {
        *(p as *mut u64) |= REMEMBERED_BIT;
        rt().remembered.push(p);
    }
}

unsafe fn slice_parts(s: u64) -> (*mut u8, usize) {
    let p = s as *mut u8;
    (p, slice_len(p))
}

/// A copy of the slice `s`, `len` elements long with room for `cap`. `s` and
/// `value` are rooted across the allocation, so the answer is the copy and
/// `value` as it is after it.
unsafe fn slice_copy(s: u64, value: u64, len: usize, cap: usize) -> (*mut u8, u64) {
    let (mut s, mut value) = (s, value);
    let elem_ref = obj_aux(s as *const u8) as u8 == kind::REF;
    root(&mut s);
    if elem_ref {
        root(&mut value);
    }
    let p = slice_alloc(obj_aux(s as *const u8), len, cap);
    unroot(if elem_ref { 2 } else { 1 });
    std::ptr::copy_nonoverlapping(slot(s as *const u8, 0), slot(p, 0), len);
    (p, value)
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

/// `xs[i] = v`, answering the slice the local keeps.
///
/// Slices are values, and `owned` is how the code generator says whether this
/// one can be written where it stands: nonzero when nothing but the local
/// being written can reach the object (see `kite-codegen-clif`'s `slices`
/// module). Then the element is stored in place. Otherwise the slice is
/// copied — exactly as long as it is, since a write does not suggest a push
/// to follow — and the copy written, which is the VM's `Rc::make_mut` answer
/// without a count to ask. The index is checked first either way, as the VM
/// checks it before its `make_mut`.
#[no_mangle]
pub extern "C" fn kite_rt_set_index(s: u64, index: i64, value: u64, owned: u8) -> u64 {
    unsafe {
        let (p, len) = slice_parts(s);
        let Some(i) = usize::try_from(index).ok().filter(|u| *u < len) else {
            trap(&format!(
                "index {} is out of range for a slice of length {}",
                index, len
            ));
        };
        let elem_ref = obj_aux(p) as u8 == kind::REF;
        let (p, value) = if owned != 0 { (p, value) } else { slice_copy(s, value, len, len) };
        *slot(p, i) = value;
        if elem_ref {
            remember(p);
        }
        p as u64
    }
}

/// `xs.push(v)`, answering the slice the local keeps.
///
/// With `owned` (as for [`kite_rt_set_index`]) and room to spare, the value
/// goes into the next slot and the length moves, with nothing allocated.
/// Otherwise the elements move to a new object twice as long plus four — 4,
/// 12, 28, … from empty — which the local then owns. The doubling is what
/// makes a loop of pushes linear: this copied the whole slice on every push
/// once, and a hundred thousand pushes took four seconds. A snapshot someone
/// else holds is never written, which is what keeps a slice a value.
#[no_mangle]
pub extern "C" fn kite_rt_slice_push(s: u64, value: u64, owned: u8) -> u64 {
    unsafe {
        let (p, len) = slice_parts(s);
        let elem_ref = obj_aux(p) as u8 == kind::REF;
        let (p, value) = if owned != 0 && len < slice_cap(p) {
            (p, value)
        } else {
            if len >= SLICE_MAX {
                trap(&format!("a slice cannot hold more than {} elements", SLICE_MAX));
            }
            slice_copy(s, value, len, (2 * len + 4).min(SLICE_MAX))
        };
        *slot(p, len) = value;
        set_slice_len(p, len + 1);
        if elem_ref {
            remember(p);
        }
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_slice_len(s: u64) -> i64 {
    unsafe { slice_len(s as *const u8) as i64 }
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
        // A fresh header with the original's element kind, which the copy
        // must keep for the collector to trace it at all — but not the
        // original's first word whole: its collector bits are not the copy's
        // (see `remember`).
        let p = slice_alloc(obj_aux(s as *const u8), count, count);
        unroot(1);
        std::ptr::copy_nonoverlapping(slot(s as *const u8, lo as usize), slot(p, 0), count);
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
    unsafe { map_live(m as *const u8) as i64 }
}

#[no_mangle]
pub extern "C" fn kite_rt_map_get(m: u64, key: u64, key_kind: u64, wrap_kind: i64) -> u64 {
    unsafe {
        let p = m as *mut u8;
        match map_find(p, key, key_kind as u8).0 {
            Some(i) => {
                let v = *map_value(p, i);
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

/// `m[key] = value`, answering the map the local keeps: an existing key
/// keeps its position and its stored key, a new one is appended — the VM's
/// rule, verbatim.
///
/// With `owned` — the code generator's word that nothing but the local can
/// see this map, as for [`kite_rt_slice_push`] — and room to spare, the write
/// goes straight in. Otherwise it goes into a copy, with twice the room when
/// the map was full, which the local then owns.
#[no_mangle]
pub extern "C" fn kite_rt_map_set(m: u64, key: u64, key_kind: u64, value: u64, owned: u8) -> u64 {
    let (mut m, mut key, mut value) = (m, key, value);
    let kk = key_kind as u8;
    unsafe {
        let aux = obj_aux(m as *const u8);
        let (found, last, h) = map_find(m as *const u8, key, kk);
        let (used, room) = (map_used(m as *const u8), map_room(m as *const u8));
        let p = if owned != 0 && (found.is_some() || used < room) {
            m as *mut u8
        } else {
            let grown = if found.is_none() && used == room {
                map_room_for_growth(room)
            } else {
                room
            };
            let key_ref = (aux & 0xFF) as u8 == kind::REF;
            let val_ref = ((aux >> 8) & 0xFF) as u8 == kind::REF;
            root(&mut m);
            if key_ref {
                root(&mut key);
            }
            if val_ref {
                root(&mut value);
            }
            let p = map_alloc(aux, grown);
            unroot(1 + usize::from(key_ref) + usize::from(val_ref));
            map_fill_from(p, m as *const u8);
            p
        };
        // A copy into other room has moved the entries; look again. The
        // hash is of the key's content, so it held across the collection the
        // copy may have run, but the slot numbers do not.
        let (found, last, h) = if map_room(p) == room { (found, last, h) } else { map_find(p, key, kk) };
        match found {
            Some(i) => *map_value(p, i) = value,
            None => map_push(p, key, value, h, last),
        }
        if map_holds_refs(aux) {
            remember(p);
        }
        p as u64
    }
}

/// `m.remove(key)`, answering the map the local keeps: the map itself when
/// the key is not there — a miss changes nothing, identity included — and
/// otherwise the map without that entry, in place when `owned` says so (as
/// for [`kite_rt_map_set`]) and in a copy when not. On a miss the code
/// generator leaves its owned flag as it was, since the answer is the map
/// the flag already described.
#[no_mangle]
pub extern "C" fn kite_rt_map_remove(m: u64, key: u64, key_kind: u64, owned: u8) -> u64 {
    let mut m = m;
    unsafe {
        let Some(i) = map_find(m as *const u8, key, key_kind as u8).0 else {
            return m;
        };
        let p = if owned != 0 {
            m as *mut u8
        } else {
            root(&mut m);
            let p = map_alloc(obj_aux(m as *const u8), map_room(m as *const u8));
            unroot(1);
            // Into the same room, so slot `i` is still the entry.
            map_fill_from(p, m as *const u8);
            p
        };
        map_take(p, i);
        p as u64
    }
}

/// A map's keys or values as a slice, in insertion order, so the two line up
/// element for element.
fn map_side(m: u64, values: bool) -> u64 {
    let mut m = m;
    unsafe {
        let len = map_live(m as *const u8);
        let aux = obj_aux(m as *const u8);
        let elem_kind = if values { (aux >> 8) & 0xFF } else { aux & 0xFF };
        root(&mut m);
        let p = slice_alloc(elem_kind, len, len);
        unroot(1);
        let m = m as *const u8;
        for (n, i) in map_entries(m).enumerate() {
            *slot(p, n) = if values { *map_value(m, i) } else { *map_key(m, i) };
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

/// How many characters a string holds — counted once, then remembered.
///
/// A string's second header word is that memory: zero when the string is
/// made (every constructor below writes it so), the count plus one once
/// something has asked. Nothing else reads the word — the collector sizes a
/// string from its byte length and copies the whole header when it moves
/// one — and a string's bytes never change, so the count cannot go stale.
/// Without it a parser's `input.slice(i, i + 1)` counted the whole input to
/// clamp `i`, on every character.
unsafe fn str_chars(s: u64) -> usize {
    let word1 = (s as *mut u64).add(1);
    if *word1 != 0 {
        return (*word1 - 1) as usize;
    }
    let n = str_str(s).chars().count();
    *word1 = n as u64 + 1;
    n
}

/// Record a count the caller already knows, so the string is never walked
/// to find it.
unsafe fn str_set_chars(s: u64, n: usize) {
    *(s as *mut u64).add(1) = n as u64 + 1;
}

/// The byte offset at which character `c` starts, for `c` at most the
/// string's length in characters.
///
/// A string with as many characters as bytes is ASCII, so there the answer
/// is `c` itself and indexing is constant time — the common case for the
/// text a parser reads. A long string that is not ASCII has the start of
/// every [`CHAR_STRIDE`]-th character found once, as the VM's has, so no
/// more than that many are walked; a short one is walked from the front.
unsafe fn str_byte_at(s: u64, c: usize) -> usize {
    let text = str_str(s);
    if str_chars(s) == text.len() {
        return c;
    }
    let (from, skip) = if text.len() > CHAR_STRIDE {
        let marks = str_marks(s);
        // The last mark, for `c == chars` on a whole number of strides.
        let j = (c / CHAR_STRIDE).min(marks.len() - 1);
        (marks[j] as usize, c - j * CHAR_STRIDE)
    } else {
        (0, c)
    };
    text[from..].char_indices().nth(skip).map_or(text.len(), |(b, _)| from + b)
}

const CHAR_STRIDE: usize = 64;
const CHAR_MARKS_KEPT: usize = 4;

/// The marks of a long string that is not ASCII, found in `Rt::char_marks`
/// or made there: `marks[j]` is where character `j * CHAR_STRIDE` starts.
/// Nothing here allocates on the Kite heap, so `s` cannot move meanwhile.
unsafe fn str_marks<'a>(s: u64) -> &'a [u32] {
    let rt = rt();
    // Kept in the order last used, and the one used longest ago is the one
    // let go: a loop that reads a long input and, between reads, each long
    // piece it cut from it would otherwise push the input out every few
    // pieces and index it afresh, which is quadratic again.
    if let Some(at) = rt.char_marks.iter().position(|(p, _)| *p == s) {
        let hit = rt.char_marks.remove(at);
        rt.char_marks.push(hit);
    } else {
        let marks: Vec<u32> = str_str(s)
            .char_indices()
            .step_by(CHAR_STRIDE)
            .map(|(b, _)| b as u32)
            .collect();
        if rt.char_marks.len() == CHAR_MARKS_KEPT {
            rt.char_marks.remove(0);
        }
        rt.char_marks.push((s, marks));
    }
    &rt.char_marks[rt.char_marks.len() - 1].1
}

/// How many bytes the `k` characters starting at byte `start` take — so a
/// slice walks to its start once, not to each end from the front.
unsafe fn str_byte_at_from(s: u64, start: usize, k: usize) -> usize {
    let text = str_str(s);
    if str_chars(s) == text.len() {
        return k;
    }
    let rest = &text[start..];
    rest.char_indices().nth(k).map(|(b, _)| b).unwrap_or(rest.len())
}

/// A string's length as its header records it, or `None` past what 32 bits
/// can say.
fn str_len_field(len: usize) -> Option<u32> {
    u32::try_from(len).ok()
}

/// The first header word of a string of `len` bytes — or a trap, before
/// anything is allocated, when the length does not fit.
///
/// The length lives in the header's upper 32 bits, and the collector sizes
/// the object from it. A truncated length would not be a shorter string: it
/// would be an object the collector copies short and the renderer reads
/// short, with the rest of the bytes still sitting in memory the allocator
/// believes is free. The VM has no such limit, so a program can reach it only
/// here, and saying so is the honest answer.
fn str_word0(len: usize) -> u64 {
    match str_len_field(len) {
        Some(n) => word0(obj::STR, n),
        None => trap(&format!(
            "string too long: {} bytes, and a native string holds at most {}",
            len,
            u32::MAX
        )),
    }
}

fn make_str(bytes: &[u8]) -> u64 {
    // The bytes must not point into the heap — every caller below builds them
    // in Rust-owned memory first, precisely so this allocation cannot move
    // its own input.
    let header = str_word0(bytes.len());
    let p = alloc(HEADER + round8(bytes.len()));
    unsafe {
        *(p as *mut u64) = header;
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
        let header = str_word0(total);
        root(&mut a);
        root(&mut b);
        let p = alloc(HEADER + round8(total));
        unroot(2);
        let (abytes, bbytes) = (str_bytes(a), str_bytes(b));
        *(p as *mut u64) = header;
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(abytes.as_ptr(), p.add(HEADER), abytes.len());
        std::ptr::copy_nonoverlapping(
            bbytes.as_ptr(),
            p.add(HEADER + abytes.len()),
            bbytes.len(),
        );
        // Two counted halves make a counted whole, so a string built up
        // piece by piece is never walked to learn its length.
        let (ca, cb) = (obj_word1(a as *const u8), obj_word1(b as *const u8));
        if ca != 0 && cb != 0 {
            str_set_chars(p as u64, (ca - 1 + cb - 1) as usize);
        }
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

// Every string operation counts characters, not bytes, because a string is
// text rather than storage. The count is remembered in the header
// ([`str_chars`]), an ASCII string indexes by byte, and any other long one
// through marks found once ([`str_byte_at`]), so a loop over a long
// string's characters is linear rather than quadratic —
// as the VM's is, and as the Wasm backend's array of scalars is anyway.

#[no_mangle]
pub extern "C" fn kite_rt_str_len(s: u64) -> i64 {
    unsafe { str_chars(s) as i64 }
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
        let header = str_word0(len);
        root(&mut s);
        let p = alloc(HEADER + round8(len));
        unroot(1);
        *(p as *mut u64) = header;
        *(p as *mut u64).add(1) = 0;
        std::ptr::copy_nonoverlapping(str_bytes(s).as_ptr().add(start), p.add(HEADER), len);
        p as u64
    }
}

#[no_mangle]
pub extern "C" fn kite_rt_str_slice(s: u64, from: i64, to: i64) -> u64 {
    let mut s = s;
    unsafe {
        // Clamped rather than trapping: an out-of-range slice is an ordinary
        // condition in text processing.
        let char_len = str_chars(s) as i64;
        let from_c = from.clamp(0, char_len) as usize;
        let to_c = to.clamp(from.clamp(0, char_len), char_len) as usize;
        let start = str_byte_at(s, from_c);
        let end = start + str_byte_at_from(s, start, to_c - from_c);
        let len = end - start;
        let header = str_word0(len);
        root(&mut s);
        let p = alloc(HEADER + round8(len));
        unroot(1);
        *(p as *mut u64) = header;
        str_set_chars(p as u64, to_c - from_c);
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
        match usize::try_from(at) {
            Ok(i) if i < str_chars(s) => {
                let start = str_byte_at(s, i);
                str_str(s)[start..].chars().next().map_or(-1, |c| c as i64)
            }
            _ => -1,
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering — the VM's `Display`, transcribed
// ---------------------------------------------------------------------------

/// Print a float so it reads back as a Kite float: `1.0`, not `1`.
/// The rule the bytecode VM and the Wasm glue follow too, from the one place
/// it is written.
fn float_text(v: f64) -> String {
    kite_float::float_text(v)
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
                let len = slice_len(o);
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
                let (kk, vk) = ((aux & 0xFF) as u8, ((aux >> 8) & 0xFF) as u8);
                out.push('{');
                for (n, i) in map_entries(o).enumerate() {
                    if n > 0 {
                        out.push_str(", ");
                    }
                    render(*map_key(o, i), kk, out);
                    out.push_str(": ");
                    render(*map_value(o, i), vk, out);
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

/// Walked with a worklist of pairs still to compare rather than by recursion: a
/// value can be as deep as the program makes it, and comparing two lists of a
/// million cells recursed once per cell until the process aborted. The VM
/// walks its values the same way, for the same reason.
///
/// The worklist is made only for two aggregates, though. A map is a scan that
/// compares its key with each entry's, so this runs once per entry on every
/// `get`, `set` and `remove`, and allocating a worklist for a `str` there —
/// or copying a struct's shape to compare two of them — cost map-heavy
/// programs several times their time. A scalar, a string, `nil`, a closure
/// and a box of a scalar answer without one.
#[inline]
fn value_eq(a: u64, b: u64, k: u8) -> bool {
    match shallow_eq(a, b, k) {
        Some(same) => same,
        None => deep_eq(a, b),
    }
}

/// The answer for a pair that needs no worklist, or `None` for two
/// aggregates of one kind.
#[inline]
fn shallow_eq(a: u64, b: u64, k: u8) -> Option<bool> {
    match k {
        kind::FLOAT => return Some(f64::from_bits(a) == f64::from_bits(b)),
        kind::REF => {}
        _ => return Some(a == b),
    }
    if a == 0 || b == 0 {
        return Some(a == b);
    }
    unsafe {
        let (pa, pb) = (a as *const u8, b as *const u8);
        let ka = obj_kind(pa);
        if ka != obj_kind(pb) {
            return Some(false);
        }
        match ka {
            obj::STR => Some(str_bytes(a) == str_bytes(b)),
            // An optional's box holding a scalar: the payload's comparison,
            // which is one more step and no more.
            obj::BOX if obj_aux(pa) as u8 != kind::REF => {
                shallow_eq(*slot(pa, 0), *slot(pb, 0), obj_aux(pa) as u8)
            }
            // The VM's equality has no closure arm, so closures — even the
            // same closure — compare unequal.
            obj::CLOSURE => Some(false),
            _ => None,
        }
    }
}

/// Two aggregates, compared without recursing. The worklist starts empty,
/// which allocates nothing, and holds only pairs of aggregates: a struct of
/// scalars, or a slice of them, compares without allocating at all.
#[inline(never)]
fn deep_eq(a: u64, b: u64) -> bool {
    let mut work: Vec<(u64, u64, u8)> = Vec::new();
    if !ref_eq(a, b, &mut work) {
        return false;
    }
    while let Some((a, b, k)) = work.pop() {
        let same = match shallow_eq(a, b, k) {
            Some(same) => same,
            None => ref_eq(a, b, &mut work),
        };
        if !same {
            return false;
        }
    }
    true
}

/// Compare two children, answering now if [`shallow_eq`] can and queueing
/// them if they are aggregates.
fn pair(work: &mut Vec<(u64, u64, u8)>, a: u64, b: u64, k: u8) -> bool {
    match shallow_eq(a, b, k) {
        Some(same) => same,
        None => {
            work.push((a, b, k));
            true
        }
    }
}

/// Compare two aggregates of one kind one level deep, queueing the children
/// that are aggregates themselves.
fn ref_eq(a: u64, b: u64, work: &mut Vec<(u64, u64, u8)>) -> bool {
    unsafe {
        let (pa, pb) = (a as *const u8, b as *const u8);
        let each = |work: &mut Vec<(u64, u64, u8)>, kinds: &[u8]| {
            kinds
                .iter()
                .enumerate()
                .all(|(i, k)| pair(work, *slot(pa, i), *slot(pb, i), *k))
        };
        let shapes = &rt().shapes;
        match obj_kind(pa) {
            obj::STRUCT => {
                obj_aux(pa) == obj_aux(pb) && each(work, &shapes.structs[obj_aux(pa) as usize])
            }
            obj::ENUM => {
                if obj_aux(pa) != obj_aux(pb) || obj_word1(pa) != obj_word1(pb) {
                    return false;
                }
                let variant = (obj_word1(pa) & 0xFFFF_FFFF) as usize;
                each(work, &shapes.enums[obj_aux(pa) as usize][variant])
            }
            obj::TUPLE => each(work, &shapes.tuples[obj_aux(pa) as usize]),
            // By length: two slices of equal contents are equal whatever room
            // each has spare.
            obj::SLICE => {
                let (la, k) = (slice_len(pa), obj_aux(pa) as u8);
                la == slice_len(pb) && (0..la).all(|i| pair(work, *slot(pa, i), *slot(pb, i), k))
            }
            obj::MAP => {
                // In order: the VM compares the entry vectors directly, so two
                // maps built in different orders are different values.
                let (kk, vk) = ((obj_aux(pa) & 0xFF) as u8, ((obj_aux(pa) >> 8) & 0xFF) as u8);
                map_live(pa) == map_live(pb)
                    && map_entries(pa).zip(map_entries(pb)).all(|(i, j)| {
                        pair(work, *map_key(pa, i), *map_key(pb, j), kk)
                            && pair(work, *map_value(pa, i), *map_value(pb, j), vk)
                    })
            }
            obj::PAIR => {
                pair(work, *slot(pa, 0), *slot(pb, 0), obj_aux(pa) as u8)
                    && pair(work, *slot(pa, 1), *slot(pb, 1), kind::REF)
            }
            obj::ERR => pair(work, *slot(pa, 0), *slot(pb, 0), kind::REF),
            obj::BOX => pair(work, *slot(pa, 0), *slot(pb, 0), obj_aux(pa) as u8),
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
/// of input. Read as bytes and decoded lossily, as the VM does: a line that is
/// not UTF-8 gets U+FFFD for its bad bytes rather than reading as the end.
#[no_mangle]
pub extern "C" fn kite_rt_read_line() -> u64 {
    use std::io::BufRead;
    let mut line = Vec::new();
    let read = std::io::stdin().lock().read_until(b'\n', &mut line).unwrap_or(0);
    if read == 0 {
        return make_str(b"");
    }
    let line = String::from_utf8_lossy(&line);
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
// The host boundary — `kite-driver`'s host for the VM, transcribed
// ---------------------------------------------------------------------------
//
// A native program is its own host, and the one namespace a command-line
// program needs and a browser cannot have is `std/fs`. So this answers
// `@host("fs")` with the same six functions the bytecode VM's host answers,
// with the same semantics and the same failure encoding — `kite-driver`'s
// `host.rs` is the specification, and a difference here is a difference
// between `kitec run` and `kitec run --native`. Every other namespace — the
// DOM, the network — is the trap the VM gives without an embedder, because
// there is nothing here to supply it.
//
// # Declared signatures
//
// A host function is matched by name, and the name is whatever the program
// declared: nothing stops a program writing
// `@host("fs") extern fn read_text(path: int) -> str`. The VM finds out when it
// looks at the value it was handed, and traps. Here the value is a bare word,
// and reading an integer as a string would be reading memory at an address
// the program chose. So the code generator registers each extern's declared
// signature — one byte per parameter, then `:`, then one for the result: `s`
// a `str`, `i` an `int`, `f` a `float`, `b` a `bool`, `u` unit, `r` any other
// reference — and a declaration that does not match what the host implements
// traps before any word is read as anything.

/// Marks a returned string as a failure. Must match `fs.FAILURE_MARK` in
/// `std/fs.kite`, and the VM's host.
const HOST_FAILURE: char = '\u{1}';

/// Marks the answer of a call that can fail but did not. Must match
/// `fs.SUCCESS_MARK`: with only a failure mark, a file beginning with U+0001
/// read back as an error.
const HOST_SUCCESS: char = '\u{2}';

/// The host functions this runtime implements: the name, the signature a
/// declaration must have, and the name the VM's type-confusion trap uses for
/// it — so a mismatched argument is reported in the VM's words.
const HOST_FUNCTIONS: &[(&str, &[u8], &str)] = &[
    ("fs.read_text", b"s:s", "fs.read"),
    ("fs.write_text", b"ss:s", "fs.write"),
    ("fs.list_dir", b"s:s", "fs.list"),
    ("fs.remove_path", b"s:s", "fs.remove"),
    ("fs.path_kind", b"s:i", "fs.kind"),
    ("fs.temp_path", b":s", "fs.temp_path"),
];

/// A host function's answer, before it becomes a word.
enum HostAnswer {
    Str(String),
    Int(i64),
}

fn host_failure(message: impl std::fmt::Display) -> HostAnswer {
    HostAnswer::Str(format!("{}{}", HOST_FAILURE, message))
}

fn host_success(text: impl std::fmt::Display) -> HostAnswer {
    HostAnswer::Str(format!("{}{}", HOST_SUCCESS, text))
}

/// Answer one call. `args` are the declared parameters, already copied out of
/// the heap and already checked against the signature, so indexing them is
/// safe for every name matched here.
fn host_call(name: &str, args: &[String]) -> HostAnswer {
    match name {
        "fs.read_text" => match std::fs::read(&args[0]) {
            // Not `read_to_string`, so that "this file is not text" is a
            // message rather than a panic — and not lossy decoding, which
            // turns a binary file into plausible-looking rubbish.
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => host_success(text),
                Err(_) => host_failure("not valid UTF-8"),
            },
            Err(e) => host_failure(e),
        },
        "fs.write_text" => match std::fs::write(&args[0], &args[1]) {
            Ok(()) => host_success(""),
            Err(e) => host_failure(e),
        },
        "fs.list_dir" => match std::fs::read_dir(&args[0]) {
            Ok(entries) => {
                let mut names = String::new();
                for entry in entries {
                    match entry {
                        Ok(e) => {
                            // A line break inside a name would be read as the end of it; the
                            // listing carries it as NUL, which no file name can contain, and
                            // `fs.list` turns it back.
                            names.push_str(&e.file_name().to_string_lossy().replace('\n', "\0"));
                            names.push('\n');
                        }
                        Err(e) => return host_failure(e),
                    }
                }
                host_success(names)
            }
            Err(e) => host_failure(e),
        },
        "fs.remove_path" => {
            let meta = match std::fs::symlink_metadata(&args[0]) {
                Ok(m) => m,
                Err(e) => return host_failure(e),
            };
            // `remove_dir`, never `remove_dir_all`: deleting a tree is not
            // something a standard library should make a one-liner.
            let result = if meta.is_dir() {
                std::fs::remove_dir(&args[0])
            } else {
                std::fs::remove_file(&args[0])
            };
            match result {
                Ok(()) => host_success(""),
                Err(e) => host_failure(e),
            }
        }
        // 0 missing, 1 file, 2 directory — matching `fs.Kind`.
        "fs.path_kind" => HostAnswer::Int(match std::fs::metadata(&args[0]) {
            Ok(m) if m.is_dir() => 2,
            Ok(_) => 1,
            Err(_) => 0,
        }),
        "fs.temp_path" => {
            // Whatever this platform calls it, without a trailing separator,
            // so a caller joins with one and never gets two.
            let dir = std::env::temp_dir();
            let text = dir.to_string_lossy();
            HostAnswer::Str(text.trim_end_matches(['/', '\\']).to_string())
        }
        other => unreachable!("`{}` passed the host table and has no body", other),
    }
}

/// A signature's parameters and result, either side of the `:`.
fn split_sig(sig: &[u8]) -> (&[u8], &[u8]) {
    match sig.iter().position(|b| *b == b':') {
        Some(i) => (&sig[..i], &sig[i + 1..]),
        None => (sig, &[]),
    }
}

/// Check a declaration against what the host implements, trapping on a
/// mismatch, and hand back how many parameters the host reads.
fn host_params(name: &str, declared: &[u8], wants: &[u8], op: &str) -> usize {
    let _ = op;
    let (want_params, want_ret) = split_sig(wants);
    let (have_params, have_ret) = split_sig(declared);
    // As many parameters as the host reads, each declared as what the host
    // reads it as — the VM's check, in the VM's words. An extra one was
    // ignored, so a declaration could say the host takes a value it never
    // reads.
    if have_params.len() != want_params.len() {
        let plural = if have_params.len() == 1 { "" } else { "s" };
        trap(&format!(
            "`{}` is declared with {} parameter{}, and the host reads {}",
            name,
            have_params.len(),
            plural,
            want_params.len()
        ));
    }
    for (i, (have, want)) in have_params.iter().zip(want_params).enumerate() {
        if have != want {
            trap(&format!(
                "`{}` declares parameter {} as {}, and the host reads {}",
                name,
                i + 1,
                sig_type_name(*have),
                sig_type_name(*want)
            ));
        }
    }
    // The VM would hand back a value of the host's type whatever the
    // declaration said, and trap wherever the program next used it as the
    // other thing. A word has no type to check later, so this checks now.
    if have_ret != want_ret {
        trap(&format!(
            "`{}` is declared to return {}, and the host returns {}",
            name,
            sig_type_name(have_ret.first().copied().unwrap_or(b'u')),
            sig_type_name(want_ret.first().copied().unwrap_or(b'u'))
        ));
    }
    want_params.len()
}

fn sig_type_name(code: u8) -> &'static str {
    match code {
        b's' => "str",
        b'i' => "int",
        b'f' => "float",
        b'b' => "bool",
        b'u' => "()",
        _ => "reference",
    }
}

/// A call across the declared host boundary: the arguments are the first
/// `argc` words of the staging window, and the answer comes back as a word.
///
/// Every argument is copied out of the heap before the host runs and before
/// the answer is allocated, so no heap reference is held across the one
/// allocation here. That is the discipline `make_str` states, and it is
/// what makes the staged arguments safe to leave unrooted: after the copy
/// nothing reads them, so a collection that moves what they pointed at leaves
/// nothing stale behind.
#[no_mangle]
pub extern "C" fn kite_rt_call_extern(index: u64, argc: u64) -> u64 {
    let shapes = &rt().shapes;
    let name = shapes
        .externs
        .get(index as usize)
        .cloned()
        .unwrap_or_else(|| "?".to_string());
    let Some(&(_, wants, op)) = HOST_FUNCTIONS.iter().find(|(n, _, _)| *n == name) else {
        trap(&format!(
            "`{}` is a host function, and this runtime supplies no host",
            name
        ));
    };
    let declared = shapes.extern_sigs.get(index as usize).cloned().unwrap_or_default();
    let params = host_params(&name, &declared, wants, op);
    // SAFETY: the signature check guarantees the program declared at least
    // this many parameters, each a `str`; the code generator staged one word
    // per declared parameter, and the checker has proved each of those words
    // is a live string. Nothing has allocated since they were staged.
    debug_assert!(params <= argc as usize);
    let args: Vec<String> = (0..params)
        .map(|i| unsafe { str_str(*stage_slot(i)).to_string() })
        .collect();
    match host_call(&name, &args) {
        HostAnswer::Str(text) => make_str(text.as_bytes()),
        HostAnswer::Int(n) => n as u64,
    }
}

// ---------------------------------------------------------------------------
// The scheduler — the VM's `drive`, transcribed
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn kite_rt_task_spawn(poll: u64) -> i64 {
    rt().tasks.spawn(poll) as i64
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

/// An `await` that has to wait: not worth polling until task `id` finishes.
#[no_mangle]
pub extern "C" fn kite_rt_task_park_on(id: i64) {
    rt().park_on_request = Some(id as u64);
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
        let mut awaiters = Vec::new();
        let mut cursor = 0;
        while let Some(id) = rt().tasks.next_ready(cursor) {
            cursor = id + 1;
            let Some(t) = rt().tasks.get_mut(id) else { continue };
            if t.wake_at.is_some_and(|w| w > rt().clock) {
                continue;
            }
            // Read afresh for each poll: a collection during the last one may
            // have moved the closure, and the scheduler's slot is what it
            // updated.
            let poll = t.poll;
            polled = true;
            rt().tasks.start(id);
            rt().wake_request = None;
            rt().park_request = false;
            rt().park_on_request = None;
            rt().host_wait_request = false;
            let done = poll_task(poll);
            let asked = sched::Requests {
                wake_at: rt().wake_request.take(),
                park: std::mem::take(&mut rt().park_request),
                park_on: rt().park_on_request.take(),
                host: std::mem::take(&mut rt().host_wait_request),
            };
            if done {
                awaiters.extend(rt().tasks.finish(id));
                completed = true;
            } else {
                rt().tasks.file(id, asked);
            }
        }
        // A completion wakes what awaited it, every parked task and every
        // sleeping one, and lets each decide for itself.
        rt().tasks.swept(completed, awaiters);
        if !polled {
            // Everything is waiting. The VM gives its host a turn first; the
            // host here, like the one `kite-driver` gives the VM, answers
            // every call before returning and so never has anything
            // outstanding — its turn is always "nothing happened", and the
            // clock is the only thing that can move. If nothing is waiting on
            // that either, nothing will ever happen.
            match rt().tasks.next_deadline() {
                Some(next) if next > rt().clock => {
                    rt().clock = next;
                    rt().tasks.host_turned();
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
        // `kite_hir::TypeTag::encode`, which this crate cannot depend on:
        // structs count from one so that no type's tag is zero, the tag of an
        // error that carries nothing.
        let tag = match obj_kind(p) {
            obj::STRUCT => obj_aux(p) + 1,
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
        kite_rt_run,
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
        kite_rt_register_mutable_struct,
        kite_rt_struct_new,
        kite_rt_enum_new,
        kite_rt_tuple_new,
        kite_rt_slice_new,
        kite_rt_slice_extend,
        kite_rt_closure_new,
        kite_rt_map_new,
        kite_rt_map_extend,
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
        kite_rt_map_remove,
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
        kite_rt_register_extern_sig,
        kite_rt_task_spawn,
        kite_rt_task_wake_at,
        kite_rt_task_park,
        kite_rt_task_park_on,
        kite_rt_task_wait_host,
        kite_rt_time_now,
        kite_rt_drive,
    ];
    v.push(("KITE_RT_STAGE", (&raw const KITE_RT_STAGE).cast::<u8>()));
    v.push(("KITE_RT_DEPTH", (&raw const KITE_RT_DEPTH).cast::<u8>()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This runtime's `sched.rs` and the VM's are one text below their header
    /// comments: the two must poll in the same order, and a change made to
    /// one alone would reorder one backend's tasks and not the other's. The
    /// first line that differs is named, in both files.
    #[test]
    fn the_scheduler_is_the_vms_word_for_word() {
        use std::path::Path;
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sched.rs");
        let vm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kite-vm/src/sched.rs");
        let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e));
        let (ours, theirs) = (read(&here), read(&vm));
        // The header is the leading `//!` comment; each file says there why
        // it is a copy, in its own words. Numbered from one, as an editor does.
        let body = |text: &str| -> Vec<(usize, String)> {
            text.lines()
                .enumerate()
                .skip_while(|(_, l)| l.starts_with("//!"))
                .map(|(i, l)| (i + 1, l.to_string()))
                .collect()
        };
        let (ours, theirs) = (body(&ours), body(&theirs));
        for k in 0..ours.len().max(theirs.len()) {
            let line = |b: &[(usize, String)]| match b.get(k) {
                Some((n, l)) => format!("line {}: {:?}", n, l),
                None => "the end of the file".to_string(),
            };
            let same = matches!((ours.get(k), theirs.get(k)), (Some((_, a)), Some((_, b))) if a == b);
            assert!(
                same,
                "kite-rt's sched.rs and kite-vm's differ below their headers — change them together:\n  \
                 kite-rt/src/sched.rs {}\n  kite-vm/src/sched.rs {}",
                line(&ours),
                line(&theirs),
            );
        }
    }

    /// A full map grows to twice its room, always a power of two (an
    /// indexed map's bucket mask needs one), and always to more than it had.
    #[test]
    fn a_full_map_doubles_its_room() {
        assert_eq!(map_room_for_growth(0), MAP_SMALL);
        assert_eq!(map_room_for_growth(5), 16);
        assert_eq!(map_room_for_growth(MAP_SMALL), 16);
        for k in 4..24 {
            assert_eq!(map_room_for_growth(1 << k), 2 << k);
        }
    }

    /// A stack, laid out by hand, one word per slot. Addresses grow with the
    /// index, as a real stack's do towards its base.
    struct FakeStack {
        words: Vec<u64>,
    }

    impl FakeStack {
        fn new(len: usize) -> FakeStack {
            FakeStack { words: vec![0; len] }
        }
        fn addr(&self, i: usize) -> usize {
            self.words.as_ptr() as usize + 8 * i
        }
        /// A frame record at word `i`: the caller's frame pointer, then the
        /// return address into the caller.
        fn record(&mut self, i: usize, caller_fp: usize, ret: usize) {
            self.words[i] = caller_fp as u64;
            self.words[i + 1] = ret as u64;
        }
        fn walk(&self, start: usize, entry: usize, maps: &[(usize, Vec<u32>)]) -> Result<Vec<usize>, String> {
            let mut seen = Vec::new();
            // SAFETY: every record the chains below describe lies inside
            // `words`, and the walk reads only between `start` and `entry`.
            unsafe { walk_frames(start, entry, maps, &mut |s| seen.push(s as usize)) }.map(|()| seen)
        }
    }

    /// The layout that broke the first version of the walk: AArch64 Linux,
    /// where a Rust function's frame record sits *below* the registers it
    /// saves, so its caller's stack pointer is not the record plus 16.
    ///
    /// Words, from the bottom: the runtime function's record, then the four
    /// callee-saved registers it stored above it (`stp x29, x30, [sp, #-48]!;
    /// stp x22, x21, [sp, #16]; stp x20, x19, [sp, #32]`), then the compiled
    /// caller's frame — two spill slots at its stack pointer, then its frame
    /// record — and the entry's frame above that.
    #[test]
    fn slots_are_found_from_the_callers_own_frame_pointer() {
        let mut stack = FakeStack::new(16);
        let ret = 0x1000_0040;
        let (callee, caller, entry) = (0, 8, 12);
        stack.record(callee, stack.addr(caller), ret);
        for (i, saved) in (2..6).zip([0x2222, 0x2121, 0x2020, 0x1919]) {
            stack.words[i] = saved;
        }
        // The caller's spills, at its stack pointer: word 6 and word 7.
        stack.words[6] = 0xAAAA;
        stack.words[7] = 0xBBBB;
        stack.record(caller, stack.addr(entry), 0x2000_0000);
        // Cranelift's map, as `collect_maps` records it: 16 and 8 bytes
        // below the caller's frame pointer.
        let maps = vec![(ret, vec![16, 8])];
        let slots = stack.walk(stack.addr(callee), stack.addr(entry), &maps).unwrap();
        assert_eq!(slots, vec![stack.addr(6), stack.addr(7)]);
        // What "the callee's record plus 16" made of the same map: the
        // caller's stack pointer taken to be word 2, so its slots at words 2
        // and 3 — two of the callee's saved registers, which the collector
        // rewrote while it lost the real references.
        let span = 16;
        let old: Vec<usize> = [16usize, 8]
            .iter()
            .map(|below| stack.addr(callee) + 16 + (span - below))
            .collect();
        assert_eq!(old, vec![stack.addr(2), stack.addr(3)]);
    }

    /// No cap on depth: the walk is bounded by the entry, not by a count, and
    /// a root in the outermost frame of a very deep chain is still found.
    #[test]
    fn a_chain_of_any_depth_reaches_the_entry() {
        const FRAMES: usize = 1_500_000;
        let mut stack = FakeStack::new(2 * FRAMES + 4);
        for f in 0..FRAMES - 1 {
            stack.record(2 * f, stack.addr(2 * f + 2), 0x4000);
        }
        // The last record's caller is the entry itself, two words further up
        // so that the word just below the entry's frame pointer is a slot of
        // the outermost frame rather than part of the record.
        let entry = stack.addr(2 * FRAMES + 2);
        stack.record(2 * (FRAMES - 1), entry, 0x5000);
        let maps = vec![(0x5000, vec![8])];
        let slots = stack.walk(stack.addr(0), entry, &maps).unwrap();
        assert_eq!(slots, vec![entry - 8]);
    }

    #[test]
    fn a_chain_that_stops_climbing_is_an_error() {
        let mut stack = FakeStack::new(8);
        stack.record(0, stack.addr(2), 0);
        stack.record(2, stack.addr(0), 0);
        let why = stack.walk(stack.addr(0), stack.addr(6), &[]).unwrap_err();
        assert!(why.contains("stopped climbing"), "{}", why);
    }

    #[test]
    fn a_chain_that_misses_the_entry_is_an_error() {
        let mut stack = FakeStack::new(8);
        stack.record(0, stack.addr(6), 0);
        let why = stack.walk(stack.addr(0), stack.addr(4), &[]).unwrap_err();
        assert!(why.contains("left the stack"), "{}", why);
    }

    #[test]
    fn a_slot_outside_its_frame_is_an_error() {
        let mut stack = FakeStack::new(8);
        let ret = 0x3000;
        stack.record(0, stack.addr(4), ret);
        stack.record(4, stack.addr(6), 0);
        // 40 bytes below the caller's frame pointer is word -1: below the
        // callee's own record, so not a slot of the caller's at all.
        let maps = vec![(ret, vec![40])];
        let why = stack.walk(stack.addr(0), stack.addr(6), &maps).unwrap_err();
        assert!(why.contains("outside the frame"), "{}", why);
    }

    /// A run's settings are consumed by the run they were for, and its heap
    /// is gone once it is over — not held until some later startup, which in
    /// `kitec` never comes.
    #[test]
    fn a_finished_run_leaves_nothing_behind() {
        let _lock = run_lock();
        prepare_run(RunConfig { nursery_bytes: Some(4096), major_threshold: None }, true);
        kite_rt_startup();
        assert!(NEXT_RUN.lock().unwrap().is_none(), "startup left its settings for the next run");
        assert_eq!(rt().nursery_size, 4096);
        kite_rt_print_str(make_str(b"hello"));
        let (captured, stats) = finish_run();
        assert_eq!(captured, b"hello\n");
        assert_eq!(stats, RunStats::default());
        assert!(rt_if_started().is_none(), "the heap outlived the run");
        assert_eq!(unsafe { ENTRY_FP }, 0);
    }

    /// Run this test's own binary again, filtered to `name`, with a variable
    /// that tells it to do the thing that exits — the way to test a trap,
    /// which ends the process rather than panicking.
    fn in_a_child(name: &str) -> std::process::Output {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env("KITE_RT_TEST_CHILD", "1")
            .output()
            .unwrap()
    }

    #[test]
    fn a_string_past_four_gib_is_a_trap_not_a_truncation() {
        assert_eq!(str_len_field(u32::MAX as usize), Some(u32::MAX));
        assert_eq!(str_len_field(u32::MAX as usize + 1), None);
        if std::env::var_os("KITE_RT_TEST_CHILD").is_some() {
            str_word0(u32::MAX as usize + 1);
            unreachable!("a string of 4 GiB and a byte got a header");
        }
        let out = in_a_child("tests::a_string_past_four_gib_is_a_trap_not_a_truncation");
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("error: string too long: 4294967296 bytes"), "{}", stderr);
    }

    #[test]
    fn a_host_declaration_that_does_not_match_is_a_trap() {
        // Matching: nothing to say.
        assert_eq!(host_params("fs.write_text", b"ss:s", b"ss:s", "fs.write"), 2);
        if std::env::var_os("KITE_RT_TEST_CHILD").is_some() {
            host_params("fs.read_text", b"i:s", b"s:s", "fs.read");
            unreachable!("an int was accepted as a path");
        }
        let out = in_a_child("tests::a_host_declaration_that_does_not_match_is_a_trap");
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The VM's words for the same mistake, naming what was declared.
        let said = "error: `fs.read_text` declares parameter 1 as int, and the host reads str";
        assert!(stderr.contains(said), "{}", stderr);
    }

    /// A parameter the host does not read is a declaration saying something
    /// the host does not do. It was ignored, here and on the VM alike.
    #[test]
    fn a_host_declaration_with_a_parameter_too_many_is_a_trap() {
        if std::env::var_os("KITE_RT_TEST_CHILD").is_some() {
            host_params("fs.temp_path", b"i:s", b":s", "fs.temp_path");
            unreachable!("a parameter the host never reads was accepted");
        }
        let out = in_a_child("tests::a_host_declaration_with_a_parameter_too_many_is_a_trap");
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        let said = "error: `fs.temp_path` is declared with 1 parameter, and the host reads 0";
        assert!(stderr.contains(said), "{}", stderr);
    }

    /// The host's answers, and above all its failures, in the encoding
    /// `std/fs` unwraps: a leading `\u{1}`, then the reason.
    #[test]
    fn the_host_answers_as_the_vms_host_does() {
        let text = |a: HostAnswer| match a {
            HostAnswer::Str(s) => s,
            HostAnswer::Int(n) => panic!("expected a str, got {}", n),
        };
        let int = |a: HostAnswer| match a {
            HostAnswer::Int(n) => n,
            HostAnswer::Str(s) => panic!("expected an int, got {:?}", s),
        };
        let missing = text(host_call("fs.read_text", &["/definitely/not/here".to_string()]));
        assert!(missing.starts_with(HOST_FAILURE), "{:?}", missing);

        let dir = text(host_call("fs.temp_path", &[]));
        assert!(!dir.ends_with('/') && !dir.ends_with('\\'), "{:?}", dir);
        let file = format!("{}/kite-rt-host-test-{}.txt", dir, std::process::id());
        assert_eq!(text(host_call("fs.write_text", &[file.clone(), "hello".to_string()])), "\u{2}");
        assert_eq!(text(host_call("fs.read_text", std::slice::from_ref(&file))), "\u{2}hello");
        assert_eq!(int(host_call("fs.path_kind", std::slice::from_ref(&file))), 1);
        assert_eq!(int(host_call("fs.path_kind", std::slice::from_ref(&dir))), 2);
        let listing = text(host_call("fs.list_dir", std::slice::from_ref(&dir)));
        let name = format!("kite-rt-host-test-{}.txt", std::process::id());
        assert!(listing.starts_with(HOST_SUCCESS), "{:?}", listing);
        assert!(listing[1..].lines().any(|l| l == name), "{} is not in the listing", name);
        assert_eq!(text(host_call("fs.remove_path", std::slice::from_ref(&file))), "\u{2}");
        assert_eq!(int(host_call("fs.path_kind", std::slice::from_ref(&file))), 0);
        assert!(text(host_call("fs.remove_path", &[file])).starts_with(HOST_FAILURE));
    }

    /// A nursery size is a number from outside — `KITE_NURSERY_BYTES`, or a
    /// harness — and one too large for a `Layout` panicked inside the
    /// runtime, where nothing can report it. Past a gigabyte it is a
    /// gigabyte, and under a page a page.
    #[test]
    fn a_nursery_size_out_of_range_is_brought_into_it() {
        let _run = run_lock();
        for (asked, got) in [(usize::MAX, MAX_NURSERY), (100_000_000_000_000, MAX_NURSERY), (0, MIN_NURSERY)] {
            prepare_run(RunConfig { nursery_bytes: Some(asked), major_threshold: None }, false);
            kite_rt_startup();
            assert_eq!(rt().nursery_size, got, "asked for {}", asked);
            finish_run();
        }
    }

    /// Every allocation this test binary makes, counted per thread, so a
    /// test can ask how many a piece of code made without a neighbour's
    /// counting too.
    struct Counting;

    thread_local! {
        static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    // SAFETY: every call is passed straight to the system allocator; the
    // count is a `Cell` in a `const`-initialised thread local, which neither
    // allocates nor registers a destructor, so counting cannot recurse into
    // this allocator.
    unsafe impl std::alloc::GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
            std::alloc::System.alloc(layout)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            std::alloc::System.dealloc(ptr, layout)
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// `==` walks a worklist only once it has an aggregate to descend into.
    /// A map is a scan comparing its key with every entry's, and the worklist
    /// made for each pair of `str`s there — and the copy of a struct's shape
    /// made to compare two structs — made map-heavy programs several times
    /// slower. A flat value, and an aggregate holding only flat values,
    /// compares without allocating at all.
    #[test]
    fn equality_of_flat_values_allocates_nothing() {
        let _run = run_lock();
        kite_rt_startup();
        let kinds = [kind::INT, kind::FLOAT, kind::REF];
        // SAFETY: `kinds` is three readable bytes.
        unsafe { kite_rt_register_struct_shape(0, kinds.as_ptr(), 3) };
        let point = |x: i64, name: &str| {
            let name = make_str(name.as_bytes());
            unsafe {
                *stage_slot(0) = x as u64;
                *stage_slot(1) = 0.5f64.to_bits();
                *stage_slot(2) = name;
            }
            kite_rt_struct_new(0, 3)
        };
        let ints = |xs: &[i64]| {
            for (i, x) in xs.iter().enumerate() {
                unsafe { *stage_slot(i) = *x as u64 };
            }
            kite_rt_slice_new(kind::INT as u64, xs.len() as u64, xs.len() as u64)
        };
        let boxed = |x: u64| kite_rt_box_new(x, kind::INT as u64);
        let cases = [
            (make_str(b"key 1234"), make_str(b"key 1234"), true),
            (make_str(b"key 1234"), make_str(b"key 1235"), false),
            (make_str(b"key"), 0, false),
            (boxed(7), boxed(7), true),
            (boxed(7), boxed(8), false),
            (point(1, "a"), point(1, "a"), true),
            (point(1, "a"), point(1, "b"), false),
            (point(1, "a"), point(2, "a"), false),
            (ints(&[1, 2, 3]), ints(&[1, 2, 3]), true),
            (ints(&[1, 2, 3]), ints(&[1, 2]), false),
        ];
        let mut failures = Vec::new();
        for (i, (a, b, want)) in cases.iter().enumerate() {
            let before = ALLOCATIONS.with(|n| n.get());
            let answer = value_eq(*a, *b, kind::REF);
            let made = ALLOCATIONS.with(|n| n.get()) - before;
            if answer != *want || made != 0 {
                failures.push(format!("case {}: answered {}, allocated {} times", i, answer, made));
            }
        }
        let nan = f64::NAN.to_bits();
        assert!(value_eq(7, 7, kind::INT) && !value_eq(nan, nan, kind::FLOAT));
        finish_run();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
