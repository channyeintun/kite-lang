# Concurrency: one concept, many threads

How Kite gives you `async`/`await` on a genuinely multi-threaded runtime, without
goroutines, channels, mutexes, or data races — and how the same source becomes
parallel on the web the day the platform allows it.

---

## 1. The question this design answers

> *Can the runtime manage threads, with the user writing plain `async`/`await`,
> without the whole thing being single-threaded?*

Yes. The confusion to clear first is that **`async` is not a threading model.**
It is a syntax for expressing "this takes time, and other work may proceed
meanwhile." How many OS threads execute that work is a property of the
**scheduler**, which the source never mentions.

The proof is deployed at scale:

| System | Surface | Scheduler |
|---|---|---|
| Rust + Tokio | `async fn` / `.await` | Multi-threaded work-stealing pool by default |
| Erlang / BEAM | Plain sequential-looking code | N schedulers, one per core, preemptive |
| Swift 6 | `async func` / `await` | Multi-threaded cooperative pool |
| C# / .NET | `async` / `await` | Thread pool |
| JavaScript | `async` / `await` | Single-threaded (a platform limit, not a syntax one) |

Four of those five are multi-threaded with the identical surface syntax. The
outlier is JavaScript, and it is the outlier because of *its* platform, not
because `async` implies one thread.

**So the real question is not "can the syntax be multi-threaded" — it is "what
data is allowed to cross a thread boundary, and who checks."** That is a type
system question, and Kite answers it once, at v1, in a way that costs the
programmer nothing in the common case.

---

## 2. The surface

One concept. Some operations take time; mark them `async` and `await` them.

```kite
pub async fn fetch_user(id: UserId) -> (User, error) {
    let (res, err) = await http.get("/api/users/\(id)")
    check err

    let (user, err) = await json.decode<User>(res.body)
    check err

    return user, nil
}
```

Calling an `async fn` **without** `await` starts it and hands back a `Task<T>`.
That is the whole concurrency primitive:

```kite
// Sequential: 200ms
let (a, err) = await fetch_user(1)
check err
let (b, err) = await fetch_user(2)
check err

// Concurrent: 100ms
let ta = fetch_user(1)          // starts now
let tb = fetch_user(2)          // starts now
let ((a, ea), (b, eb)) = await task.both(ta, tb)
check ea
check eb
```

Combinators, and that is the complete list:

```kite
task.all([t1, t2, t3])          // -> Task<[T]>      all must finish
task.both(ta, tb)               // -> Task<(A, B)>   two of different types
task.race([t1, t2])             // -> Task<T>        first to finish wins
task.timeout(t, time.seconds(5))
task.parallel(items, |item| …)  // -> Task<[U]>      CPU-bound fan-out
```

**There is no channel type.** A `Task<T>` *is* a one-shot result channel; you
`await` it instead of receiving from it. Nothing in this API requires
understanding buffering, closing, or `select`, and there is no way to write a
deadlock by mismatching send and receive counts.

---

## 3. The scheduler is a target property

| Target | Scheduler | Real parallelism today |
|---|---|---|
| `native-*` | Work-stealing pool, one worker thread per core | **Yes** |
| `kbc` (bytecode VM) | Work-stealing pool | **Yes** |
| `wasm32-gc` (web, now) | Cooperative loop on main thread; `task.parallel` fans out to a Web Worker isolate pool | **CPU-bound work only** |
| `wasm32-gc` (web, later) | Same work-stealing pool as native | **Yes, when shared-everything-threads ships** |

Nothing above is visible in Kite source. `fetch_user` compiles unchanged for all
four rows.

### Why the web row is restricted

Not a design choice — a platform fact. **WasmGC references cannot cross a thread
boundary at all.** There is currently no way to share a reference value between
Wasm threads. The
[shared-everything-threads proposal](https://github.com/WebAssembly/shared-everything-threads)
exists to fix precisely this — adding `shared` annotations, sequentially
consistent and release-acquire access to shared GC data, and managed waiter
queues — and it remains a **draft**.

This is why Kotlin/Wasm's `Dispatchers.Default` and `Dispatchers.IO` quietly run
on the main thread, and why Flutter's multi-threaded web rendering needs COOP/COEP
headers and still cannot share an object graph. Nobody has shared-heap
parallelism in a browser right now. Any language claiming otherwise on the web
target is either using linear memory without a GC, or is mistaken.

### The bet

Kite enforces the invariant that proposal will require **starting in v1**. When
shared-everything-threads ships, the web scheduler is replaced with the native
work-stealing pool and **existing programs become parallel with no source
change**. Programs written today are already correct for that runtime.

This is the most consequential forward-compatibility decision in the language,
and it costs nothing to make now and would be a breaking change to make later.

---

## 4. `Share`: the invariant, made nearly invisible

`Share` is an auto-derived marker meaning *"a value of this type may move to
another thread or isolate."*

### The rule

A type is `Share` when:

- it is a primitive (`int`, `float`, `bool`, `char`, sized numerics), or
- it is a `str`, or
- it is a struct or enum whose fields are **all `Share`** and **none is `var`**, or
- it is a slice, map, or tuple whose elements are `Share`, or
- it is explicitly synchronised: `sync.Mutex<T>`, `sync.Atomic<T>`.

A type is **not** `Share` when it has a `var` field anywhere in its transitive
structure, or holds a host reference (DOM node, canvas context, file handle,
socket).

### Why this costs almost nothing

Because **struct fields are immutable by default**
([spec §1.3](../SPECIFICATION.md#13-why-immutable-by-default)), the great
majority of user types satisfy `Share` automatically. The programmer never writes
`Share`, never reads it in a signature, and in most programs never learns it
exists.

```kite
pub struct Order {          // Share — every field immutable and Share
    pub id:    int
    pub items: [LineItem]
    pub total: Money
}

pub struct Counter {        // NOT Share — has a var field
    pub var count: int
}
```

It becomes visible only when violated, and then it explains itself:

```
error[E0520]: `Counter` cannot be moved to another task
   ┌─ worker.kite:12:38
   │
12 │     let totals = await task.parallel(counters, |c| c.tick())
   │                                                    ^ `Counter` is not Share
   │
   ┌─ counter.kite:2:5
   │
 2 │     pub var count: int
   │     --- because this field is mutable, `Counter` may not be shared
   │
   = note: two tasks holding the same mutable value is a data race
help: return a new value instead of mutating
   │
 - pub var count: int
 + pub count: int
   │
help: or serialise access explicitly
   │
12 │     let totals = await task.parallel(counters, |c| c.lock().tick())
   │                                                     ^^^^^^^ via sync.Mutex<Counter>
```

This is the same insight as Rust's `Send` and Swift 6's `Sendable`. The
difference is the default: Rust and Swift default to mutability and so surface
the marker constantly. Kite defaults to immutability, so the marker is dormant
until you actually create the hazard.

**Result: Kite has no data races on any target, with no annotation burden.**

---

## 5. CPU-bound work on the web, today

`task.parallel` is the fan-out primitive. Its input and output must be `Share`,
which — because `Share` values are deeply immutable — means they serialise safely
across `postMessage` **and** will share directly by reference once shared-heap
threads exist.

```kite
pub async fn process_images(images: [ImageData]) -> ([ImageData], error) {
    let results = await task.parallel(images, |img| {
        return filters.gaussian_blur(img, radius: 4.0)
    })
    return results, nil
}
```

| Today | After shared-everything-threads |
|---|---|
| Worker isolate pool, `Share` values structured-cloned in and out | Same work-stealing pool as native, `Share` values passed by reference |
| Copy cost proportional to payload | Zero copy |
| **Source unchanged** | **Source unchanged** |

The runtime picks the strategy. `task.parallel` is the only place a Kite program
can observe that more than one thread might exist, and even there it observes
only the constraint (`Share`), never the mechanism.

For the web target, `task.parallel` requires COOP/COEP headers to use
`SharedArrayBuffer` for zero-copy transfer of `buffer.*` payloads; without them it
falls back to structured clone. `kite build --target web` warns when a program
uses `task.parallel` and prints the two header lines needed.

---

## 6. Cancellation

Structured, and tied to scope:

```kite
pub async fn search(query: str) -> ([Result], error) {
    let scope = task.scope()
    defer scope.cancel()          // every task started in this scope stops here

    let fast = scope.start(cache.lookup(query))
    let slow = scope.start(db.search(query))

    return await task.race([fast, slow])   // loser is cancelled by the defer
}
```

A cancelled task stops at its next `await` point. Cancellation is cooperative —
there is no preemptive kill, because a preempted task cannot maintain
invariants. `defer` blocks still run, so resources are released.

A task started inside a `task.scope()` cannot outlive it. This makes task leaks
structurally impossible, the same guarantee structured concurrency provides in
Kotlin and Swift.

---

## 7. Function colouring, and what it buys

The honest cost: `async` colours functions. A synchronous function cannot call an
async one without becoming async itself.

Three alternatives were considered:

| Alternative | Why rejected |
|---|---|
| **Callbacks** | No colouring, but callback pyramids and manual error plumbing at every level. Verbose in the way that harms reading, not the way that helps it. |
| **Implicit async (effect inference)** | Compiler infers which functions do I/O and transforms them. Looks fully synchronous, like Go. Rejected because a function's suspension behaviour would then depend on its transitive callees — a change deep in a library silently colours everything above it, and the error messages become impossible to localise. That directly contradicts Kite's premise that a line should be understandable locally. |
| **Goroutine-style green threads** | Requires the runtime to multiplex M user stacks onto N OS threads. On the web this needs the [stack-switching proposal](https://github.com/WebAssembly/stack-switching), which is post-3.0 and unshipped. Not implementable today. |

Colouring buys something real: **`await` marks every point where other code may
run and state may change**. In a goroutine model any function call may yield, so
every shared value is suspect. In Kite, between two `await`s, nothing else
touched your state. That is a strong, checkable local property, and it is what
makes reasoning about concurrent code tractable for someone who has never studied
concurrency.

---

## 8. Implementation

### Today: state machines

An `async fn` compiles to a resumable state machine. The body is split at each
`await` into numbered states; locals live across a suspension point are stored in
a WasmGC struct that *is* the `Task`. This is the transformation Rust, C#, and
Kotlin all use, and it needs no Wasm feature beyond those ratified in 3.0.

```
async fn f() {           →   struct F_State {
    let a = g()                  var state: i32
    await h()                    var a: G_Result
    use(a)                       // locals live across the await
}                            }
                             fn F_resume(s: F_State) -> Poll { … }
```

### Later: stack switching

The [stack-switching proposal](https://github.com/WebAssembly/stack-switching)
would permit real coroutine stacks, removing the state-machine transformation and
its allocation per task. It reuses the tags from the exception-handling proposal
that shipped in Wasm 3.0, so the groundwork exists.

Kite's semantics are compatible with either lowering. Adopting it is a **compiler
change with no language change** — the same property that makes the `Share` bet
safe.

### Scheduler

```
kite-rt/
  scheduler/
    native.rs      work-stealing deque per worker, one worker per core
    wasm_web.rs    microtask-queue-driven cooperative loop + isolate pool
    shared.rs      future: work-stealing over shared-everything-threads
  task.rs          Task<T>, scopes, cancellation tokens
  waker.rs         wake-up plumbing, uniform across schedulers
```

The scheduler is selected at link time by target. `Task<T>` and the combinators
are identical across all of them.

---

## 9. Summary

| Question | Answer |
|---|---|
| Is the surface single-threaded? | **No.** `async`/`await` says nothing about threads. |
| Is the runtime multi-threaded? | **Yes on native and bytecode, today.** Partially on web, fully when the platform allows. |
| Why is the web limited? | WasmGC references cannot cross threads. The fix is a draft proposal, not a Kite decision. |
| Will web programs need rewriting later? | **No.** `Share` enforces the future invariant now. |
| How many concepts must a beginner learn? | **One:** some things take time; `await` them. |
| Can a Kite program have a data race? | **No**, on any target, by construction. |
| Can a Kite program deadlock? | Not by channel mismatch — there are no channels. A cycle of `await`s can still stall, and the runtime detects and reports it in debug builds. |
