---
name: kite
description: Kite language support — writing, reading, reviewing or debugging Kite; any .kite file; kitec and its diagnostics; kite.toml and kitec pkg; the std/ modules and the prelude. Kite is a WasmGC-first language that is in no model's training data, so unverified Kite is a guess and usually does not compile.
---

# Kite

Garbage-collected, WasmGC-first. Go's `(T, error)` made enforceable, Rust's traits
without lifetimes, newline-terminated statements, no truthiness, no implicit
conversion. **You have not seen this language before.** Anything carried over from
Rust, Go or TypeScript is a guess, and most of the guesses below are wrong.

## 1. Compile it. Do not guess.

The compiler is one command away. Being wrong first time is fine; handing over
unchecked Kite is not.

```bash
# in this repo
/Users/channyeintun/Desktop/may/target/release/kitec check file.kite

# anywhere, nothing installed
npx --yes --package=@kite-lang/compiler-wasm kitec check file.kite
```

`--package=` is load-bearing: `npx @kite-lang/compiler-wasm kitec check f.kite`
passes `kitec` as the *subcommand* and fails.

Every diagnostic carries a code, and the code explains itself. Run
`kitec --explain E0302` for any code you see — it prints the rule and its
rationale, not a restatement of the message. Also `kitec run`, `kitec test`
(`pub fn test_*` **and** every doc-comment example, **in the entry file only** —
a `use`d module's tests are invisible to it), `kitec fmt`, `kitec fix`,
`kitec build --emit wasm --out dist`. Write, check, fix, check again.

For a **web page** a project reaches for `vite-plugin-kite` rather than `kitec`:
`<script type="module" src="/src/main.kite">` in the project's own `index.html`
is the entire wiring. It hands the compiler the `.kite` files beside the entry
keyed by basename, so keep such a project's modules one file each.

## 2. Hello, world

```kite
fn main() {
    io.print("hello, world")
}
```

`io` is a compiler builtin reached by dotted path, not a module: `use std/io` is
`E0400`, and `io.println` does not exist.

## 3. The shape of a real program

```kite
use std/errors

struct Item {
    name: str
    pence: int
}

impl Display for Item {
    fn show(self) -> str {
        return "\(self.name) at \(self.pence)p"
    }
}

enum Payment {
    Cash
    Card(last4: str)
}

fn describe(p: Payment) -> str {
    return match p {
        Cash => "cash",
        Card(last4) => "card ending \(last4)",
    }
}

fn lookup(stock: [Item], name: str) -> (Item, error) {
    let hit = find(stock, |it| it.name == name)
    if hit == nil {
        return _, errors.new("no item called \(name)")
    }
    return hit, nil
}

fn total(stock: [Item], wanted: [str]) -> (int, error) {
    var sum = 0
    for name in wanted {
        let (item, err) = lookup(stock, name)
        check errors.wrap(err, "totalling \(name)")
        sum = sum + item.pence
    }
    return sum, nil
}

fn main() {
    let stock = [
        Item{ name: "bolt", pence: 30 },
        Item{ name: "nut", pence: 12 },
    ]
    for it in stock {
        io.print(it)
    }
    io.print(describe(Card(last4: "4242")))

    let (sum, err) = total(stock, ["bolt", "nut", "washer"])
    if err != nil {
        io.error(err.message())
        return
    }
    io.print("total \(sum)p")
}
```

## 4. What you will get wrong

| The reflex | Kite |
|---|---|
| `;` ends a statement | `;` is not a token — `E0002`. Newlines terminate statements. |
| Continue a line by starting the next one with `\|\|`, `+`, `-` | Continuation is decided by the **last** token of a line. A leading `\|\|` is a zero-argument closure, built and discarded (`E0117`); a leading `-` is a fresh statement with **no diagnostic** and a silently wrong answer. Put the operator at the end of the line it continues. `>` and `>>` never continue. |
| `/* … */` | Only `//`, `///`, `//!`. |
| A ` ```kite ` fence in a `///` comment is prose | It is a test. `kitec test` appends it to **its own module** — everything the comment documents is already in scope, no import — and runs it. Mark an illustration ` ```kite ignore `. |
| `'a'` is a char; `42i64`; `1.0f32` | No `char` type (a code point is an `int`, via `s.code_at(i)`), one `int`, one `float`, no literal suffixes. |
| `let f: float = 3` | `E0200`. No implicit conversion anywhere, not even for literals. Write `3.0`, or `n as float`; `as` converts int↔float and nothing else. |
| `if x { }` on a non-bool | No truthiness. A condition is exactly `bool` (`E0202`). |
| `a & b == c` parses as `a & (b == c)` | Bitwise binds **tighter** than comparison. And comparison does not chain: `a < b < c` is `E0100`. |
| `[3]int`, `xs[..2]`, `let r = 0..n` | No fixed-length arrays, no open-ended ranges, and a range is not a value — it is syntax for a `for` header and an index. Write both ends. |
| `m[key]` yields a `V`; `xs[i]` yields nil when missing | `m[key]` and `xs.get(i)` are always `Option<V>`. `xs[i]` **traps** out of range; `xs[a..b]` **clamps**. |
| `for k in someMap` | A map needs a pair binding: `for (k, v) in m`. One binding is `E0200`. |
| `s[0]`, `s.split(",")` | `str` is not indexable and has exactly five methods: `len` `slice` `index_of` `trim` `code_at`. `split`, `join`, `contains`, `replace`, `lower` are prelude *functions*. Slices have only `len` `get` `push`; maps only `len` `keys` `values`. |
| `if x != nil && x.field` | Narrowing does not cross `&&` (`E0200`). Nest the `if`. |
| A module-level `var` or `let` constant | There are **no** module-level bindings of any kind. A shared constant is a `pub fn` that returns it. |
| A closure captures a `var` | `E0211`. Captures are by value at creation time. Mutate through a named function taking a `var` parameter — there is no capture list. |
| Struct fields separated by commas | **Declarations** are newline-separated (a comma is `E0100`); commas belong to literals and patterns. |
| `P{ x: 1, ..base }` | `..base` comes **first**: `P{ ..base, x: 1 }`. Opposite of Rust. |
| Overloading, default arguments, named arguments, turbofish | None exist (`E0112`, `E0113`, `E0209`). Where a type cannot be inferred it names itself at the front: `Book.decode(doc)`, `NotFound.is(err)`, `let s: Stack<int> = Stack.empty()`. Many optional inputs → take a struct. |
| `Self`; `fn method<T>(self, …)` | No `Self` type, no method-level type parameters (`E0204`). Name the concrete type; put `<T>` on a free function or on the `impl` header. |
| `let x = if c { let y = 1  y } else { 0 }` | A value-`if` branch is exactly **one expression** — no statements, no tail expression. A `match` block arm holding more than one statement is `()`. Arms that need statements `return` instead. |
| Re-`let` the same name in one block | `E0112`; only a nested scope shadows. The one exception is rebinding an error that is already checked. |
| `println!` / `console.log`; deriving `Display` | `io.print(v)`. A user type needs a hand-written `impl Display` — `Display` never derives. `@derive` covers `Debug`, `Hash`, `Encode`, `Decode` only (and `Encode`/`Decode` need `use std/json` in the file). |
| `assert(cond)` | `assert(condition, message)` — two arguments, always, and it is a **builtin**, not a value you can pass around. `require` is the same but survives `--release`. |
| `(T, error)` is a tuple return | It is a *fallibility form*. `p.0` on one is `E0200`, `return (v, nil)` is `E0203`, and `return a, b` from a non-fallible function is `E0200`. A real tuple is returned as `return (1, 2)`. `-> error` alone is fallible too. |
| Ignore the error; `v, _ := f()`; a bare call | All `E0302` — an error cannot be dropped. `_ = f()` is the one deliberate discard, chosen to be greppable. |
| Read the value, then check the error | The value is unreadable until the error is disproved (`E0301`), and stays unreadable on the `err != nil` path. There is no zero value on the failure path. |
| `err.message()` on any error | Needs `err` proved non-nil first, or `E0301`. And `io.print(err)` is `E0200` — print `err.message()`. |
| `?` propagates | `check err`, a statement on its own line. In a non-fallible function — including `main` — it is `E0303`; test the error there instead. |
| `panic` / `recover`, try/catch | Traps (index out of range, divide by zero, failed `assert`) are **not catchable**. No unwinding, no handler. |
| A name imported anywhere is in scope everywhere | A module reaches only what it `use`s; `use` paths are relative to the importing file's directory; imports are always qualified, with no wildcard. |
| Files in one directory see each other | Only in a directory some `use` **names** — those files share one namespace and never import each other. **The file you hand `kitec` is a one-file module, and the directory it sits in is not a module at all**: a sibling `words.kite` is invisible until `use words`, then it is `words.greeting()`. `kitec` compiles the entry plus what its `use`s reach, transitively, and nothing else in the tree. |
| Calling an `async fn` starts running it | It only **queues**. Nothing in the body runs until something awaits the `Task<T>`. Concurrency is two calls, then two awaits. |
| `map(xs, \|n\| n * 2)` | `E0209` — nothing fixes `map`'s result type. Annotate: `map(xs, \|n: int\| n * 2)`. `filter` needs no annotation. |
| `match s { Shape.Circle(r) => … }` | A **qualified** pattern silently becomes a wildcard that matches everything and satisfies exhaustiveness alone, with no diagnostic. Patterns are written bare: `Circle(r)`. |
| `fn main(args: [str])`, `os.args()`, `env.get` | **There are no command-line arguments at all.** The whole input surface is `io.read_line()`, and at end of input it returns `""` and goes on returning it — a `for` loop over it never ends unless you break on `""`. Words after `kitec run f.kite` are the compiler's. |
| A failed run exits non-zero | **There is no exit status and no `exit(code)`.** A trap exits 1; `io.error(msg)` then `return` exits 0, which a shell cannot tell from success. |
| `--emit wasm` writes a page that works | The generated `index.html` is a `<canvas id="stage">` and a `<pre id="out">`, nothing else. A DOM program finds no mount point, `dom.find("#app")` is nil, the `== nil` guard returns, and **no one reports anything**. Put your own `index.html` in `--out` first — it is left alone — or mount into `dom.body()`, an `Option` that needs narrowing. **Your page must also load the module**, or it is still blank: see below. |

### A page of your own has to start the program

`--out` leaves your `index.html` alone, but nothing in it runs Kite until you
say so. Without these four lines the page is blank and the console is empty —
the same silent failure as a missing mount point, one step later.

```html
<ul id="list"></ul>
<script type="module">
  import { instantiate, resident } from "./app.js";
  const exports = await instantiate("./app.wasm");
  resident(exports);      // keeps listeners and tasks alive after main returns
  exports.main();
</script>
```

`resident` is not optional for a program that listens: without it, everything
`main` attached is collected the moment `main` returns.

## 5. The reference pages

Read the page before writing much of the thing it covers; they are checked against
the compiler and they say where SPECIFICATION.md is wrong.

| Page | Read it for |
|---|---|
| `references/01-lexical-and-types.md` | tokens, literals, interpolation, block strings, semicolon insertion, precedence, primitives, `str`, slices/maps/tuples, `Option` and narrowing, `as`, type aliases |
| `references/02-declarations-and-control-flow.md` | `let`/`var`, `pub`, functions and `var` parameters, closures, `if`/`for`/`match`/`defer`, `assert`/`require` |
| `references/03-errors.md` | anything with an `error` in it: the taint analysis, `check`, `errors.wrap`, typed errors and `T.is`/`T.as` |
| `references/04-structs-enums-traits-generics.md` | structs, methods, enums and how variant names resolve, traits, `dyn`, generics and inference, `@derive` |
| `references/05-concurrency-modules-ffi.md` | `async`/`await`/`Task<T>`, `Share`, modules, `kite.toml` and `kitec pkg`, memory and `E0800`, `JsValue`, `extern`/`@host`, `std/js` |
| `references/06-stdlib-and-diagnostics.md` | what a function is *called*: the prelude, the builtin dotted paths, the twenty `std/` modules, all 48 diagnostic codes, the `kitec` CLI |

Three worked projects sit in `/Users/channyeintun/Desktop/may/examples/`, and they are the three
shapes usually asked for: `page/` — hand-written `index.html` with `--emit wasm`
built into that same directory; `vite-starter/` — `vite-plugin-kite`, one file
per module; `inventory/` — a module directory whose two files share a namespace.
The rest of that directory is single-file demos of one feature each.

When a page does not answer it, `kitec check` a five-line file. That is faster than
reading, and it is the authority — the compiler wins over every document here.
