# The prelude, the builtins, the standard library and the toolchain

Everything below was checked against `target/release/kitec`. Where a file in the
repository says otherwise, the compiler won.

## Surprises first

- **`io` is not a module.** `use std/io` is `E0400`. `io.print` is a compiler
  builtin reached by dotted path with no import at all. So are `errors.new`,
  `time.now`, `text.from_code`, `js.func`, `assert`, `require`, `ptr.same`, and
  the whole of `draw.*` and `task.*`'s primitives.
- **`io.println` does not exist.** It is `io.print`, and the mistake is
  `E0111 cannot find io` — the whole head is reported, not the leaf.
- **One dotted head can be both a builtin and a module.** `errors.new` needs no
  import; `errors.wrap` needs `use std/errors`. Same for `time.now` vs
  `time.show`, `task.yield` vs `task.sleep`, `text.from_code` vs
  `text.bidi_runs`, `js.func` vs `js.global`. The builtin table is consulted
  *first*, so no module function can shadow a builtin of the same path — a
  sibling module of your own declaring `draw.rect` loses to the builtin.
- **A module's own imports land in your file too.** `use std/fs` also puts
  `errors.*` in scope, because `std/fs` imports it; `use std/http` brings
  `task.*`, `use std/dom` brings `js.*`, `use std/toml` brings `math.*` and
  `errors.*`. This is why the specification's Appendix A calls `errors.wrap`
  with no `use std/errors` in sight. Do not rely on it — write the `use` you
  mean; a second `use` of an already-visible module is not an error.
- **The prelude is unqualified and always present.** `map`, `filter`, `sorted`,
  `split`, `join`, `parse_int`, `or_else` are bare names. There is no
  `std/prelude` to import — `use std/prelude` is `E0400`.
- **`map` needs an annotated closure; `filter` does not.** `map`'s `U` is
  determined only by the closure's return, and Kite has no turbofish, so
  `map(xs, |n| n * 2)` is `E0209`. Write `map(xs, |n: int| n * 2)`.
- **Slices and maps have almost no methods.** A slice has exactly `len`, `get`
  (bounds-checked, `-> Option<T>`) and `push`; `xs[i]` traps out of range. A map
  has `len`, `keys`, `values`, and is read with `m[k]` (an optional) and written
  with `m[k] = v` — and a key cannot be removed at all. Everything else is a
  prelude *function*, because Kite has no extension methods and a slice takes
  methods only from the compiler.
- **`str` has exactly five methods** — `len`, `slice`, `index_of`, `trim`,
  `code_at`. `contains`, `starts_with`, `split`, `replace`, `lower`, `upper`
  are prelude functions taking the string as the first argument.
- **Web-only modules are `dom`, `window`, `html` and `js`.** They compile
  everywhere and trap at run time off the web. `fs` is the mirror image: native
  only.
- **`kitec run` supplies exactly one host group, `fs`.** A program calling
  `std/http`, `std/socket`, `std/crypto` or `std/js` type-checks and then traps
  with *"`X` is a host function, and this runtime supplies no host"*, naming the
  extern it reached: `net.fetch_start` for `http.get`, `net.serve_open` for
  `http.open`, `net.socket_open` for `socket.connect`, `crypto.digest_start` for
  `crypto.sha256`, `crypto.random_hex` for `crypto.random`, `js.js_global` for
  anything over `std/js`. Those need `--emit wasm` and the generated glue.
- **`--explain` knows 48 codes.** The ranges leave room for a thousand; the
  gaps are real, and a code nobody can provoke is deleted rather than kept to be
  explained. Any unknown code — `kitec --explain E0999` — prints the whole list.

## The prelude

`std/prelude.kite` is compiled into every program, unqualified. Unreached
functions are dropped before code generation.

### Traits

| Trait | Method | Notes |
|---|---|---|
| `Display` | `show(self) -> str` | What `io.print` and `\(x)` look for. **Not derivable** — by design. |
| `Debug` | `debug(self) -> str` | `@derive(Debug)` writes it from the fields. |
| `Error` | `message(self) -> str` | A type implementing it may be returned where `error` is expected. |
| `Hash` | `hash(self) -> int` | FNV-1a, `@derive(Hash)`. Not a security primitive. |
| `Share` | — (empty) | Nobody implements it. The compiler decides structurally: deeply immutable. Violations are `E0520`. |

The four derivable traits are `Debug`, `Hash`, `Encode` and `Decode`.
`Encode` is `json.Encode` and needs `use std/json`; `Decode` becomes an
*associated function* `T.decode(doc) -> (T, error)`, because a trait method
cannot return `Self`.

```kite
use std/json

@derive(Debug, Hash, Encode, Decode)
pub struct User {
    pub id: int
    pub name: str
}

impl Display for User {
    fn show(self) -> str {
        return "#\(self.id) \(self.name)"
    }
}

pub enum LoadError {
    Absent(path: str)
}

impl Error for LoadError {
    fn message(self) -> str {
        return match self {
            Absent(path) => "no user file at \(path)",
        }
    }
}

fn load(path: str) -> (User, error) {
    return _, LoadError.Absent(path: path)
}

fn main() {
    let u = User{ id: 1, name: "ada" }
    io.print("\(u)")                        // Display  -> #1 ada
    io.print(u.debug())                     // Debug    -> User{ id: 1, name: "ada" }
    io.print("\(u.hash())")                 // Hash
    io.print(json.stringify(u.encode()))    // Encode   -> {"id":1,"name":"ada"}

    let (doc, err) = json.parse("{\"id\":2,\"name\":\"grace\"}")
    if err != nil {
        return
    }
    let (back, derr) = User.decode(doc)     // Decode
    if derr != nil {
        io.error(derr.message())
        return
    }
    io.print("\(back)")

    let (missing, lerr) = load("users.json")
    if lerr != nil {
        io.error(lerr.message())
    }
}
```

A struct without `Display` cannot be interpolated:

```kite fails
struct Point {
    pub x: int
    pub y: int
}

fn main() {
    let p = Point{ x: 1, y: 2 }
    io.print("\(p)")   //~ E0207
}
```

### Slices

`map` `fold` `filter` `any` `all` `count` `find` `first` `last` `reversed`
`concat` `take` `drop` `zip` `enumerate` `flatten` `position` `includes`
`unique` `chunked` `sorted` `min_by` `max_by`

| Signature | Notes |
|---|---|
| `map<T, U>([T], fn(T) -> U) -> [U]` | Closure param **must** be annotated (see below). |
| `fold<T, A>([T], A, fn(A, T) -> A) -> A` | Left to right. |
| `filter<T>([T], fn(T) -> bool) -> [T]` | |
| `any` / `all` / `count` `<T>([T], fn(T) -> bool)` | `all` is vacuously true. |
| `find<T>([T], fn(T) -> bool) -> Option<T>` | |
| `first` / `last` `<T>([T]) -> Option<T>` | |
| `reversed` / `unique` `<T>([T]) -> [T]` | `unique` keeps the first of each. |
| `concat<T>([T], [T]) -> [T]` | Neither input changes; slices are values. |
| `take` / `drop` `<T>([T], int) -> [T]` | Neither traps on a negative `n`: `take` yields nothing, `drop` yields everything. |
| `zip<A, B>([A], [B]) -> [(A, B)]` | Up to the shorter. |
| `enumerate<T>([T]) -> [(int, T)]` | A function, not a method. |
| `flatten<T>([[T]]) -> [T]` | |
| `position<T>([T], fn(T) -> bool) -> int` | `-1` when absent, not an optional. |
| `includes<T>([T], T) -> bool` | |
| `chunked<T>([T], int) -> [[T]]` | `size <= 0` gives an empty result. |
| `sorted<T>([T], fn(T, T) -> bool) -> [T]` | Stable merge sort; takes a comparison, not an `Ord`. |
| `min_by` / `max_by` `<T>([T], fn(T, T) -> bool) -> Option<T>` | Both take a *less-than*. |

### Maps

The prelude has nothing for them. No `merge`, no `get_or`, no `map_values` — a
map's whole surface is three methods and the index: `len`, `keys`, `values`,
`m[k]` (an `Option<V>`) and `m[k] = v`.

**A key cannot be removed.** `m.remove(k)` is `E0205`, whose note is the
surface list above; there is no `delete` statement; and `m[k] = nil` is `E0200`
because `nil` is not a value of `V`.

```kite fails
fn main() {
    var counts = { "a": 1, "b": 2 }
    counts.remove("a")   //~ E0205
    io.print("\(counts.len())")
}
```

Removal is a rebuild, and the copy is visible in the code rather than hidden in
a method:

```kite
fn without(m: { str: int }, key: str) -> { str: int } {
    var out: { str: int } = {}
    for k in m.keys() {
        if k == key {
            continue
        }
        let v = m[k]
        if v == nil {
            continue
        }
        out[k] = v
    }
    return out
}

fn main() {
    let counts = { "a": 1, "b": 2, "c": 3 }
    let fewer = without(counts, "b")
    io.print("\(counts.len()) \(fewer.len()) \(or_else(fewer["a"], 0))")
}
```

The empty literal needs the annotation: `var out = {}` alone is `E0204`,
because `{}` names no key or value type.

### Numbers

`abs` `absf` `min` `max` `minf` `maxf` `clamp` `clampf` `approx_eq` `divides`
`sum` `sumf`

The `f` suffix is the float form: Kite has no overloading, so `min` is
`(int, int)` and `minf` is `(float, float)`. `sum([int]) -> int`,
`sumf([float]) -> float`. `approx_eq(a, b, tolerance) -> bool` lives here, not
in `std/math`.

### Strings

`contains` `starts_with` `ends_with` `split` `join` `replace` `words`
`lower` `upper` `equal_ignoring_case` `pad_start`

All are functions over `str`, written on top of the five `str` methods.
`split(s, "")` returns the whole string rather than looping. `lower`/`upper`
are **ASCII only** and say so.

### Optionals and parsing

`or_else<T>(Option<T>, T) -> T` · `is_some<T>(Option<T>) -> bool` ·
`parse_int(str) -> Option<int>` · `parse_float(str) -> Option<float>`

`parse_int` accepts a leading `-` and digits, nothing else. `parse_float` has
no exponent — that is `std/json`'s job.

### Hashing and debug helpers

`hash_seed` `hash_combine` `hash_int` `hash_bool` `hash_float` `hash_str`
`debug_str`

These are the ordinary functions `@derive(Hash)` and `@derive(Debug)` are
written in terms of, so a hand-written body gets the same answers. FNV-1a with
the **32-bit** parameters, because Kite traps on integer overflow.

### A tour that compiles

```kite
struct Person {
    pub name: str
    pub age: int
}

fn main() {
    let people = [
        Person{ name: "ada", age: 36 },
        Person{ name: "grace", age: 45 },
        Person{ name: "alan", age: 41 },
    ]

    let names = map(people, |p: Person| p.name)
    io.print(join(sorted(names, |a, b| a < b), ", "))
    io.print("\(fold(people, 0, |total: int, p: Person| total + p.age))")
    io.print("\(count(people, |p| p.age > 40))")
    io.print("\(any(people, |p| p.age > 44)) \(all(people, |p| p.age > 30))")
    io.print("\(position(names, |n| n == "alan"))")

    let oldest = max_by(people, |a, b| a.age < b.age)
    if oldest != nil {
        io.print(oldest.name)
    }

    for (i, n) in enumerate(names) {
        io.print("\(i): \(n)")
    }

    io.print("\(flatten([[1, 2], [3]]).len()) \(unique([1, 1, 2]).len())")
    io.print("\(includes([1, 2], 2)) \(chunked([1, 2, 3], 2).len())")
    io.print("\(zip([1, 2], ["a", "b"]).len()) \(reversed([1, 2, 3]).len())")
    io.print("\(take([1, 2, 3], 2).len()) \(drop([1, 2, 3], 2).len())")
    io.print("\(or_else(first(names), "?")) \(or_else(last(names), "?"))")
    io.print("\(is_some(find(people, |p| p.age == 36)))")

    io.print("\(abs(-3)) \(min(1, 2)) \(clamp(9, 0, 5)) \(sum([1, 2, 3]))")
    io.print("\(absf(-1.5)) \(maxf(1.0, 2.0)) \(clampf(9.0, 0.0, 5.0))")
    io.print("\(approx_eq(0.1 + 0.2, 0.3, 0.0001)) \(divides(9, 3))")

    io.print("\(contains("hello", "ell")) \(starts_with("hello", "he"))")
    io.print(join(split("a,b,c", ","), "|"))
    io.print("\(replace("a-b", "-", "+")) \(words("  two  words ").len())")
    io.print("\(lower("AB")) \(upper("ab")) \(equal_ignoring_case("AB", "ab"))")
    io.print(pad_start("7", 3, "0"))

    io.print("\(or_else(parse_int("-42"), 0)) \(or_else(parse_float("3.5"), 0.0))")
    io.print("\(is_some(parse_int("12x")))")
    io.print("\(hash_str("a")) \(hash_combine(hash_seed(), 3))")
    io.print(debug_str("say \"hi\""))
}
```

### The one prelude trap

A closure's parameter types come from the place it is used. `filter`'s `T` is
already fixed by the slice, so `|n| n > 2` infers. `map`'s `U` is fixed by
nothing, so the whole expected function type is unknown and the parameter goes
with it:

```kite fails
fn main() {
    let xs = [1, 2, 3]
    let doubled = map(xs, |n| n * 2)   //~ E0209
    io.print("\(sum(doubled))")
}
```

```kite
fn main() {
    let xs = [1, 2, 3]
    let doubled = map(xs, |n: int| n * 2)
    let big = filter(xs, |n| n > 2)          // no annotation needed
    io.print("\(sum(doubled)) \(big.len())")
}
```

## The builtins — dotted paths, no import

These are resolved by `BuiltinFn::from_path` in `crates/kite-resolve/src/lib.rs`
before any module lookup. Nothing brings them into scope, and no module — yours
or the standard library's — can take a path away from them. A *local binding*
named after a head does win, because then `io.print` is an ordinary field or
method lookup on that value: `let io = …` turns `io.print("hi")` into `E0205`.

### Output and input

| Call | Type |
|---|---|
| `io.print(v)` | `v` is `int`, `float`, `bool`, `str`, or any `Display`. Anything else is `E0200`. |
| `io.error(v)` | Same, to the error stream. |
| `io.read_line()` | `-> str` — one line, newline removed. **At end of input it returns `""`, and goes on returning it.** |

### The whole command-line surface

Those three calls are it, and the shape of a terminal program follows from
that more than from anything else in this document.

**There are no command-line arguments.** No `os.args`, no `env.get`, no argv
under any spelling: the three `io.` entries above are the whole of
`BuiltinFn::from_path`'s traffic with the process, and not one of the twenty
standard modules offers another. The arguments after `kitec run f.kite` are the
*compiler's* — an extra one is `error: unexpected argument`, refused before the
program starts. So a program that would take a path takes it on stdin instead,
and says so when it gets nothing.

**EOF is indistinguishable from a blank line.** Both are `""`, forever, so the
obvious loop never ends:

```kite ignore
for {
    let line = io.read_line()      // "" at EOF, and "" next time too
    io.print(line)                 // spins
}
```

A loop that terminates has to stop on `""` — accepting that a blank line ends
the input, which is why a Kite CLI reads a list and then acts on it rather than
streaming:

```kite
use std/fs

/// Every line of stdin, stopping at the first empty one — which is also what
/// end of input looks like, and the only thing that can end this loop.
fn lines() -> [str] {
    var out: [str] = []
    for {
        let line = io.read_line()
        if line == "" {
            return out
        }
        out.push(line)
    }
}

fn main() {
    let paths = lines()
    if paths.len() == 0 {
        io.error("usage: one path per line on stdin")
        return
    }
    for path in paths {
        let (body, err) = fs.read(path)
        if err != nil {
            io.error(err.message())
            continue
        }
        io.print("\(path): \(body.len()) bytes")
    }
}
```

**There is no exit status and no `exit(code)`.** A trap exits 1 — `kitec run`
prints `error: …` and `note: traps are not catchable`. Everything else exits
**0**: `io.error(msg)` then `return` exits 0, and so does a non-nil `error`
returned from `fn main() -> error`, which is discarded. The program above exits
0 on its usage error. To a shell, `set -e`, or CI, a failing Kite run looks
exactly like a successful one — put the outcome in the output, not the status,
and do not reach for `require(false, msg)` to force a 1: it does exit 1, but as
a trap, which is a claim about the caller rather than a report to the user.

### Errors

| Call | Type |
|---|---|
| `errors.new(message: str)` | `-> error` |
| `errors.because(message: str, cause: error)` | `-> error` — keeps the cause, which is what `errors.chain` walks. |

An `error` value itself has two methods, no import needed and no trait to
implement: `err.message() -> str` and `err.cause() -> error` (nil at the
bottom). Both need `err` proved non-nil first, or `E0301`. `std/errors` is
written in terms of exactly these two.

Everything else about errors — `wrap`, `of`, `chain`, `root`, `mentions`,
`message_or` — is `use std/errors`.

### Clock

`time.now() -> int`, milliseconds. **Virtual** on the bytecode VM and in the
generated glue: when every task is waiting on a deadline the clock jumps to the
earliest, so a sleeping program costs no real time and both backends agree on
ordering. Under `kitec run` it starts at 0.

### Text measurement and construction

| Call | Type |
|---|---|
| `text.from_code(code: int)` | `-> str` — one code point, one character. |
| `text.width(body: str)` | `-> float` — asks the renderer. |
| `text.height()` | `-> float` — the current line height. |

These share the `text.` head with `std/text` and do not collide: the builtin
table wins for these three paths, the module supplies the rest.

### The host, generally

`js.func(handler) -> JsValue` wraps a Kite closure as a JavaScript function.
Every parameter is a `JsValue`, at most **4** of them, returning `JsValue` or
nothing. The expected type is pushed *into* a closure literal, so
`js.func(|e| …)` needs no annotation on `e`.

### Drawing

The surface is the host's and there is one; a program cannot yet say which
`<canvas>`. Coordinates are `float`, origin top left, colours `0xRRGGBB` as
`int`. `std/canvas` is a named layer over exactly these.

| Call | Parameters |
|---|---|
| `draw.rect` | `x, y, w, h: float, colour: int` |
| `draw.rrect` | `x, y, w, h, radius: float, colour: int` |
| `draw.drrect` | `x, y, w, h, radius, width: float, colour: int` |
| `draw.text` | `x, y: float, body: str, colour: int` — **four**, not five |
| `draw.font` | `size: float, weight: int` (CSS scale: 400, 500, 700) |
| `draw.alpha` | `a: float` |
| `draw.clip` | `x, y, w, h: float` |
| `draw.unclip` | — |
| `draw.image` | `x, y, w, h: float, src: str` |
| `draw.field` | `x, y, w, h: float, value: str, hint: str, colour: int, id: str, multiline: bool` |
| `draw.semantics` | `x, y, w, h: float, role: int, label: str, flags: int, id: str` |

```kite
fn main() {
    draw.font(14.0, 500)
    draw.rect(0.0, 0.0, 320.0, 200.0, 0x101216)
    draw.rrect(8.0, 8.0, 100.0, 40.0, 6.0, 0x1f2937)
    draw.drrect(8.0, 8.0, 100.0, 40.0, 6.0, 2.0, 0x3b82f6)
    draw.text(16.0, 24.0, "hello", 0xffffff)
    draw.image(0.0, 40.0, 32.0, 32.0, "logo.png")
    draw.clip(0.0, 0.0, 100.0, 100.0)
    draw.unclip()
    draw.alpha(0.5)
    draw.field(0.0, 80.0, 200.0, 24.0, "", "name", 0xffffff, "name", false)
    draw.semantics(0.0, 80.0, 200.0, 24.0, 1, "Name", 0, "name")
    io.print("\(text.width("hello")) \(text.height()) \(text.from_code(65))")
}
```

### Concurrency primitives

Seven, and every combinator in `std/task` is built from them.

| Call | Type | Notes |
|---|---|---|
| `task.yield()` | `-> ()` | The suspension point. `E0521` outside `async`. |
| `task.park()` | `-> ()` | "Nothing to wait for but another task" — stops a spin looking like progress. |
| `task.wake_at(ms: int)` | `-> ()` | Deadline for the virtual clock. |
| `task.wait_host()` | `-> ()` | "What I am waiting for is the host", so the runtime can hand back the event loop. |
| `task.finished(t: Task<T>)` | `-> bool` | Reads the task without suspending. |
| `task.get(t: Task<T>)` | `-> T` | Ditto. Undefined before `finished`. |
| `time.now()` | `-> int` | The seventh. |

### Identity, assertions

| Call | Type | Notes |
|---|---|---|
| `ptr.same(a: T, b: T)` | `-> bool` | Structs, enums and maps only. A slice is `E0213`: copy-on-write means sharing a buffer is an allocator detail. |
| `assert(cond: bool, message: str)` | `-> ()` | **Dropped by `--release`** — a claim about the program. |
| `require(cond: bool, message: str)` | `-> ()` | **Kept by `--release`** — a claim about the caller. |

Both take the message eagerly; there is no lazy form. Neither is a module
member — they are bare names.

```kite
struct Cell {
    pub var n: int
}

async fn work() -> int {
    task.yield()
    task.wake_at(time.now() + 5)
    return 7
}

pub async fn main() {
    let t = work()
    if task.finished(t) {
        io.print("\(task.get(t))")
    }
    let v = await t
    assert(v == 7, "work returns seven")
    require(v > 0, "the caller asked for a positive count")

    let a = Cell{ n: 1 }
    let b = a
    io.print("\(ptr.same(a, b))")
}
```

### What an agent gets wrong here

```kite fails
fn main() {
    io.println("hi")   //~ E0111
}
```

```kite fails
use std/io   //~ E0400

fn main() {
    io.print("hi")
}
```

The builtin and the module share a head but not a namespace. `errors.new` is a
builtin; `errors.wrap` is not:

```kite fails
fn main() {
    let e = errors.new("boom")
    let w = errors.wrap(e, "context")   //~ E0111
    _ = w
}
```

```kite
use std/errors

fn main() {
    let e = errors.new("boom")
    let w = errors.wrap(e, "context")
    if w != nil {
        io.print(w.message())
    }
}
```

## The standard modules

Twenty, all reached as `use std/<name>` and all qualified at the use site.
`use std/json as j` *adds* `j` as a qualifier for that file — it does not take
`json` away, and both keep working. The compiler's own
list, printed by `E0400`, is: canvas, text, task, sync, math, time, errors, fmt,
json, toml, fs, js, dom, window, html, test, buffer, http, socket, crypto.
`prelude` is not on it — it is not importable.

Availability, as the compiler and runtimes actually behave:

| Module | Host group | Runs under `kitec run` | Web | Native/Node glue |
|---|---|---|---|---|
| buffer, errors, fmt, json, math, task, test, text, time, toml | none | yes | yes | yes |
| canvas | `draw.*` builtins | traced, not drawn | yes | yes |
| sync | none | yes | yes | yes |
| fs | `fs` | **yes** | **no** | yes |
| http, socket | `net` | no | yes | yes |
| crypto | `crypto` | no | yes | yes |
| **js, dom, window, html** | `js` (dom/window/html via `std/js`) | no | **yes — web only** | no |

"no" above means the program compiles and then traps:
`` `net.fetch_start` is a host function, and this runtime supplies no host ``.
Traps are not catchable; Kite has no `recover`. "Traced, not drawn" means the
bytecode VM prints each `draw.*` call instead — `canvas.fill(0.0, 0.0, 10.0,
10.0, 0)` writes the line `rect 0.0 0.0 10.0 10.0 0` to stdout, which is what
makes drawing code testable without a canvas.

### buffer — flat numeric buffers

A `[Point]` is an array of *references*; WasmGC has no unboxed aggregates in
arrays. `buffer.F64` is a flat `[float]` with a `stride` written down beside it.

`f64(stride)` `count(b)` `get(b, record, field)` `record(b, i)`
`push(b, values)` `set(b, record, field, value)` `column(b, field)`
`extent(b, field)`. `push` and `set` **return a new buffer** — bind the result.

```kite
use std/buffer

fn main() {
    var points = buffer.f64(2)
    points = buffer.push(points, [0.0, 1.0])
    points = buffer.push(points, [2.0, 3.0])
    points = buffer.set(points, 0, 0, 0.5)
    let (low, high) = buffer.extent(points, 1)
    io.print("\(buffer.count(points)) \(buffer.get(points, 1, 0)) \(low)..\(high)")
    io.print("\(buffer.column(points, 0).len()) \(buffer.record(points, 0).len())")
}
```

### canvas — low-level drawing into a `<canvas>`

A named surface over the `draw.*` builtins, not a graphics library. There is no
line, no path and no polygon, because the boundary has none and faking them
would look wrong differently on each backend.

`fill` `rounded` `circle` `ring` `circle_ring` `text` `font` `width_of`
`line_height` `image` `clip` `unclip` `alpha` `opaque`

```kite
use std/canvas

fn main() {
    canvas.fill(0.0, 0.0, 320.0, 200.0, 0x101216)
    canvas.rounded(8.0, 8.0, 100.0, 40.0, 6.0, 0x1f2937)
    canvas.circle(160.0, 100.0, 40.0, 0x3b82f6)
    canvas.circle_ring(160.0, 100.0, 60.0, 4.0, 0xffffff)
    canvas.ring(8.0, 8.0, 100.0, 40.0, 6.0, 2.0, 0x3b82f6)
    canvas.font(14.0, 500)
    canvas.text(16.0, 24.0, "hello", 0xffffff)
    canvas.image(0.0, 40.0, 32.0, 32.0, "logo.png")
    canvas.clip(0.0, 0.0, 100.0, 100.0)
    canvas.unclip()
    canvas.alpha(0.5)
    canvas.opaque()
    io.print("\(canvas.width_of("hello")) \(canvas.line_height())")
}
```

### crypto — bindings, not implementations

Thin declared boundary over the host's primitives (WebCrypto in a browser or
Node). No ECB, no CBC, no MD5, no SHA-1, no raw RSA. Salts and nonces are
generated, never passed. Keys are opaque handles; a private half never crosses
the boundary.

Sync: `random(count) -> str` (lowercase hex), `token() -> str`,
`equal(a, b) -> bool` (constant time).
Async, each `-> (T, error)`: `sha256` `sha384` `sha512` `hmac(key, text)`
`password_hash(password)` `password_verify(password, stored)`
`generate_key` `import_key` `seal(key, plaintext)` `open(key, sealed)`
`signing_key` `verify_key` `sign` `verify(public, text, signature)`
`agreement_key` `exchange_key` `agree(key, their_public)`.
Types: `Key`, `SigningKey`, `AgreementKey`.

Comparing a value that came *straight* from a `crypto.` call with `==` is
`E0600` — a **warning**, and detected syntactically, so binding it to a `let`
first hides it. Use `crypto.equal`.

```kite
use std/crypto

pub async fn main() {
    io.print("\(crypto.random(16).len()) \(crypto.token().len())")

    let (digest, err) = await crypto.sha256("hello")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(digest)

    let (stored, herr) = await crypto.password_hash("hunter2")
    if herr != nil {
        io.error(herr.message())
        return
    }
    let (same, verr) = await crypto.password_verify("hunter2", stored)
    if verr != nil {
        io.error(verr.message())
        return
    }
    io.print("\(same) \(crypto.equal(digest, digest))")
}
```

### dom — the document. **Web only**

Ordinary Kite over `std/js` with not one `extern` in the file. `Element`,
`Event` and `Subscription` are opaque outside the module (unmarked fields).

Find: `find(selector) -> Option<Element>` `find_in` `find_all -> [Element]`
`body()`. Make: `create(tag) -> (Element, error)`.
Read/write: `text` `set_text` `attribute -> Option<str>` `set_attribute`
`remove_attribute` `value` `set_value` `checked` `set_checked` `set_style`
`add_class` `remove_class` `has_class` `set_class`.
Tree: `append` `insert_before` `remove` `same`.
Events: `on(target, name, fn(Event)) -> (Subscription, error)`, and on the
event `target -> Option<Element>` `event_value` `key` `prevent_default`
`stop_propagation`. Document: `title` `set_title`.
Escape hatches: `raw(Element) -> JsValue`, `wrap(JsValue) -> Element`,
`raw_event`.

Nearly every mutator answers with a bare `error`, and a dropped one is `E0302`
— write `_ = …` to discard on purpose.

```kite
use std/dom

fn main() {
    let field = dom.find("#name")
    if field == nil {
        return
    }
    let (sub, err) = dom.on(field, "input", |e: dom.Event| {
        _ = dom.set_text(field, "hello \(dom.event_value(e))")
    })
    if err != nil {
        io.error(err.message())
        return
    }
    _ = dom.add_class(field, "live")
    io.print("\(dom.attribute(field, "placeholder") == nil)")
}
```

#### The page a `--emit wasm` build writes has nothing to find

The example above compiles, builds, loads — and does nothing, if you let the
compiler write the page. `kitec build page.kite --emit wasm --out dist` writes
an `index.html` whose entire markup, under a `<style>` and above the module
loader, is

```html
<canvas id="stage"></canvas>
<pre id="out"></pre>
```

There is no `#name`, no `#table`, no mount point of any kind. `dom.find`
returns `nil`, the `if … == nil { return }` guard fires, `main` returns, and
**a bare `return` reports nothing anywhere** — not by the compiler, not by the
glue, not in the console. `verify.sh` cannot catch it either: it proves
compilation, not behaviour.

So do not write a bare `return`. `io.error` in a `--emit wasm` build reaches
`console.error`, which makes the nil-guard the cheapest diagnostic available
for exactly this failure — loading the generated page prints
`[ERROR] no #list in the page — nothing was rendered` and the blank page stops
being a mystery:

```kite ignore
let into = dom.find("#list")
if into == nil {
    io.error("no #list in the page — nothing was rendered")
    return
}
``` Nothing configures this away — `renderer = "dom"` under `[targets]`
in `kite.toml` parses, and `kitec build` never reads it (only `kitec pkg` reads
that table, and only to check the `entry` exists).

Two recipes work.

**Write `index.html` first and build into its directory.** The compiler leaves
an existing one alone and says so, so the markup is yours and the build only
refreshes the module beside it:

```
$ kitec build main.kite --emit wasm --out .
  and ./api.js with ./api.d.ts
wrote ./app.wasm (2743 bytes) and ./app.js
note: ./index.html was already there and was left alone
```

Four lines of the page are the wiring, and `resident` comes before `main` so
the program's own clock is running from its first line rather than its first
event:

```html
<tbody id="rows"></tbody>
<script type="module">
  import { instantiate, resident } from "./app.js";
  const exports = await instantiate("./app.wasm");
  resident(exports);
  exports.main();
</script>
```

That is `examples/page/` exactly.

**Or mount into `dom.body()`,** and let the generated page stand. `body()` is
`Option<dom.Element>` like every other lookup here, so it needs narrowing; and
`html.mount` clears its container first, so the `<canvas>` and `<pre>` go away
and the document becomes the program's:

```kite
use std/dom
use std/html

fn main() {
    let into = dom.body()
    if into == nil {
        return
    }
    let (view, err) = html.mount(into, [
        html.txt("h1", [html.id("title")], "Ledger"),
        html.empty("div", [html.id("rows")]),
    ])
    if err != nil {
        io.error(err.message())
        return
    }
    _ = html.update(view, [html.txt("h1", [html.id("title")], "Ledger (1)")])
}
```

The third route is a bundler, and it is the one a real project takes — see
`vite-plugin-kite` under *The toolchain*: there the HTML is the project's from
the start and nothing is generated.

### errors — building on and reading error values

`wrap(err, context) -> error` (nil for nil, so it composes with `check`),
`of(subject, problem)`, `chain(err) -> [str]` (outermost first),
`root(err) -> error`, `mentions(err, needle) -> bool`,
`message_or(err, absent) -> str`.

`mentions` reads the **outermost message only** — it does not walk the chain.
That is usually enough because `wrap` interpolates what it wrapped, so the outer
message contains the inner one; an error joined with `errors.because` does not
get that, and `mentions(because("outer", new("root")), "root")` is `false`.

```kite
use std/errors

fn inner() -> error {
    return errors.new("no such file")
}

fn outer() -> error {
    return errors.wrap(inner(), "loading config")
}

fn main() {
    let err = outer()
    io.print(join(errors.chain(err), " <- "))
    io.print(errors.message_or(errors.root(err), "none"))
    io.print("\(errors.mentions(err, "no such file"))")
    io.print(errors.message_or(errors.of("config", "unreadable"), "none"))
}
```

### fmt — laying text out in columns

There is no format-string language and there is not going to be one;
interpolation already renders. What it cannot do is *align*.

`pad(s, width)` `pad_left` `centre` `ellipsis(s, width)` `fixed(x, places)`
`percent(part, whole)` `grouped(n)` `row(cells, widths)`

### fs — files and directories. **Not on the web**

Every fallible call returns `(T, error)`; there is no errno.
`read(path) -> (str, error)` `write(path, body) -> error`
`list(path) -> ([str], error)` `remove(path) -> error`
`kind(path) -> Kind` (`File` | `Dir` | `Missing`) `exists` `is_file` `is_dir`
`temp_dir() -> str`.

This is the one host group `kitec run` supplies, so the block below actually
runs.

```kite
use std/fs
use std/errors

fn roundtrip() -> (str, error) {
    let path = "\(fs.temp_dir())/note.txt"
    check errors.wrap(fs.write(path, "hello"), "writing \(path)")
    let (body, err) = fs.read(path)
    check errors.wrap(err, "reading \(path)")
    check errors.wrap(fs.remove(path), "removing \(path)")
    return body, nil
}

fn main() {
    let (body, err) = roundtrip()
    if err != nil {
        io.error(errors.message_or(err, "unknown"))
        return
    }
    io.print(body)
    match fs.kind("/etc") {
        Dir => io.print("dir"),
        File => io.print("file"),
        Missing => io.print("missing"),
    }
}
```

### html — described elements, diffed on update. **Web only**

A `Node` is a value: a tag, attributes, children. `mount` builds it and
remembers it; `update` compares and touches only the difference.

Two constructors, not one per tag: `el(tag, attrs, kids)` and
`txt(tag, attrs, body)`, plus `text(body)` and `empty(tag, attrs)`.
Attributes: `attr(name, value)` `class(names)` `id(name)` `data(name, value)`.
`keyed(key, node)` — **give list items a key**, or children are matched by
position and a reorder rewrites everything it moved past.
`mount(into: dom.Element, [Node]) -> (Mounted, error)` ·
`update(var view: Mounted, [Node]) -> error`.

A mistyped tag becomes a `<flase>` in the document, not a compile error. And
the element mounted into has to be in the page: `dom.find("#table")` below
finds nothing in a generated `index.html` — see *The page a `--emit wasm` build
writes has nothing to find*, above. `mount` empties whatever it is given first.

```kite
use std/dom
use std/html

struct Row {
    pub id: int
    pub name: str
}

fn row(r: Row) -> html.Node {
    return html.keyed("\(r.id)", html.el("tr", [], [
        html.txt("td", [html.class("num")], "\(r.id)"),
        html.txt("td", [], r.name),
    ]))
}

fn main() {
    let into = dom.find("#table")
    if into == nil {
        return
    }
    let rows = [Row{ id: 1, name: "ada" }, Row{ id: 2, name: "grace" }]
    let (view, err) = html.mount(into, map(rows, row))
    if err != nil {
        io.error(err.message())
        return
    }
    _ = html.update(view, map(reversed(rows), row))
}
```

### http — client and server

A 404 is a `Response`, not an `error`: the request succeeded and the answer was
"no". Only a transport failure is an `error`.

Types: `Request{ method, path, body, headers }` (headers are `name: value`
lines, because only text crosses the boundary), `Response{ status, body }`,
`Header{ name, value }`, `Route`, `Server`, `Incoming`, `Events`, `Event`.

Client, all async `-> (Response, error)`: `get` `post` `put` `delete` `patch`
`head` `options` `query` and `send(method, url, body, headers)`.
Helpers: `ok(body)` `not_found()` `status(code, body)` `succeeded(r)`
`header(r, name)`.

Routing, all synchronous and testable with no port:
`route(method, pattern, fn(Request) -> Response)` ·
`serve([Route], Request) -> Response` · `matches(pattern, path)` ·
`parameter(pattern, path, name) -> Option<str>` ·
`request_header(request, name) -> Option<str>`.

Server: `open(port) -> (Server, error)` async (port 0 asks the host to choose),
`port_of` · `accept(server) -> (Incoming, error)` async ·
`respond(incoming, response, [Header]) -> error` — headers as **pairs, not
text**, because a newline in a value would otherwise become a separator ·
`run(server, [Route]) -> (int, error)` · `serve_closed` · `shut`.

Server-sent events: `events(url)` / `events_named(url, names)` ·
`listen(stream, name)` · `receive(stream) -> (Event, error)` · `pending` ·
`close`. A message is *taken* off a queue, never pushed at a callback.

```kite
use std/http

fn index(r: http.Request) -> http.Response {
    return http.ok("kite")
}

fn show(r: http.Request) -> http.Response {
    let id = http.parameter("/users/:id", r.path, "id")
    if id == nil {
        return http.status(400, "no id in the path")
    }
    return http.ok("user \(id)")
}

fn main() {
    let routes = [
        http.route("GET", "/", index),
        http.route("GET", "/users/:id", show),
    ]
    let req = http.Request{ method: "GET", path: "/users/7", body: "", headers: "" }
    let res = http.serve(routes, req)
    io.print("\(res.status) \(res.body)")
    io.print("\(http.succeeded(res)) \(http.not_found().status)")
}
```

```kite
use std/http

fn index(r: http.Request) -> http.Response {
    return http.ok("hello")
}

pub async fn main() {
    let (server, err) = await http.open(0)
    if err != nil {
        io.error(err.message())
        return
    }
    io.print("listening on \(http.port_of(server))")
    let (count, rerr) = await http.run(server, [http.route("GET", "/", index)])
    if rerr != nil {
        io.error(rerr.message())
        return
    }
    io.print("served \(count)")
}
```

`kitec build … --emit wasm` notices a program that listens and writes
`serve.mjs` beside `app.wasm`; run it with `node`.

### js — the host, reached generally. **Web only**

The floor below the typed world: 32 functions over 29 `extern`s, all trading in
one opaque `JsValue`. Every call that *can* fail answers with an error rather
than throwing, because a name is looked up when the program runs — that is the
`(JsValue, error)` half of the list. The rest cannot fail and return bare:
`global` `nothing` `of_str` `of_num` `of_bool` `same` `is_nothing` `kind_of`
`str_or` `num_or` `bool_or` `SAFE_INTEGER`. `std/dom`, `std/window` and
`std/html` are written over it in ordinary Kite with not one `extern` of their
own — which is the proof that anything the platform has can be reached without
touching the compiler.

`global(name)` `nothing()` `get` `set` `at` `length` ·
`call0`…`call4(target, name, …)` · `new0`…`new3(constructor, …)` ·
`same(a, b)` `is_nothing` `kind_of` `instance_of` ·
`of_str` `of_num` `of_bool` `of_int` and `as_str` `as_num` `as_bool` `as_int` ·
`settle(promise, done, failed)` · `str_or` `num_or` `bool_or` ·
`SAFE_INTEGER()`.

**Keep `JsValue` out of your own public interface** — wrap it in a struct whose
field is not `pub`, the way `dom.Element` does.

```kite
use std/js

fn page_title() -> (str, error) {
    let document = js.global("document")
    let (value, err) = js.get(document, "title")
    check err
    return js.as_str(value)
}

fn main() {
    let (title, err) = page_title()
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(title)

    let handler = js.func(|e| { io.print("clicked") })
    let (_r, aerr) = js.call2(js.global("window"), "addEventListener",
        js.of_str("click"), handler)
    if aerr != nil {
        return
    }
    io.print(js.kind_of(js.nothing()))
}
```

### json — reading and writing JSON

Written in Kite on the five `str` primitives. `enum Json { Null, Bool(bool),
Number(float), Text(str), Array([Json]), Object({ str: Json }) }`.

`parse(input) -> (Json, error)` · `stringify(value) -> str` · `pretty` ·
navigation `field(v, key)` `at(v, index)` `items(v) -> [Json]`
`entries(v) -> { str: Json }` `is_null` · extraction `text` `number_of`
`int_of` `bool_of` (each `-> Option<…>`) · with fallbacks `text_or(v, key, d)`
`int_or` `bool_or` · `trait Encode { … }` for `@derive(Encode)`.

The navigation functions take `Option<Json>` and return `Option<Json>`, so they
chain without unwrapping and a `Json` passes where an `Option<Json>` is wanted.

```kite
use std/json

fn main() {
    let (doc, err) = json.parse("{\"name\":\"ada\",\"tags\":[1,2,3]}")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(json.text_or(doc, "name", "?"))
    for item in json.items(json.field(doc, "tags")) {
        io.print("\(or_else(json.int_of(item), 0))")
    }
    io.print(json.stringify(doc))
    io.print(json.pretty(doc))
}
```

### math — floats

Everything takes and returns `float` unless it says otherwise; the `int`
helpers are in the prelude. Nothing here needs compiler support.

`abs` `min` `max` `clamp` `trunc` `floor` `ceil` `round` ·
`round_to(x) -> int` `trunc_to(x) -> int` · `sqrt` `pow(base, exponent: int)`
`powf` `cbrt` `hypot` `lerp` `sign(x) -> int` · `exp` `ln` ·
`sin` `cos` `tan` `asin` `acos` `atan` `atan2` `degrees` `radians` ·
constants as functions: `pi()` `e()` `tau()` `ln2()` ·
integers: `max_int()` `min_int()` `checked_add(a, b) -> Option<int>`
`wrapping_add(a, b) -> int`.

There is no `math.approx_eq` despite what the float-equality warning says —
the prelude's `approx_eq` is the one that exists.

### socket — WebSocket, client side

Text frames only; a binary frame is not delivered rather than delivered as
something it is not. Messages are taken off a queue, not handed to a callback.

`connect(url) -> (Socket, error)` async · `send(s, message) -> error` ·
`receive(s) -> (str, error)` async · `pending(s) -> int` · `open(s) -> bool` ·
`close(s)`. A closed socket stays closed; reopening is the program's decision.

```kite
use std/socket

pub async fn main() {
    let (s, err) = await socket.connect("wss://example.com/feed")
    if err != nil {
        io.error(err.message())
        return
    }
    let serr = socket.send(s, "hello")
    if serr != nil {
        io.error(serr.message())
        return
    }
    for socket.open(s) {
        let (message, rerr) = await socket.receive(s)
        if rerr != nil {
            break
        }
        io.print(message)
    }
    socket.close(s)
}
```

### sync — Mutex and Atomic

Not pointless without OS threads: Kite's tasks interleave at `await` and
`task.yield`, so a read-modify-write spanning a yield is a lost update *today*.

`Mutex<T>`: `mutex(value)` `is_held(m)` `try_lock(var m) -> Option<T>`
`lock(var m) -> T` async `release(var m, value)` `update(var m, fn(T) -> T)`
async.
`Atomic`: `atomic(value)` `load` `store(var a, v)` `add(var a, delta)`
`compare_swap(var a, expect, next)` `swap(var a, next)`.

`Mutex<T>` has a `var` field and is `Share` **anyway**, whatever `T` is,
because reaching the value requires the lock. `Atomic` gets the same pass. That
exemption is the one carve-out in an otherwise structural rule, and it is made
by *qualified* name — the checker tests for `sync.Mutex` and `sync.Atomic` and
nothing else, so a hand-rolled lookalike, however identical, is judged
structurally and fails `E0520`.

### task — the concurrency library

Calling an `async fn` *starts* it and yields a `Task<T>`; `await` is how the
value comes out. There is no spawn keyword.

`both<A, B>(Task<A>, Task<B>) -> (A, B)` · `all<T>([Task<T>]) -> [T]` ·
`race<T>([Task<T>]) -> T` · `sleep(ms)` · `timeout<T>(Task<T>, ms) -> Option<T>`
· `parallel<T: Share, U: Share>([T], fn(T) -> U) -> [U]` ·
`scope<T>([Task<T>]) -> [T]`. All async.

```kite
use std/task
use std/sync

async fn fetch_one(n: int) -> int {
    await task.sleep(10)
    return n * 2
}

pub async fn main() {
    let a = fetch_one(1)
    let b = fetch_one(2)
    let (x, y) = await task.both(a, b)
    io.print("\(x) \(y)")

    io.print("\(sum(await task.all([fetch_one(3), fetch_one(4)])))")
    io.print("\(await task.race([fetch_one(5), fetch_one(6)]))")
    io.print("\(is_some(await task.timeout(fetch_one(7), 1)))")

    var counter = sync.mutex(0)
    let n = await sync.lock(counter)
    sync.release(counter, n + 1)
    io.print("\(sync.is_held(counter))")

    var hits = sync.atomic(0)
    sync.add(hits, 5)
    io.print("\(sync.load(hits))")
}
```

`task.parallel` is where `Share` bites:

```kite fails
use std/task

struct Counter {
    var hits: int
}

pub async fn main() {
    let counters = [Counter{ hits: 0 }]
    let seen = await task.parallel(counters, |c: Counter| c.hits)   //~ E0520
    io.print("\(seen.len())")
}
```

```kite
use std/task
use std/sync

struct Counter {
    var hits: int
}

pub async fn main() {
    let guarded = [sync.mutex(Counter{ hits: 0 })]
    let seen = await task.parallel(guarded, |m: sync.Mutex<Counter>| 1)
    io.print("\(seen.len())")
}
```

**`task.parallel`'s mapper must be an ordinary function — one that returns a
value, not one that starts a task.** That rules out the thing it looks made
for. Calling an `async fn` *starts* it, so the mapper's `U` is `Task<int>`, and
`U: Share` rejects it:

```kite fails
use std/task

async fn fetch_one(n: int) -> int {
    await task.sleep(10)
    return n * 2
}

pub async fn main() {
    let doubled = await task.parallel([1, 2, 3], |n: int| fetch_one(n))   //~ E0520
    io.print("\(sum(doubled))")
}
```

The diagnostic says `` `Task<int>` cannot be moved to another task `` and notes
that two tasks holding one mutable value is a data race — true, and not the
cause: nothing owning a suspended computation is `Share`, so no async mapper
will ever pass. `parallel` is for work over values. To run async work
concurrently, start the tasks yourself and hand the slice to `task.all`:

```kite
use std/task

async fn fetch_one(n: int) -> int {
    await task.sleep(10)
    return n * 2
}

fn twice(n: int) -> int {
    return n * 2
}

pub async fn main() {
    let started = map([1, 2, 3], |n: int| fetch_one(n))
    io.print("\(sum(await task.all(started)))")
    io.print("\(sum(await task.parallel([1, 2, 3], twice)))")
}
```

### test — assertions as values

A test is a `pub fn` whose name starts with `test_`. The runner will happily
call a `test_` of any shape, but write `-> (int, error)` — that is the shape
`check` needs, and the only one that can report a failure rather than pass
silently. `kitec test file.kite` finds them, runs each, and also runs every
` ```kite ` doc example. No assertion traps: a failure reports and the rest
still run.

Discovery walks the *compiled* functions and matches `test_` against the name
each one ended up with, which has two consequences worth knowing before you
lay a project out.

`pub` is load-bearing, because an unreached private function has already been
dropped and is not there to be found. Put a `pub` and a private `test_` in one
file and the runner reports `1 passed`.

**And `kitec test` is entry-file-only.** A function that arrived through `use`
is compiled under its *qualified* name — `money.test_double` — which does not
start with `test_`, so it is never a test, even when it is in the binary
because `main` calls it. Doc examples are worse: they are extracted from the
entry file's text, and a module's are never read at all.

```
$ kitec test proj/src/main.kite          # main calls money.test_double()
no tests in `proj/src/main.kite`
note: a test is a `pub fn test_…() -> (int, error)`, or a ```kite fence in a doc comment
```

Pointing `kitec test` at the module file instead recompiles that file alone, so
everything it took from its siblings is gone —
`error[E0204]: unknown type 'Amount'` for a struct declared next door.

So a module's tests are runnable exactly two ways. Either they live in the same
file as the code they test, and that file is self-contained — one file *is* a
module `kitec test` can run, which is why the block below works. Or a Node
runner drives `compiler().build({ entry, siblings })` from
`@kite-lang/compiler-wasm`, hands over the sibling sources by module name,
instantiates the result and calls a `main` that is itself the list of claims;
that is `test/run-kite.mjs` in the POS app, and it is the same thing
`vite-plugin-kite` does to build a page.

`is_true(cond, what)` `is_false` `equal_int` `equal_str` `equal_bool`
`equal_float(found, want, tolerance, what)` `equal_ints` `equal_strs`
`failed(err, what)` `ok(err, what)` `fail(what)` — each returns `error`, so
`check` makes a test read as a list of claims.

```kite
use std/test

fn add(a: int, b: int) -> int {
    return a + b
}

/// Twice a number.
///
/// ```kite
/// io.print("\(double(21))")
/// ```
pub fn double(n: int) -> int {
    return n * 2
}

pub fn test_adding() -> (int, error) {
    check test.equal_int(add(2, 2), 4, "two and two")
    check test.is_true(add(1, 1) == 2, "one and one")
    return 0, nil
}

pub fn test_strings() -> (int, error) {
    check test.equal_str(upper("ab"), "AB", "upper")
    check test.equal_strs(split("a,b", ","), ["a", "b"], "split")
    return 0, nil
}

fn main() {
    io.print("\(add(1, 2)) \(double(2))")
}
```

### text — Unicode algorithms for `std/canvas`

Only for a program placing its own glyphs; a program writing into the DOM never
needs this. Each entry point is a **named subset** with the unimplemented rules
stated.

`bidi_class(code) -> BidiClass` · `paragraph_level(body) -> int` ·
`bidi_runs(line) -> [Run]` (a `Run` has `body` and `rtl` — no offsets) ·
`bidi_runs_with(line, base)` · `bidi_levels` · `bidi_visual` ·
`is_combining(code)` · `join_arabic(run) -> str` ·
`line_break_class(code) -> LineBreak` · `break_opportunities(body) -> [bool]`.

UAX #9 rules P2–P3, X1–X10, W1–W7, N0–N2, I1–I2, L1–L2 (not L3, L4, HL1–HL6);
UAX #14 LB1–LB31 with SA treated as AL and CB unimplemented; Arabic joining to
Presentation Forms-B with the lam-alef ligature, not HarfBuzz.

```kite
use std/text

fn main() {
    let line = "hello \u{05D0}\u{05D1} world"
    io.print("\(text.paragraph_level(line))")
    for r in text.bidi_runs(line) {
        io.print("\(r.rtl): \(r.body)")
    }
    io.print("\(text.bidi_levels(line).len()) \(text.bidi_visual(line).len())")
    io.print("\(count(text.break_opportunities("a b c"), |b| b))")
    io.print("\(text.join_arabic("\u{0628}\u{0627}")) \(text.is_combining(0x0301))")
    match text.bidi_class(65) {
        L => io.print("left"),
        _ => io.print("other"),
    }
}
```

### time — the clock and what to say about it

Durations are plain `int` milliseconds; there is no `Duration` type.
`seconds(n)` `minutes(n)` `hours(n)` `millis(n)` are the constructors.
`since(start)` · `show(ms)` ("1m 30s") · `clock(ms)` ·
`struct Civil` and `civil_of(ms, east_minutes)` / `epoch_of(c, east_minutes)` ·
`iso_day` `hhmm` `stamp` `weekday` — each takes an offset in minutes east of
UTC · `is_leap(year)` `days_in_month` `month_start` `month_end`.

```kite
use std/time

fn main() {
    let start = time.now()
    io.print(time.show(time.seconds(90)))
    io.print(time.clock(time.hours(3) + time.minutes(4)))
    io.print("\(time.iso_day(start, 0)) \(time.hhmm(start, 0))")
    io.print(time.stamp(start, 0))
    let c = time.civil_of(start, 0)
    io.print("\(c.year)-\(c.month)-\(c.day)")
    io.print("\(time.epoch_of(c, 0) == start)")
    io.print("\(time.is_leap(2024)) \(time.days_in_month(2024, 2))")
    io.print("\(time.weekday(start, 0)) \(time.since(start) >= 0)")
}
```

### toml — reading and writing TOML

Same shape as `std/json`. `enum Toml`, `parse(input) -> (Toml, error)`,
`emit(doc) -> str`, and dotted-path lookups `at(doc, path) -> Option<Toml>`,
`text_at(doc, path, fallback)`, `int_at`, `float_at`, `bool_at`.

The subset is named: comments, bare/quoted/dotted keys, `[table]` and
`[[array.of.tables]]`, basic and literal and multi-line strings, integers with
`_` separators, floats with exponents (`inf` and `nan` are refused), booleans,
arrays, inline tables. **Dates and times are not implemented** — a date parses
as the string it was written as, losslessly, rather than being half-mapped onto
`std/time`.

```kite
use std/toml

fn main() {
    let (doc, err) = toml.parse("[server]\nport = 8080\nname = \"edge\"\ntls = true\n")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print("\(toml.int_at(doc, "server.port", 0))")
    io.print(toml.text_at(doc, "server.name", "?"))
    io.print("\(toml.bool_at(doc, "server.tls", false))")
    io.print("\(is_some(toml.at(doc, "server")))")
    io.print(toml.emit(doc))
}
```

### window — everything around the document. **Web only**

Also ordinary Kite over `std/js`, with no `extern` in the file. One escape
hatch, `raw() -> JsValue`, is the whole of `JsValue` in its interface — there is
no `wrap`, because there is only ever one window.

Events and timers: `on(name, fn())` `on_passive` `after(ms, fn())`
`every(ms, fn())`, returning `(Listener, error)` / `(Timer, error)`.
Geometry: `scroll_x` `scroll_y` `width` `height` `scroll_to`.
Location: `origin` `path` `query` `hash` `set_hash` `go_to`
`parameter(name) -> Option<str>` `escaped` `unescaped`.
History: `push(url)` `rewrite(url)` `back()` `own_scroll_restoration()`.
Storage: `kept`/`keep`/`drop_kept` (local) and `cached`/`cache`/`drop_cached`
(session). Misc: `epoch_ms` `timezone_offset` `epoch_of(iso)` `seems_online`
`print_page` `register_worker(path)` `raw()`.

Absent on purpose: anything that is a recipe rather than a platform fact —
saving a blob through a synthetic anchor, uploading a file, reading a `<meta>`.

```kite
use std/window

fn route() {
    io.print(window.path())
}

fn main() {
    if window.kept("session") == nil {
        _ = window.go_to("/sign-in")
        return
    }
    let (listener, err) = window.on("hashchange", route)
    if err != nil {
        io.error(err.message())
        return
    }
    let (timer, terr) = window.every(1000, || { io.print("\(window.scroll_y())") })
    if terr != nil {
        return
    }
    _ = window.cache("last", window.hash())
    io.print("\(or_else(window.parameter("q"), "")) \(window.seems_online())")
}
```

## Diagnostics

Specification §16 makes error quality a language constraint: nominal traits,
explicit `dyn`, no implicit conversions and no overloading were all chosen so
the compiler can name **one cause and one fix**. Every diagnostic carries a
stable code, a primary span, secondary spans explaining *why*, and where
possible a machine-applicable fix. Codes are never reused for a different
meaning, and a code nobody can provoke is deleted rather than left to be
explained. Two more requirements §16 puts on the implementation: **one error per
cause** — a single missing brace must not produce forty, so the parser recovers
at statement and declaration boundaries — and **a type error names the source of
the expectation**, giving the parameter or return type that created the
constraint its own secondary span.

The rendering in §16 is exactly what the compiler prints, down to the path it
was handed:

```
error[E0114]: cannot assign to immutable binding `total`
  ┌─ ./cart.kite:3:5
  │
2 │     let total = 0
  │         ----- declared immutable here
3 │     total = total + 1
  │     ^^^^^ cannot assign
  │
help: make the binding mutable
  │
2 │     var total = 0
  │     ~~~
```

`kitec fix cart.kite` applies that `help` in place.

### Ranges

| Range | Area |
|---|---|
| E0000–E0099 | lexical |
| E0100–E0199 | syntax and bindings |
| E0200–E0299 | types, traits, patterns |
| E0300–E0399 | error handling (taint analysis) |
| E0400–E0499 | modules and visibility |
| E0500–E0599 | concurrency and `Share` |
| E0600–E0699 | cryptography |
| E0700–E0799 | derivation |
| E0800–E0899 | exclusivity |
| E0900–E0999 | the compiler failing, rather than the program |

### All 48 codes `--explain` knows

`kitec --explain E0301` prints the rationale for the rule, not just the
message. An unknown code prints the whole list. This table is the whole of
`crates/kite-diag/src/codes.rs`; anything outside it is a code the compiler
cannot emit.

| | | | |
|---|---|---|---|
| E0001 unterminated string literal | E0002 invalid character in source | E0003 invalid escape sequence | E0004 invalid number literal |
| E0005 block comments are not supported | E0006 interpolation nested too deeply | E0100 unexpected token | E0101 unclosed delimiter |
| E0102 expression nested too deeply | E0110 use of possibly-uninitialised binding | E0111 unknown name | E0112 duplicate definition |
| E0113 wrong number of arguments | E0114 cannot assign to immutable binding | E0115 `break`/`continue` outside a loop | E0116 unreachable code |
| E0117 statement has no effect | E0200 type mismatch | E0201 cannot apply operator to these types | E0202 condition must be `bool` |
| E0203 missing return value | E0204 unknown type | E0205 no such method, function, or callable value | E0206 trait cannot be a trait object |
| E0207 value cannot be interpolated | E0208 invalid type parameter | E0209 type argument cannot be inferred | E0210 non-exhaustive match |
| E0211 invalid closure | E0212 invalid cast | E0213 type has no identity | E0214 invalid type alias |
| E0301 value used before its error was checked | E0302 error is never checked | E0303 `check` outside a fallible function | E0400 module not found |
| E0401 private item | E0402 module cycle | E0403 module name reserved by the standard library | E0404 two modules of the same name |
| E0520 type cannot be moved to another task | E0521 `await` outside an async function | E0600 comparing a secret with `==` | E0700 malformed `@derive` |
| E0701 nothing derives that | E0702 a field the derive cannot write | E0800 one object under two argument names | E0900 the compiler emitted an invalid module |

### Warnings, not errors

`E0116` (unreachable code), `E0600` (secret compared with `==`) and `E0201` in
its float-equality form are **warnings**: `kitec check` still exits 0. The
float lint deliberately does not fire when either operand is a literal, because
`x == 0.0` is the guard written before a division and a tolerance would answer
a different question.

### The three you will actually hit

`E0302` — an error that goes out of scope unexamined, including one from a call
that binds nothing:

```kite fails
fn touch() -> error {
    return errors.new("no")
}

fn main() {
    touch()   //~ E0302
    io.print("done")
}
```

```kite
fn touch() -> error {
    return errors.new("no")
}

fn main() {
    _ = touch()                     // discarded, out loud
    let err = touch()
    if err != nil {
        io.error(err.message())
    }
    io.print("done")
}
```

`E0301` — a `(T, error)` pair is *correlated*. There is no zero value on the
failure path, so the value is unreadable until the error is disproved:

```kite fails
fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let (v, err) = load()
    io.print(v)   //~ E0301
    if err != nil {
        io.print(0)
    }
}
```

```kite
fn load() -> (int, error) {
    return 1, nil
}

fn main() {
    let (v, err) = load()
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(v)   // readable: the error was disproved
}
```

The same rule applies to `error` itself: `err.message()` before `err != nil` is
`E0301`, because an `error` is either nil or a failure and there is no message
on the nil side.

`E0303` — `check` propagates, so the enclosing function must be able to return
an error:

```kite fails
use std/fs

fn main() {
    let (body, err) = fs.read("a.txt")
    check err   //~ E0303
    io.print(body)
}
```

```kite
use std/fs

fn load(path: str) -> (str, error) {
    let (body, err) = fs.read(path)
    check err
    return body, nil
}

fn main() {
    let (body, err) = load("a.txt")
    if err != nil {
        io.error(err.message())
        return
    }
    io.print(body)
}
```

### The Wasm debug information

A name section and a source map (`app.wasm.map`, pointed at by a
`sourceMappingURL` section) are emitted so browser stack traces name `.kite`
files and lines. **One entry per function** — a frame resolves to the line the
function was declared on, not the line that trapped. Both are dropped by
`--release`, which is observable: the release output directory has no
`app.wasm.map`.

## The toolchain

```
kitec run    <file.kite>     compile and run
kitec check  <file.kite>     check without running
kitec build  <file.kite>     compile and report what was produced
kitec test   <file.kite>     run every `test_` function and doc example
kitec fmt    <file.kite>     lay the file out the one way
kitec doc    <file.kite>     the reference, from the doc comments
kitec fix    <file.kite>     apply every machine-applicable suggestion
kitec bundle <file.kite>     one executable that needs nothing installed
kitec pkg    [directory]     resolve dependency versions, write `kite.lock`
```

| Option | Effect |
|---|---|
| `--release` | `assert` is dropped, `require` is not; debug info is dropped |
| `--offline` | with `pkg`, resolve only from what is already vendored |
| `--check` | with `fmt`, report rather than rewrite (exit 1 if unformatted) |
| `--all` | with `doc`, include what is not `pub` |
| `--native` | with `run`, execute machine code under the JIT — no linker |
| `--emit <stage>` | `check`, `ast`, `hir`, `mir`, `kbc`, `wasm`, `native` |
| `--out <dir>` | where `--emit wasm` and `--emit native` write |
| `--update` | with `pkg`, allow `kite.lock` to change; without it a dependency whose bytes moved is an error |
| `--explain <CODE>` | the rationale for a diagnostic |
| `--version`, `--help` | |

Notes worth having:

- **`kitec run` is the bytecode VM.** It supplies the `fs` host group and
  nothing else. `--native` runs the same program as machine code under a JIT.
  It exits 1 on a trap and 0 on everything else — see *The whole command-line
  surface*.
- **`--out` is relative to the directory you run `kitec` in**, never to the
  entry file: from `proj/`, `kitec build page/main.kite --emit wasm --out out`
  writes `proj/out`, not `proj/page/out`.
- **`--emit wasm --out dist`** writes `app.wasm`, `app.js`, `index.html`,
  `app.wasm.map` (debug only), and `api.js` + `api.d.ts` when the **entry file
  declares at least one `pub fn` of its own** — a `pub struct` alone produces
  neither, and `main` never appears in the wrapper. See §7 of
  `05-concurrency-modules-ffi.md` for which parameter types survive the
  crossing; a `pub fn` taking a slice, an `Option<T>` or a `(T, error)` is left
  out of the wrapper with the reason written into the file. A program that listens also gets
  `serve.mjs`. The generated `index.html` holds a `<canvas id="stage">` and a
  `<pre id="out">` and nothing else, so a DOM program building into an empty
  directory finds no mount point — put your own `index.html` there first and it
  is left alone.
- **`--emit ast|hir|mir|kbc`** dumps that stage to stdout. Useful for
  confirming what the compiler actually did.
- **`kitec test`** runs `pub fn test_*() -> (int, error)` and every ` ```kite `
  doc comment example, reporting both — **in the entry file only**. A `use`d
  module's tests and doc examples are invisible to it; see `### test`.
- **`kitec doc`** reads signatures from the parse, so it cannot describe a
  function that is not there.
- **`kitec pkg`** needs a `kite.toml`; without one it says so. It resolves path
  and git dependencies, picks **one** version per name across the whole graph,
  and writes `kite.lock` with a content hash per dependency that is *verified*
  on later runs. A build never reaches the network — `pkg` is the only thing
  that fetches. There is no post-install script anywhere to put one.
- **The formatter works on tokens**, not the tree, so comments and blank lines
  survive. It decides indentation, spacing and blank lines and leaves line
  breaks to the author.

### No install at all

```bash
npx --package=@kite-lang/compiler-wasm kitec check f.kite
```

The package's one binary is called `kitec`, and the *first* argument is the
subcommand — `npx @kite-lang/compiler-wasm kitec check f.kite` would ask it to
run a command named `kitec`.

The WebAssembly build of the same Rust crate. Its dispatch has exactly five
cases — `run`, `check`, `build`, `fmt`, `doc` — one artefact for every platform,
nothing fetched at install time, and it works inside a browser-based Node such
as WebContainer where machine code cannot execute. `test`, `fix`, `bundle`,
`pkg`, `--native` and the language server are the native `kitec`'s.

As a library it exposes `compiler()`, whose methods are `run`, `check`,
`format`, `docs`, `checkModule`, `runModule` and
`build({ entry, siblings, release })` — the last throwing `BuildFailed` with the
rendered diagnostics. Siblings are keyed by **module name** — `checkout`, not
`checkout.kite`, because that is what `use` names.

`@kite-lang/cli` ships the native binary instead, with `kitec` and `kite-lsp`
on the path.

### Three npm packages, and which one to reach for

| Package | What it is | Reach for it when |
|---|---|---|
| `@kite-lang/cli` | the native `kitec` and `kite-lsp` | you want the whole CLI — `test`, `fix`, `bundle`, `pkg`, `--native` |
| `@kite-lang/compiler-wasm` | the same crate as WebAssembly: a five-command `kitec`, and `compiler()` as a library | there is no install, the platform has no native build, or something else is driving the compiler — a bundler, an editor, a test runner |
| `vite-plugin-kite` | a Vite plugin over `compiler-wasm` | you are building a **web page** |

For a terminal program `kitec` is the whole answer. For a page there are two
routes, and the plugin is the one a project takes — `examples/vite-starter` and
the POS app both do. `<script type="module" src="/src/main.kite">` is the
entire wiring: the plugin compiles the file, instantiates it, registers it
resident and calls `main`, so there is no JavaScript in the project at all and
no generated `index.html` to work around. Without a bundler, the other route is
`kitec build --emit wasm` into a directory that already holds your own
`index.html` — `examples/page/`, and the section under `### dom` above.

**The plugin's module model is not `kitec`'s, and the difference will bite.**
It never lets the compiler touch a filesystem: it reads the `.kite` files
**beside the entry**, keys each by its basename, adds one level inside every
`kite.toml` dependency keyed `name/file`, and hands that to
`build({ entry, siblings })`. So `use checkout` reaches `src/checkout.kite` and
`use markdown/render` reaches a dependency — but a sibling **directory** of
several files, which `kitec` reads whole, is not handed over at all and its
`use` fails. Keep a Vite project's own modules one file each.

## Where the specification is wrong or silent

- **`std/http`'s own header comment says the server half "needs no sockets"
  because "no Kite target has yet" got them.** It is stale: `http.open`,
  `accept`, `respond`, `run`, `shut` and `Server`/`Incoming` are all there, over
  the `net` host, and `--emit wasm` writes a `serve.mjs` for them.
- **The float-equality warning, and specification §16's line about it, point at
  `math.approx_eq`.** No such function exists. The prelude's
  `approx_eq(a, b, tolerance)` is the real one, and it is unqualified.
- **The specification never enumerates the builtin dotted paths.** They are only
  in `crates/kite-resolve/src/lib.rs`, which is why `io.println` and
  `use std/io` are the two mistakes a model makes first.
