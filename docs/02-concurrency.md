# Concurrency: one concept, many threads

How Kite gives you `async`/`await` without goroutines, channels, mutexes, or
data races — and how the same source becomes parallel, on every target
including the web, the day the platform and the runtime allow it.

**The scheduler is a cooperative loop today and nothing here runs on two cores
yet.** What is finished is the part that has to be finished first: the surface,
and the type system rule about what may cross a thread boundary. Adding threads
later is a runtime change; adding the rule later would be a breaking one.

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

| Target | Scheduler today | Real parallelism today |
|---|---|---|
| `wasm32-gc` (web) | Cooperative loop on the main thread | **No** |
| `kbc` (bytecode VM) | Cooperative loop; the VM's values are `Rc`-based | **No** |
| `native-*` | Cooperative loop | **No** |
| any, later | Work-stealing pool, one worker thread per core | **When the runtime has one** |

**Nothing runs on two cores yet, on any target.** This table used to say
`native-*` and `kbc` did, which was the intended runtime written up as the
existing one: there is no thread spawned anywhere in the compiler or the
runtime today.

Nothing above is visible in Kite source, and that is the whole point of the
section — `fetch_user` compiles unchanged for every row, including the one that
has not been built.

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

`task.parallel` is the fan-out primitive, and **today it does not fan out** —
it applies the function to each item in turn, yielding between them, on every
target. What is finished about it is the *rule*: its input and its output must
be `Share`.

```kite
pub async fn blur_all(images: [Picture]) -> [Picture] {
    return await task.parallel(images, |img: Picture| blurred(img, 4.0))
}
```

That rule is what makes the shape forward-compatible, because `Share` values
are deeply immutable and so are safe under either mechanism a real runtime
could use:

| If the runtime copies | If the runtime shares |
|---|---|
| Serialised in and out — safe, because nothing else can write to a `Share` value | Passed by reference — safe, for the same reason |
| Copy cost proportional to payload | Zero copy |
| **Source unchanged** | **Source unchanged** |

The runtime picks. `task.parallel` is the only place a Kite program can observe
that more than one thread might exist, and even there it observes only the
constraint, never the mechanism — which is why the constraint is the half worth
shipping first.

---

## 6. Cancellation

Structured, and tied to scope:

**There is no cancellation, and that is the current answer rather than a gap
waiting to be filled.** `task.scope(tasks)` waits for every task in a group
before continuing, so a task cannot outlive the code that started it by
accident — which is the half of structured concurrency that needs no new
concept:

```kite
pub async fn search(query: str) -> [Answer] {
    let fast = cache_lookup(query)
    let slow = db_search(query)
    return await task.scope([fast, slow])
}
```

`task.race` returns the first result and **does not stop the losers**. That is
deliberate: a task the program started is the program's work, and ending it
silently from somewhere else is exactly the hidden control flow this language
spends its omissions avoiding. A loser runs to completion and its result is
dropped.

What a cancellation design would have to settle, whenever it is attempted:
where the stop happens (the next `await`, since a preempted task cannot
maintain its invariants), what a stopped task's `defer` blocks do, and how a
caller learns which of the two it got. None of that is decided here, and
inventing it in a document ahead of the code is how this section came to
describe a `scope.cancel()` that never existed.

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

Today the scheduler is a cooperative loop, and it is small enough to live
beside the collector in `kite-rt/src/lib.rs` rather than in a directory of its
own. A task is resumed when its deadline arrives or when the host answers; when
every task is waiting on a deadline the clock jumps to the earliest, which is
what makes a program that sleeps cost no real time under test.

The shape a threaded runtime would take — a work-stealing deque per worker,
one worker per core, chosen at link time by target — is what the rest of this
document is written against. `Task<T>` and the combinators do not change when
it arrives, which is the claim `Share` exists to keep true.

---

## 9. Summary

| Question | Answer |
|---|---|
| Is the surface single-threaded? | **No.** `async`/`await` says nothing about threads. |
| Is the runtime multi-threaded? | **Not yet, on any target.** The surface and the type system are ready for one; the scheduler is a cooperative loop. |
| Why is the web limited? | WasmGC references cannot cross threads. The fix is a draft proposal, not a Kite decision. |
| Will web programs need rewriting later? | **No.** `Share` enforces the future invariant now. |
| How many concepts must a beginner learn? | **One:** some things take time; `await` them. |
| Can a Kite program have a data race? | **No**, on any target, by construction. |
| Can a Kite program deadlock? | Not by channel mismatch — there are no channels. A cycle of `await`s can still stall, and nothing detects that today. |
