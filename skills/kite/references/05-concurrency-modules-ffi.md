# Concurrency, modules, packages, memory and the host boundary

The deltas that will catch you out, in the order you will hit them:

1. **Calling an `async fn` does not run it.** Not even up to the first `await`.
   The body is *queued*; nothing in it executes until somebody awaits the
   `Task<T>` or the scheduler gets a turn. Rust's futures are lazy this way too;
   JavaScript's `async` functions are not, and that is the assumption that
   breaks.
2. **`Task<T>` is an ordinary value.** A plain `fn` may call an `async fn`, hold
   the task, put it in a slice, and return it. Only `await` needs `async`.
3. **`use` is per *module*, not per file** — and a module reaches only what it
   imports. A qualified name that resolves in one module does not resolve in
   another, even in the same program — the entry file included (§4).
4. **The file you hand `kitec` is a one-file module.** The directory it sits in
   is *not* a module: a sibling `.kite` file is reachable only through a `use`,
   exactly like any other module (§4).
5. **`use` paths are relative to the importing file's own directory**, not to
   the project root.
6. **A module is where its source is**, so `util` inside `a/` and `util` inside
   `b/` are two modules, and so are `dep/utils` and `utils` — but the standard
   library's twenty names and `prelude` are reserved on the *last* segment, so
   `use dep/json` is `E0403`.
7. **`Share` is a real bound you can write yourself**, and closures, `dyn`
   values, bare type parameters and `JsValue` all fail it.
8. **Nothing is parallel on any target today.** `Share` is enforced anyway.
9. **`JsValue` has no `==`.** Identity is `js.same`.

Everything below was checked against `target/release/kitec`. Where the
specification and the compiler disagree it is called out inline; the compiler
wins.

---

## 1. `async`, `await`, `Task<T>`

An `async fn` returns `Task<T>` where `T` is the declared return type. `await`
suspends until the task has a value and unwraps it. There is no `spawn`, no
channel type, no cancellation, and no executor to construct — `kitec run` and
the generated web glue each supply a cooperative loop.

### Calling queues; awaiting runs

```kite
async fn noisy(tag: str) -> int {
    io.print("body of \(tag) ran")
    return 1
}

async fn main() {
    io.print("before call")
    let t = noisy("a")
    io.print("after call")
    io.print("awaited \(await t)")
}
```

That prints `before call`, `after call`, `body of a ran`, `awaited 1` — in that
order. The body did not begin at the call.

An un-awaited task is **not** discarded: the runtime drains the queue after
`main` returns, so the body still runs, just later and with nobody reading the
result.

```kite
async fn work() -> int {
    io.print("work ran")
    return 7
}

fn main() {
    let t = work()
    io.print("main done")
}
```

prints `main done` then `work ran`. Note also that `main` here is a plain `fn`:
calling an `async fn` from synchronous code is fine, because what comes back is
a value.

Awaiting the same task twice is defined and cheap — the body runs once and the
second `await` returns the stored result.

### Concurrency is two calls then two awaits

```kite
use std/task

async fn fetch(name: str, ms: int) -> str {
    await task.sleep(ms)
    return "body of \(name)"
}

async fn main() {
    // Sequential: 150ms. The second call is not reached until the first
    // await returns.
    let a = await fetch("alpha", 100)
    let b = await fetch("beta", 50)

    // Concurrent: 100ms. Both tasks exist before either is awaited.
    let ta = fetch("gamma", 100)
    let tb = fetch("delta", 50)
    let (c, d) = await task.both(ta, tb)

    io.print("\(a) \(b) \(c) \(d)")
}
```

### `await` needs `async`

```kite fails
async fn work() -> int {
    return 7
}

fn main() {
    let v = await work() //~ E0521
    io.print(v)
}
```

The fix is `async fn main()`. `E0521` also fires on `task.yield()` in a
non-`async` function, for the same reason.

`await` is a **prefix unary operator that binds tighter than any binary
operator**: `await f() + 1` parses as `(await f()) + 1`.

### `check` in an async function

`check` needs a fallible *enclosing* function, and `main` cannot be one, so the
outermost `await` is destructured and tested by hand.

```kite
use std/task

async fn fetch(id: int) -> (str, error) {
    if id < 0 {
        return _, errors.new("bad id")
    }
    await task.sleep(1)
    return "user \(id)", nil
}

async fn greet(id: int) -> (str, error) {
    let (name, err) = await fetch(id)
    check err
    return "hello, \(name)", nil
}

async fn main() {
    let (line, err) = await greet(7)
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(line)
}
```

Note the shape of the type: `async fn fetch(id: int) -> (str, error)` produces
`Task<(str, error)>`, and `await` on it yields the pair, which is then
destructured. The taint analysis (`E0302`) still applies across the `await`.

### `Task<T>` in a signature

```kite
use std/task

async fn work(n: int) -> int {
    return n
}

fn later(n: int) -> Task<int> {
    return work(n)
}

async fn main() {
    let t: Task<int> = work(3)
    let queue: [Task<int>] = [later(1), later(2)]
    let rest = await task.all(queue)
    io.print("\(await t) then \(rest.len())")
}
```

---

## 2. The seven primitives and `std/task`

Six `task.*` names and `time.now` are **compiler builtins**, reachable with no
`use` at all. Everything else in `std/task` is ordinary Kite written over them,
and needs `use std/task`.

| Builtin | Arity | Meaning |
|---|---|---|
| `task.yield()` | 0 | Suspend and let another task run. `async` only, else `E0521`. |
| `task.park()` | 0 | Declare there is nothing to wait for but another task finishing, so a poll loop is not mistaken for progress. |
| `task.wake_at(ms)` | 1 | Ask the scheduler to resume no earlier than this absolute millisecond. |
| `task.wait_host()` | 0 | Suspend until the host hands something back. |
| `task.finished(t)` | 1 | `bool` — has the task produced a value? |
| `task.get(t)` | 1 | `T` — the task's result slot, read without suspending. |
| `time.now()` | 0 | `int` milliseconds. Virtual under the bytecode VM and the web glue. |

`task.get` is a raw slot read and **does not check**. On an unfinished task it
yields `nil`, whatever `T` claims. The read itself does not trap and `io.print`
of it prints `nil`, so the mistake stays silent right up to the first use of the
value *as* its type: a field access or a `\(…)` on a `nil` slot traps, and traps
are not catchable. Always guard it:

```kite
async fn work() -> int {
    return 5
}

async fn main() {
    let t = work()
    if task.finished(t) {
        io.print(task.get(t))
    } else {
        io.print("not yet: \(await t)")
    }
}
```

The library combinators, all `async`, all in `std/task`:

| Function | Type | Notes |
|---|---|---|
| `both(a, b)` | `Task<A>, Task<B> -> (A, B)` | Two of different types. |
| `all(tasks)` | `[Task<T>] -> [T]` | In the order given, not the order they finished. |
| `race(tasks)` | `[Task<T>] -> T` | **The losers are not cancelled.** They keep running. |
| `timeout(work, ms)` | `Task<T>, int -> Option<T>` | `nil` on expiry; the task itself is not stopped. |
| `sleep(ms)` | `int -> ()` | Virtual clock: a sleeping program costs no real time under `kitec test`. |
| `parallel(items, f)` | `[T: Share], fn(T) -> U: Share -> [U]` | **Not parallel on any target.** See below. |
| `scope(tasks)` | `[Task<T>] -> [T]` | `all` under a name that says a scope cannot be left with work running. |

There is no channel type. A `Task<T>` *is* the one-shot result channel, and it
is awaited rather than received from.

Reaching a library combinator without the import is `E0111`, and the diagnostic
names the module rather than the function:

```kite fails
async fn main() {
    await task.sleep(5) //~ E0111
    io.print("slept")
}
```

versus:

```kite
use std/task

async fn main() {
    await task.sleep(5)
    io.print("slept")
}
```

`task.yield()` in that same program needs no `use`, because it is a builtin.

---

## 3. `Share`

`Share` is declared in `std/prelude` as an empty trait, but **nobody implements
it** — the compiler answers structurally. It is an ordinary bound you can write
on your own generic function, which is the easiest way to see the rule.

A type is `Share` when it is:

- `int`, `float`, `bool`, `str`, `()`, `error`;
- a slice, `Option`, map, tuple or `(T, error)` of `Share` elements;
- a struct or enum **all of whose fields are non-`var` and `Share`**;
- `sync.Mutex<T>` or `sync.Atomic` — **by name**, whatever `T` is, unless `T`
  holds a `JsValue`: a lock settles races, not which isolate a host reference
  belongs to.

A type is **not** `Share` when it is:

- a struct or enum with a `var` field anywhere in its transitive structure;
- a `JsValue`, or anything holding one;
- **a function or closure type** (`fn(int) -> int`) — it may have captured a
  `var` binding;
- **a `dyn Trait`** — the concrete type is not known at the bound;
- **a generic parameter `T` without a `Share` bound** — it could be anything.
  Declaring `T: Share` makes it `Share`, so a `Share`-bounded function may pass
  its `T` on to another:

```kite
fn send<T: Share>(value: T) -> T {
    return value
}

fn pass<T: Share>(v: T) -> T {
    return send(v)
}

fn main() {
    io.print(pass(1))
}
```

```kite fails
fn send<T: Share>(value: T) -> T {
    return value
}

fn pass<T>(v: T) -> T {
    return send(v) //~ E0520
}

fn main() {
    io.print(pass(1))
}
```

**A function handed to a `Share`-bounded parameter takes its captures with it**,
so every capture must be `Share` too — `task.parallel(xs, |x| bump(c) + x)` over a
`c` with a `var` field is `E0520`, and so is a closure holding a `JsValue`. A
function value read from a binding cannot have its captures checked there, and
is refused the same way; write the closure literal, or name a function, where it
is handed over.

```kite fails
fn send<T: Share>(value: T) -> T {
    return value
}

struct Counter {
    var hits: int
}

fn main() {
    let c = send(Counter{ hits: 0 }) //~ E0520
    io.print(c.hits)
}
```

The diagnostic points at the offending field, not just the type. The two fixes
are to drop the `var` and return a new value, or to wrap it:

```kite
use std/sync

fn send<T: Share>(value: T) -> T {
    return value
}

struct Counter {
    hits: int
}

fn main() {
    // immutable: Share without doing anything
    let c = send(Counter{ hits: 0 })
    io.print(c.hits)

    // or synchronised: Share by name, even guarding a type that is not
    let guarded = send(sync.mutex(1))
    io.print(sync.load(sync.atomic(2)))
    io.print(sync.is_held(guarded))
}
```

The `sync` exemption is by name — `sync.Mutex` and `sync.Atomic` after
stripping type arguments. A home-made lock with the same shape is judged
structurally and fails:

```kite fails
fn send<T: Share>(value: T) -> T {
    return value
}

struct Padlock {
    var value: int
    var held: bool
}

fn main() {
    let p = send(Padlock{ value: 0, held: false }) //~ E0520
    io.print(p.value)
}
```

Because struct fields are immutable unless marked `var`, most user types are
`Share` and their authors never learn the trait exists. It only appears as
`E0520`.

### `sync`

`sync.Mutex<T>` holds the value *inside* the lock, so there is no way to name
what it guards without taking it. Under a cooperative scheduler this is not
theatre: any read-modify-write that spans an `await` or a `task.yield()` can
lose an update today, on one thread.

```kite
use std/task
use std/sync

async fn bump(var m: sync.Mutex<int>) {
    await sync.update(m, |n: int| n + 1)
}

async fn main() {
    let m = sync.mutex(0)
    let a = bump(m)
    let b = bump(m)
    let done = await task.both(a, b)

    let n = await sync.lock(m)
    io.print(n)
    sync.release(m, n)

    let counter = sync.atomic(1)
    io.print(sync.add(counter, 4))
    io.print(sync.compare_swap(counter, 5, 9))
    io.print(sync.swap(counter, 0))
}
```

`sync.update(m, f)` is the shape to reach for: `f` is a plain `fn`, so it cannot
suspend, so the read-modify-write cannot interleave. `try_lock`/`lock` +
`release` is the manual form and can be got wrong.

### Parallelism

No target runs two Kite tasks on two cores today, and `task.parallel` walks its
input in order, yielding between items. SPECIFICATION.md §12.2 says so itself
after correcting an earlier table. `Share` is enforced anyway so that the same
source becomes parallel when WasmGC's shared-everything-threads proposal ships,
with no rewrite.

---

## 4. Modules

**A module is a directory, or a single `.kite` file.** Every `.kite` file in a
module directory contributes to one namespace; the files are read in sorted
order and there are no per-file imports of siblings. Neither form is a module
until a `use` names it — a `.kite` file nothing imports is never compiled.

**The file you hand `kitec` is a one-file module, and the directory it sits in
is not a module at all.** Only a directory a `use` *names* is read whole. A
sibling `.kite` file beside the entry is its own one-file module and is reached
the same way everything else is — sharing a directory shares nothing. The
failure lands on the call, not on the layout:

```kite fails
// src/main.kite. `pub fn greeting() -> str` sits in src/words.kite, beside it,
// which changes nothing: this is the diagnostic with and without that file.
fn main() {
    io.print(greeting()) //~ E0111
}
```

Qualifying it does not help — `words.greeting()` is `cannot find 'words'`,
because nothing loaded `words`. One line fixes both:

```kite ignore
// src/main.kite
use words

fn main() {
    io.print(words.greeting())
}
```

A flat `src/*.kite` layout — one file per module, no subdirectories — is what
the production Kite applications use, and `kitec` builds it: verified on a 15-file
`src/` where `kitec build src/pos.kite --emit wasm` resolves the eight sibling
modules the entry names and everything they import in turn. What `kitec` will
not do is merge them. Web projects reach for `vite-plugin-kite` for the Vite
integration, not for a different module model — it hands the compiler each
sibling keyed by its filename without the extension, which is exactly the name
`use words` asks for.

```
myapp/
  kite.toml
  src/
    main.kite          // the entry — a module by itself; `src/` is not one
    money.kite         // a module, one file — needs `use money`
    config/
      load.kite
      schema.kite      // the same module as load.kite
```

```kite ignore
use config
use money
use std/json as j

fn main() {
    let (cfg, err) = config.load("app.toml")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(money.render(config.budget(cfg)))
}
```

### Paths are relative to the importing file's directory

Not to the project root, and not to the entry file. A file at `src/a/x.kite`
writing `use config` is asking for `src/a/config/` or `src/a/config.kite`. The
`E0400` diagnostic prints both paths it tried, which is the fastest way to see
this.

The one exception is the first segment: if it names a declared dependency, the
path roots there instead (§5).

### `use` is per module, not per file

Every file in a module directory shares the module's imports and aliases. One
file writing `use config` is enough for its siblings to write `config.load`.
SPECIFICATION.md §13.1 says "one *file* may not spell two modules alike"; the
compiler's granularity is the module — two different files in one directory
importing `utils` and `dep/utils` is `E0404`, and the message reads "already
names another module **here**".

### A module reaches only what it imports

This is the rule most likely to surprise. Declarations are merged under
qualified names, but a qualified name resolves in a file only if *that file's
module* has a `use` for it — one exception, below. Another module importing
`config` does not make `config.load` exist for the whole program.

Below, `json` is never loaded at all, so the first program fails whichever
module it is in:

```kite fails
fn main() {
    let (v, err) = json.parse("{}") //~ E0111
    io.print(err == nil)
}
```

```kite
use std/json

fn main() {
    let (v, err) = json.parse("{}")
    io.print(err == nil)
}
```

Without the rule, a dependency loading `config` anywhere would put `config.load`
in scope everywhere, and a dependency could reach the importing program's own
modules just by naming them.

**The entry file is held to it too.** It was exempt for a while — its
declarations are the only unqualified ones, so its "own" `json.parse` was
module `json`'s, and anything *any* module had loaded was reachable from it
with no `use` of its own. That is gone: below, `other` imports `std/json`, and
`main.kite` still has to.

```kite ignore
// src/other/o.kite
use std/json
pub fn go() -> bool {
    let (v, err) = json.parse("{}")
    return err == nil
}
```

```kite ignore
// src/main.kite — error[E0111]: cannot find `json`, until it says `use std/json`
use other

fn main() {
    let (v, err) = json.parse("{}")
    io.print(err == nil)
}
```

An alias is the same: after `use std/math as m`, `m.floor(1.5)` resolves and
`math.floor(2.5)` does not, in the entry file as everywhere else.

**Unqualified variants are the module's own.** A bare `Circle` — in an
expression or a pattern with a payload — is one of the asking module's own
enums' variants, or the prelude's. Another module's variants are written
qualified, `shapes.Shape.Circle`, imported or not. (A bare *unit* variant in a
`match` arm is still read against the scrutinee's own enum.)

### Imports are always qualified

There is no wildcard import and no way to bring a bare name into scope.
`config.load` always says where `load` came from. Types are qualified too:
`let one: inventory.Item = inventory.item("spare", 5, 1)`.

### An alias belongs to the module that writes it

`use std/math as m` makes `m` mean `math` in that module and nowhere else.
Writing `m.round` in a module that did not alias it is `E0111`. This matters for
supply chain reasons: aliases used to share one program-wide table, so
`use leak as crypto` written inside a dependency rewrote every `crypto.…` call
in the importing program with no diagnostic.

### A module is where its source is — except for `std`

A module is identified by the file or directory it was loaded from, and named
by its path within its package: `a/util` for `a/util.kite` beside the entry,
`md/util` for `util.kite` inside a dependency called `md`. That is the name a
diagnostic uses — ``private to module `a/util` ``. So `use util` inside `a/`
and `use util` inside `b/` are two modules, each spelled `util` in the file
that imported it; a nested `lib/x` beside a top-level `x` is not a cycle; and a
module whose `util` does not exist gets `E0400` rather than whichever `util`
somebody else loaded first.

`use dep/utils` and `use utils` are two different modules, and every segment is
honoured when the files are found. Two spellings coexist in one file as long as
one is aliased:

```kite ignore
use utils                   // `utils.…` is this one
use dep/utils as theirs     // `theirs.…` is that one
```

`std` is the exception: `use std/json` has identity `json`, because that is how
every program writes it. A standard module is exactly `std/<name>` —
`use std/a/b/json` is `E0400`.

### `E0403` — the standard library's names are reserved

The twenty names in the compiler's `STD_MODULES` table, and `prelude`:

```
buffer  canvas  crypto  dom   errors  fmt   fs    html   http  js
json    math    socket  sync  task    test  text  time   toml  window
prelude
```

A non-`std` module may not take one. `use crypto` naming a sibling directory is
`E0403` before the file system is even consulted. `prelude` is reserved because
the prelude is found by that name from everywhere: a module called `prelude`
used to compile, and its declarations — private ones included — became every
module's unqualified fallback.

```kite fails
use crypto //~ E0403

fn main() {
    io.print(1)
}
```

One correction to SPECIFICATION.md §13.1 here: **the check is on the last
segment regardless of depth**, so `use dep/crypto` is *also* `E0403`, although
the spec says "full paths keep `dep/crypto` and `std/crypto` apart on their
own". A dependency whose module is named after any of the reserved names is
unreachable; rename it. (`use std/prelude` is `E0400`: the prelude is ambient,
not a module you import.)

### `E0111` blames the module even when the module is fine

The caret lands on the module segment and the message is `cannot find 'dom'` in
both cases: `dom` was never imported, **and** `dom` was imported and the
*member* does not exist. A misspelled function therefore reads like a broken
import, and the `use` line is the wrong place to look first.

```kite fails
use std/dom

fn main() {
    let node = dom.find("#row")
    if node == nil {
        return
    }
    io.print(dom.tag_of(node)) //~ E0111
}
```

`std/dom` is imported and loaded here; `tag_of` simply does not exist — `dom`
has no tag accessor at all, and `dom.text` is what reads an element:

```kite
use std/dom

fn main() {
    let node = dom.find("#row")
    if node == nil {
        return
    }
    io.print(dom.text(node))
}
```

Same for a user module: with `use m`, where `m` exports `one` and not `two`,
`m.two()` is `cannot find 'm'` with the caret on `m`. Check the member against
the module's `pub fn` list before you touch the import.

### The other module codes

| Code | Meaning |
|---|---|
| `E0400` | Module not found. Prints the directory and the `.kite` file it looked for. Also used for `use std/<not-a-std-module>`, and then lists the whole standard library. |
| `E0401` | The item is not `pub`, so it is visible only inside its own module. Applies to functions, types (in expressions and in signatures), enum variants, struct fields, methods and associated functions. |
| `E0406` | An `impl` outside its type's module: an extension method, or an imported trait implemented for an imported type. |
| `E0402` | Import cycle. The diagnostic prints the chain (`a → b`). Extract the shared part into a third module. |
| `E0403` | Reserved standard-library name (above). |
| `E0404` | One module spelling two modules alike — give one an alias — or two packages of one name reached from two different places. |
| `E0405` | A `kite.toml` that does not read. Reported at the offending line; a build no longer treats a broken manifest as no manifest. |
| `E0111` | A qualified name whose module this module never imported — **or** an unknown member of one it did (above). |

**An unmarked field makes a `pub struct` opaque outside its module**
(SPECIFICATION.md §4.3, §15.4): reading or writing it, building the struct with a
literal or a `..base` update, and destructuring it are all `E0401`. That is what
makes an opaque wrapper — `crypto.Key`, a `JsValue` in a struct — something the
module that owns it can rely on.

---

## 5. Packages

### `kite.toml`

The reader is a deliberate TOML subset: tables, inline tables, string values,
nothing else. No arrays, no numbers, no nesting beyond one inline level.

```toml
[package]
name    = "myapp"
version = "0.1.0"

[targets]
web    = { entry = "src/main.kite", renderer = "dom" }
native = { entry = "src/main.kite" }

[dependencies]
markdown = { git = "https://github.com/example/kite-markdown", tag = "v1.2.0" }
local    = { path = "../local" }
solver   = { git = "https://github.com/example/solver", version = "^1.2" }
shortcut = "../shortcut"
```

- `[package]` accepts `name` and `version` and nothing else, and the version
  must be a version (`1.2.0`, no leading zeros).
- A target needs an `entry`; `renderer` is optional; nothing else.
- A dependency needs **exactly one** of `path` or `git`. A bare string value is
  shorthand for `path`. It takes `path`, `git`, `tag` and `version` and nothing
  else — a misspelt `verison` or a `branch` is an error, not something dropped —
  and a `tag` goes only with `git`.
- `tag` and `version` are mutually exclusive: a tag pins, a version resolves.
- Names — package and dependency alike — are identifiers: ASCII letters,
  digits and `_`, not starting with a digit, at most 64. The name is written in
  a `use`, so `kite-md` could never be imported, and it becomes a directory
  under `.kite/vendor`, so a `/` or a `..` in one would escape that directory —
  and since a *transitive* manifest introduces names, the escaping name need
  never appear in a manifest anybody wrote.

The program's manifest is looked for **upwards** from the entry file, so
`src/main.kite` finds the `kite.toml` beside `src/`. A manifest that does not
parse is `E0405` at its line, in `kitec run`/`check`/`build` as well as in
`kitec pkg` — and so is one that does not read at all: a single Latin-1 byte
in a comment is reported at its line as not UTF-8, not taken for no manifest.

### Reaching a dependency

A `use` whose **first segment** names a declared dependency roots there. A
declared dependency wins over a sibling directory of the same name.

```kite ignore
use markdown/render        // the `render` module inside the `markdown` package
use markdown               // the package's own root module — its top-level .kite files
```

Verified end to end: with `markdown = { path = "../markdown" }` in
`app/kite.toml`, a `render/` directory and a top-level `lib.kite` in the
package, `kitec run app/src/main.kite` resolves both `render.to_html` and
`markdown.name` — and does so even when `src/markdown/` also exists beside the
entry file, because what a package depends on is what it declared rather than
what happens to be lying next to it. `git` dependencies
resolve to `<manifest dir>/.kite/vendor/<name>`, which is where `kitec pkg` put
them; nothing is fetched during a build.

**Each package's dependencies are its own.** A module inside a dependency
resolves `use` against *that* package's `kite.toml` — the one in the
dependency's own directory, never one above it — so a package can use what it
declares and cannot use what only the program declared. There is no hoisting.
A `path` in a dependency's manifest is relative to that manifest; a `git`
dependency any package declares is read from the program's
`.kite/vendor/<name>`, where `kitec pkg` puts every package in the graph. A
package's unqualified `use helper` names the package's own `helper` and nothing
else: without one it is `E0400`, never the application's.

A package **only a dependency declares** does not take its name from the
program. An application with its own `log.kite` may depend on a `web` package
that depends on a `log` package: the application's `use log` is its own file
and `web`'s `use log` is the package, in either order. A package the program
*does* declare is one package everywhere — two manifests naming it from two
places is `E0404`, as `kitec pkg` refuses it.

### `kitec pkg` and the lockfile

```
kitec pkg [directory]       resolve versions, write kite.lock
kitec pkg --update          accept changed bytes and rewrite the lockfile
kitec pkg --offline         resolve only from what is already vendored
```

The lockfile records a **SHA-256** over every `.kite` file in the dependency, by
sorted name, each field length-prefixed. That includes `node_modules/`, whose
name a `use` can write; only `.git` and `.kite/` are left out. A symbolic link a
build would follow — a `.kite` file, or a directory a `use` can name — is
refused:

```
[[locked]]
name = "markdown"
version = "1.2.0"
source = "../markdown"
hash = "ba29ae3668aa1ea41dd84ebc4490c1fd0f9c687d50f79945d83b755a2245981e"
```

It is **checked, not just written**, entry by entry. Change one byte of a
dependency without changing its version or its source and plain `kitec pkg`
fails, naming it:

```
error: `app/kite.lock` does not match what resolution produced
  a 1.0.0: <new hash> was <old hash>

note: a dependency's contents changed under the same version — a moved tag,
      a re-pushed repository, or something answering for one
note: run `kitec pkg --update` to accept the new bytes and rewrite the lockfile
```

`--update` prints `kite.lock changed — a dependency is not what it was`, and
fetches every git checkout again rather than trusting the one cached — beside
it, so a fetch that fails leaves the cached checkout, and `--offline`, as they
were. Anything
else that changed — a dependency added or removed, a new version because the
manifest now asks for one, a new source — is a line of output (`added b 1.0.0`,
`a 1.0.0 → 1.2.0`) and `kite.lock updated`, not an error.

The lockfile is also an input: resolution **tries the locked versions first**,
so a release published since is not taken until the manifest stops accepting
the locked one or `--update` asks. The
digest is cryptographic because the party it is aimed at chooses the bytes; it
was FNV-1a, which is invertible, so a suffix landing the digest on the recorded
value could be solved for rather than searched for.

**`kitec pkg` is the only place that check happens.** `build`, `run` and `test`
compile whatever is in `.kite/vendor` without consulting `kite.lock` — a
pipeline that wants the guarantee must run `kitec pkg` in it.

Absent by construction, not by policy: no post-install scripts, no build-time
code execution, no transitive hoisting. Dependency URLs are `https://` or
`ssh://` only, because a transitive manifest could otherwise opt a project into
cleartext it never chose.

---

## 6. Memory

Managed on every target. No manual allocation, no `free`, no ownership, no
moves, no borrowing, no lifetimes, no annotations.

| Target | Collector |
|---|---|
| `wasm32-gc` | The host engine's — V8, SpiderMonkey, JavaScriptCore. Kite ships no collector in the binary. |
| `native-*` | Precise tracing, generational: the nursery moves survivors into an old generation that does not move. |
| `kbc` (bytecode VM) | Reference counting. A cycle of references is never freed while the program runs — harmless in a test, a leak in a long-running embedding. |

Three consequences of WasmGC's shape, accepted deliberately:

- **No interior pointers.** A reference always points at an object's head. Kite
  has no `&x.field`, so this is unobservable.
- **No unboxed aggregates inside arrays.** `[Point]` is an array of references
  to `Point` objects, not a flat `(f64, f64)` buffer. `std/buffer` is the escape
  hatch — but note that `buffer.F64` is **implemented over a `[float]` slice**,
  not over linear memory as SPECIFICATION.md §14 claims; the Wasm backend has no
  linear memory today. Its `values` field is `pub var`, which also means
  `buffer.F64` is not `Share`.
- **No weak references and no finalizers.** A cache that must not retain its
  entries needs an explicit eviction policy.

### Exclusivity — `E0800`

Collection settles safety. It does not settle the one hazard reference
semantics introduce on their own: the same object arriving at one call under two
names, where a write through one is invisible to the other.

```kite fails
struct Account {
    var balance: int
}

fn transfer(var from: Account, var to: Account, amount: int) {
    from.balance = from.balance - amount
    to.balance = to.balance + amount
}

fn main() {
    let a = Account{ balance: 100 }
    transfer(a, a, 50) //~ E0800
    io.print(a.balance)
}
```

The rule: **while an object is being written through one argument, no other
argument of the same call may name it.** Precisely:

- Two arguments name the same object when **one path is a prefix of the other**,
  so `f(o, o.inner)` is rejected alongside `f(a, a)`.
- `f(o.left, o.right)` is accepted — neither path contains the other.
- **A literal index distinguishes elements**: `f(xs[0], xs[1])` is accepted and
  `f(xs[i], xs[j])` is rejected, because the compiler cannot show `i != j` and
  the call is wrong on the run where they are equal.
- Only **reference types** participate — structs and `dyn` values. Slices, maps
  and tuples are copy-on-write, so a `var [T]` parameter is the callee's own
  copy.

```kite
struct Account {
    var balance: int
}

struct Pair {
    left: Account
    right: Account
}

fn transfer(var from: Account, var to: Account, amount: int) {
    from.balance = from.balance - amount
    to.balance = to.balance + amount
}

fn main() {
    let a = Account{ balance: 100 }
    let b = Account{ balance: 0 }
    let xs = [a, b]

    transfer(a, b, 50)              // distinct names
    transfer(xs[0], xs[1], 10)      // literal indices
    let p = Pair{ left: a, right: b }
    transfer(p.left, p.right, 10)   // neither path contains the other

    io.print("\(a.balance) \(b.balance)")
}
```

**This is not borrowing.** It reads one call site and reports only what is
written there. Aliasing arranged elsewhere is not detected and compiles:

```kite
struct Account {
    var balance: int
}

struct Pair {
    left: Account
    right: Account
}

fn transfer(var from: Account, var to: Account, amount: int) {
    from.balance = from.balance - amount
    to.balance = to.balance + amount
}

fn main() {
    let shared = Account{ balance: 100 }
    let pair = Pair{ left: shared, right: shared }
    transfer(pair.left, pair.right, 50)   // accepted — and still the same bug
    io.print(shared.balance)
}
```

Seeing through that assignment is alias analysis, which is the rest of a borrow
checker. The bug left behind is a wrong number, never a wrong address.

Two rules a Rust programmer expects are absent because value semantics settle
them: `for x in xs` walks a snapshot, so growing `xs` in the body terminates;
and a slice passed to a function is copied, so a `push` inside is not
observable by the caller.

### `ptr.same`

`ptr.same(a, b)` is a builtin (no `use`) asking whether two names are one heap
cell. Structs, enums and maps have such a cell. Numbers, strings, `bool`s,
functions and `dyn` values do not, and slices are excluded because they are
copy-on-write — two sharing a buffer is an allocator fact that a write would
end. Anything else is `E0213`, and a type mismatch between the two arguments is
`E0200`.

```kite fails
struct Model {
    count: int
}

fn main() {
    let a = Model{ count: 1 }
    let b = Model{ count: 1 }
    io.print(ptr.same(a, b))
    io.print(ptr.same([1], [1])) //~ E0213
}
```

---

## 7. The host boundary

The web target has no direct DOM access — no ratified Wasm proposal calls Web
IDL without JavaScript glue — so Kite defines the boundary explicitly.

### `JsValue`

A host reference. On the web it lowers to `externref`. Asked for an artefact
that cannot hold one, the compiler refuses rather than inventing a
representation: `--emit kbc`, `--emit native` and `run --native` all report
**`E0204`** — "`JsValue` is a host object, and this target has no host". Three
paths deliberately do not: `kitec check` (a web program is valid Kite and must
keep checking in an editor), and plain `kitec run` and `kitec build`, which
trap at the first host call instead — see the end of this section.

| Property | Consequence |
|---|---|
| Opaque | Kite cannot read inside it. |
| Not `Share` | It belongs to one isolate. A struct holding one is not `Share` either — `E0520`. |
| Not comparable with `==` | `externref` is outside Wasm's `eq` hierarchy. `E0201`. |
| Cannot be forged | There is no literal for it. |

```kite fails
@host("js")
extern fn global(name: str) -> JsValue

fn main() {
    let a = global("document")
    let b = global("window")
    io.print(a == b) //~ E0201
}
```

The wrapper is refused too: a struct whose only field is a `JsValue` also
rejects `==`, because comparing it would mean walking a field the compiler
cannot see. Identity is `js.same(a, b)`, which is `===`.

Lifetime needs no rule. On the web the Wasm heap *is* the JavaScript heap, so a
Kite struct holding an element, whose listener holds a Kite closure, which holds
the struct, is a cycle the one collector collects. There is no release call and
no handle table.

### `extern` and `@host`

```
ExternDecl = HostAttr "extern" "fn" Ident "(" [ Params ] ")" [ "->" Type ] ;
HostAttr   = "@" "host" "(" StringLit ")" ;
```

`@host("…")` is **mandatory** — a bare `extern fn` is a parse error, not a
missing-attribute diagnostic. There is no body. The string names the host
namespace the glue must supply.

```kite
use std/js

@host("net")
extern fn connect(host: str, port: int) -> JsValue

pub struct Socket {
    raw: JsValue
}

pub fn open(host: str, port: int) -> Socket {
    return Socket{ raw: connect(host, port) }
}

pub fn raw(s: Socket) -> JsValue {
    return s.raw
}

fn main() {
    let s = open("example.com", 443)
    io.print(js.kind_of(raw(s)))
}
```

`extern` is direct, monomorphic and checked at the call. `std/fs`, `std/http`,
`std/socket` and `std/crypto` are built from it. Drawing does not use it at all
— the drawing calls are compiler builtins.

A program that `check`s fine will still **trap at run time under a host that
supplies no such namespace**, and traps are not catchable:

```
error: `js.js_global` is a host function, and this runtime supplies no host
note: traps are not catchable; Kite has no `recover`
```

So `kitec check` is the right verification for host code outside the browser.

### `std/js`

`std/js` declares nothing of its own to the host beyond a fixed set of
primitives — 29 `@host` externs behind 33 exported names — through which any
host object is reachable. `std/dom` and
`std/window` are ordinary Kite written over them (and `std/html` over
`std/dom`), and so is anything the standard library never wrapped.

| Group | Names |
|---|---|
| Roots | `js.global(name)`, `js.nothing()` |
| Properties | `js.get(v, name)`, `js.set(v, name, x)` |
| Array-likes | `js.at(v, i)`, `js.length(v)` |
| Methods | `js.call0(v, name)` … `js.call4(v, name, a, b, c, d)` |
| Construction | `js.new0(name)` … `js.new3(name, a, b, c)` |
| Callbacks | `js.func(f)` |
| Promises | `js.settle(p, done, failed)` |
| Asking | `js.same(a, b)`, `js.is_nothing(v)`, `js.kind_of(v)`, `js.instance_of(v, name)` |
| Into JS | `js.of_str`, `js.of_num`, `js.of_bool`, `js.of_int` |
| Out of JS | `js.as_str`, `js.as_num`, `js.as_bool`, `js.as_int` |
| Shorthands | `js.str_or(v, name, fallback)`, `js.num_or`, `js.bool_or` |
| Constant | `js.SAFE_INTEGER()` |

Arities are spelled out rather than taking an argument list because a slice is a
Kite aggregate and does not cross the boundary.

Fallibility is not uniform, and guessing it wrong is `E0200` either way —
destructuring something that is not `(T, error)`, or using a pair as a value.
Three groups:

- **Infallible, plain value.** `js.global`, `js.nothing` — reading a missing
  property in JavaScript yields `undefined`, which is not an error; ask
  `js.is_nothing`. Also `js.same`, `js.is_nothing`, `js.kind_of`, `js.of_str`,
  `js.of_num`, `js.of_bool`, the three `*_or` shorthands, and `js.SAFE_INTEGER`.
- **A bare `error`.** `js.set` and `js.settle`, because a `(unit, error)` pair
  would force every call site to destructure nothing.
- **`(T, error)`.** Everything that can throw or can be the wrong type:
  `js.get`, `js.at`, `js.length`, `js.call0`…`js.call4`, `js.new0`…`js.new3`,
  `js.instance_of`, `js.of_int`, and the four `js.as_*`.

### `js.func` and its arity ceiling

`js.func` is a **compiler builtin**, not a function in `std/js` — it needs no
`use`, and it keeps its full path even inside `std/js` itself. It takes a
closure of **up to four `JsValue` parameters**, answering with a `JsValue` or
with nothing, and the compiler emits a wrapper of that exact arity.

```kite ignore
js.func(|| { … })                                    // a timer, a microtask
js.func(|e: JsValue| { … })                          // a listener
js.func(|entries: JsValue, obs: JsValue| { … })      // an observer
js.func(|a: JsValue, b: JsValue| -> JsValue { … })   // a comparator
```

A fifth parameter is a compile error rather than an argument that silently
disappears, and so is any parameter or result that is a Kite aggregate:

```kite fails
use std/js

fn main() {
    let f = js.func(|a: JsValue, b: JsValue, c: JsValue, d: JsValue, e: JsValue| { //~ E0200
        io.print("five")
    })
    io.print(js.kind_of(f))
}
```

The return value is what makes `sort`, `map`, `filter` and a `Promise` executor
reachable.

### Promises: `js.settle`

Promises already work through `js.call1(p, "then", js.func(…))`. What
`js.settle` adds is that **both halves are required**: `then` with one callback
compiles, runs, and throws the rejection away.

```kite
use std/js

fn load(url: str) -> error {
    let (p, err) = js.call1(js.global("globalThis"), "fetch", js.of_str(url))
    check err
    return js.settle(p,
        |response: JsValue| { io.print(js.str_or(response, "url", "?")) },
        |why: str| { io.error("could not load: \(why)") },
    )
}

fn main() {
    let e = load("/data.json")
    io.print(e == nil)
}
```

The failure arrives as `str`, not `error`, because an `error` is either nil or a
failure and this callback only runs when something went wrong. Write
`errors.new(why)` if you need to propagate one.

### Everything catches

A host exception must never cross the boundary raw. The `@host` externs behind
`std/js` are module-private; each returns a **marker object** carrying the
message, made fresh per throw, and the public wrapper turns it into an `error`.
The taint analysis then makes the check mandatory — so dropping it is `E0302`,
not a silent failure:

```kite fails
use std/js

fn main() {
    let (node, err) = js.call1(js.global("document"), "querySelector", js.of_str("#form")) //~ E0302
    io.print(js.kind_of(node))
}
```

```kite
use std/js

fn find(selector: str) -> (JsValue, error) {
    let (node, err) = js.call1(js.global("document"), "querySelector", js.of_str(selector))
    check err
    return node, nil
}

fn main() {
    let (node, err) = find("#form")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(js.kind_of(node))
}
```

This is the difference between a mistyped method name failing one call and a
thrown exception unwinding through the Wasm frames and taking every running task
with it.

It also removes JavaScript's commonest bug by construction: `undefined`
becoming `0` or `NaN` somewhere later is untraceable, whereas `js.as_num`
returns an error that must be tested before the number is usable.

### Numbers and absence

Numbers cross as `f64`, because a JavaScript number *is* an `f64` and a Kite
`int` is an i64 — every crossing would otherwise allocate a BigInt. So:

- `js.of_int(n)` returns `(JsValue, error)` and **refuses rather than rounds**
  when `|n| > js.SAFE_INTEGER()` (9007199254740991).
- `js.as_int(v)` asks the host's own `Number.isSafeInteger` and errors on a
  fractional or oversized value. The value becomes an `int` only once the answer
  is yes.
- `js.as_str`, `js.as_num` and `js.as_bool` each check `typeof` first and return
  an error naming what they saw.

`Option<T>` is the *wrapper* layer's shape for absence, not `std/js`'s: nothing
in `std/js` returns one, because absence there is `js.nothing()` and the
question is `js.is_nothing`. It is `dom.find`, `dom.body`, `dom.attribute`,
`dom.target` and `window.parameter` that answer `Option<T>`, and that is the
shape to copy when wrapping. There is no tolerated zero handle and no null
object anywhere in the boundary.

### The other direction: `api.js` and `api.d.ts`

Everything above is Kite reaching out. `kitec build … --emit wasm --out dist`
also writes `dist/api.js` and `dist/api.d.ts` beside `app.wasm` and `app.js`
— the typed door **in**, for JavaScript calling Kite. They appear when the entry
file has at least one `pub fn` of its own (a `pub struct` alone produces
nothing), and the contract they implement is narrow:

| Kite | TypeScript | On the wire |
|---|---|---|
| `int` | `bigint` | a JavaScript `BigInt`. `int` is 64-bit and a `number` would lose the top eleven bits silently |
| `float` | `number` | `f64`, untouched |
| `bool` | `boolean` | an i32 — the wrapper writes `b ? 1 : 0` going in and `!== 0` coming out |
| `str` | `string` | through `str()` / `text()`, the module's Unicode-scalar helpers |
| no return | `void` | |

**Nothing else crosses.** A `pub fn` whose parameters or result mention a
struct, an enum, a slice, a map, `Option<T>`, `(T, error)`, a bare `error`,
`Task<T>` (so: every `pub async fn`) or `JsValue` is **omitted from both files**,
with no diagnostic — the build succeeds and the reason is printed into the
generated files by name.

```kite
use std/math

pub fn with_tax(cents: int, rate: float) -> int {
    return cents + math.round_to(cents as float * rate)
}

pub fn label(name: str, settled: bool) -> str {
    if settled {
        return name
    }
    return "\(name) (pending)"
}

pub fn lines(names: [str]) -> int {
    return names.len()
}

fn main() {
    io.print(with_tax(100, 0.07))
    io.print(label("order", true))
    io.print(lines(["a"]))
}
```

The exported half of the `api.js` that comes out of it, verbatim — above it the
file declares `load(source)` and the `$kiteReady()` guard, and imports the
module's string helpers as `$kiteStr` and `$kiteText`:

```js
export function with_tax(cents, rate) {
  return $kiteReady().with_tax(cents, rate);
}

export function label(name, settled) {
  return $kiteText($kiteReady().label($kiteStr(name), settled ? 1 : 0));
}

// Left out, because these take or answer with a type JavaScript has no
// representation for yet: lines.
// The module still exports them; describing them wrongly would be worse
// than not describing them.
```

The Wasm export for `lines` is still there — only the typed door is missing —
but calling it raw means reproducing the conversions by hand. Design the
boundary out of the five rows above.

Three more rules, each read off a generated file:

- **Only the entry file's own `pub fn`s.** A `pub fn` in a `use`d module is not
  in `api.js` at all, not even in the left-out note: `m.triple` is not a
  JavaScript identifier, and one bad `export function` name would take the whole
  file's parse with it.
- **`main` is never in the wrapper**, whatever its signature.
- **A parameter named as a JavaScript reserved word is renamed by position.**
  `pub fn label(new: str, class: int)` becomes `label(new_0, class_1)` in both
  files. Strict mode's words count too — `static`, `interface`, `private`,
  `eval`, `arguments` — because a module is always strict.
- **A `pub fn` named as a reserved word keeps its name.** `pub fn delete` is
  declared as `$kite_delete` and exported `as delete`, so
  `import { delete as del } from "./api.js"` works. The file's own bindings all
  begin `$kite`, which no Kite name can, so `pub fn text` and a parameter called
  `str` are ordinary names.
- **A `pub fn load` is left out**, with a note: `load` is the wrapper's own. The
  module still exports it — call it on the object `load()` answers.
- **A generic `pub fn` is left out**, with a note: the module has a copy per
  type it was used at and no one function to export.

Loading is explicit and once:

```js
import { load, with_tax } from "./api.js";

await load();            // the app.wasm beside api.js; also a URL or a Uint8Array
with_tax(100n, 0.07);    // => 107n
```

Calling anything before `load()` throws ``call `await load()` before calling
into the module``.

### Hygiene

`JsValue` is untyped, and if it reaches application code the type system has
stopped helping. Two conventions keep it in:

1. **Wrap it in a struct with unmarked fields**, so the value inside is reached
   only through the wrapping module — the compiler holds every other module to
   that (§4).
2. **Provide exactly one door out** — `dom.raw(e)` and `dom.wrap(v)`, greppable
   and documented. Sealing a wrapper completely is worse: the user who needs the
   one method nobody wrapped rebuilds a parallel untyped world beside the typed
   one.

`std/js` is a separate module precisely so that importing it is visible in a
file's first three lines. It carries the cultural marking Rust gives `unsafe`:
normal inside a module whose job is wrapping, a smell in an application's public
interface.

What is admitted: a mistyped property or method name **compiles** and fails at
run time. `extern` does not have that problem, which is why the standard library
uses it for the calls it makes often. The long-term exit is generating the typed
layer from the browser's own interface definitions.
