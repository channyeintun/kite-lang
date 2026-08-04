# Platform research: what WebAssembly can and cannot do, August 2026

Every constraint that shaped Kite's design, with sources. Read this before
disagreeing with a decision in the specification — most of them are forced.

---

## 1. WebAssembly 3.0 is ratified and shipped

The specification landed **13 June 2026**, standardising nine features. All nine
ship in current versions of every major browser, Safari included.

| Feature | What it gives a language implementer |
|---|---|
| **WasmGC** | `struct` and `array` heap types managed by the host collector |
| **Native exception handling** | `exnref`, first-class throw/catch primitives |
| **Tail calls** | Recursion without stack growth |
| **Typed function references** | Type-checked indirect calls — the basis for cheap vtables |
| **Memory64** | Past the 4 GB ceiling |
| **128-bit SIMD** | Deterministic vectorisation |
| **Relaxed SIMD** | Wider vectorisation for numeric and ML work |
| **Multiple memories** | Separate memory regions per module |
| **Extended constant expressions** | Richer module initialisation |

**Consequence for Kite:** the language may assume GC, exceptions, tail calls, and
typed function references are present. No feature detection, no polyfill, no
fallback path in v1.

Sources: [WebAssembly 3.0 spec release](https://byteiota.com/webassembly-30-spec-release/) ·
[State of WebAssembly 2026](https://devnewsletter.com/p/state-of-webassembly-2026/)

---

## 2. WasmGC is baseline, including Safari

This is the finding that makes the whole project viable.

- Chrome enabled WasmGC by default in **December 2023**.
- Firefox shipped it in the same window.
- **Safari 18.2 shipped it in December 2024**, completing cross-browser baseline.

**Consequence:** Kite ships **no garbage collector in the `.wasm` binary**. The
host engine traces Kite's objects natively. Reported effects across the ecosystem
are 2–4 MB of runtime eliminated and **3–10× reductions in binary size and
startup time** for managed languages.

This is precisely why Grain and early AssemblyScript carry weight Kite will not:
they were designed when a language had to bring its own collector, and Grain's
own roadmap names WasmGC as the thing that will make it "even more effective."

**Timing matters here.** This design was not viable in 2022 and is
straightforward in 2026. Starting now is the correct call.

Sources: [WasmGC enabled by default in Chrome](https://developer.chrome.com/blog/wasmgc) ·
[V8: bringing GC languages to Wasm](https://v8.dev/blog/wasm-gc-porting) ·
[The State of WebAssembly 2025–2026](https://platform.uno/blog/the-state-of-webassembly-2025-2026/)

---

## 3. WasmGC's limitations, and how Kite avoids each

WasmGC is not a general-purpose heap. Its restrictions were the strongest
influence on Kite's type system.

| Limitation | Effect on a naive design | Kite's answer |
|---|---|---|
| **No interior pointers** | Go cannot be compiled faithfully — Go relies on `&struct.field` | No `&` operator exists. Unobservable. |
| **No flat aggregates in arrays** | `[]Point` becomes an array of *references*, not a packed buffer. A flat array of tuples must be transposed into parallel arrays or accept one GC object per element. | Accepted for ordinary code. `buffer.F64` gives a flat linear-memory buffer for the layout engine and renderer, where it matters. |
| **Fixed field indices and types** | Languages wanting dynamic field access must work around it | Kite is statically typed with no reflection. Unobservable. |
| **No weak references or finalizers** | Weak caches and resource cleanup on collection are impossible | `defer` for scope-bound resources; explicit eviction policies for caches. |
| **GC values cannot cross threads** | Any shared-memory threading model is unimplementable | See §5. This is the big one. |

Sources: [V8: bringing GC languages to Wasm](https://v8.dev/blog/wasm-gc-porting) ·
[WebAssembly limitations](https://qouteall.fun/qouteall-blog/2025/WebAsembly%20Limitations)

---

## 4. Strings are solved; the DOM is not

### JS String Builtins — shipped everywhere

Landed in **Safari 26.2** in 2025, completing browser coverage. Wasm modules can
operate on JavaScript string primitives directly — `concat`, `compare`, `length`
— with **no glue code and no copying**.

**Consequence:** Kite's `str` is a JavaScript string reference on the web target.
Passing a string to a DOM API is free. This removes what was historically the
single largest per-call cost in Wasm UI work.

### Direct DOM access — does not exist

There is **no standardised way for Wasm to call a Web API without JavaScript
glue**. Interest in Web IDL bindings for Wasm exists, and a Component Model
subset has been floated, but as of August 2026 there is **no formal proposal**.

**Consequence:** Kite defines an explicit `extern` host boundary
([spec §15](../SPECIFICATION.md#15-foreign-function-interface)) and *generates*
the JavaScript glue from those declarations. Pretending direct DOM access exists
would produce a design that cannot be implemented; hiding the boundary entirely
would make its cost invisible to the programmer. Declaring it once, in Kite, and
generating from it, is the honest middle.

Sources: [JS String Builtins proposal](https://github.com/WebAssembly/js-string-builtins/blob/main/proposals/js-string-builtins/Overview.md) ·
[State of WebAssembly 2026](https://devnewsletter.com/p/state-of-webassembly-2026/)

---

## 5. Threads and WasmGC are currently incompatible

The finding that determined Kite's concurrency design.

> There is **no way to use threads with WasmGC programs at all**, because there
> is no way to share reference values across threads.

The [shared-everything-threads proposal](https://github.com/WebAssembly/shared-everything-threads)
exists to fix exactly this. It adds `shared` annotations on tables, functions and
globals; sequentially-consistent and release-acquire accesses to shared WasmGC
data; and managed waiter queues for a futex-like wait/notify usable with GC
references. It is a **draft**. It has not shipped.

### What this means in practice, from languages already in production

- **Kotlin/Wasm:** `Dispatchers.Default` and `Dispatchers.IO` exist and *appear*
  to offer parallelism, but on Wasm they behave like `Dispatchers.Main` — they
  run on the same thread. Threads must be implemented via Web Workers, and a
  specific Wasm function cannot be spawned onto one.
- **Flutter Web:** multi-threaded rendering requires the server to send COOP/COEP
  headers, and even then the object graph is not shared.
- **Web Workers are not threads.** A worker runs its own browser-managed event
  loop with its own heap; a native thread executes a function until it returns.
  The abstractions do not line up.

**Consequence — and the key forward-compatibility bet:** Kite's `async`/`await`
surface says nothing about thread count. The `Share` marker
([spec §12.4](../SPECIFICATION.md#124-the-share-marker)) enforces the exact
invariant shared-everything-threads will require, starting in v1. Native and
bytecode targets get a real work-stealing pool immediately. The web target gets
isolate-based parallelism now and **true shared-heap parallelism with no source
change** when the proposal ships.

Sources: [shared-everything-threads](https://github.com/WebAssembly/shared-everything-threads) ·
[Kotlin/Wasm and web workers](https://marchuk.io/kotlin-wasm/) ·
[Concurrency in WebAssembly, ACM Queue](https://queue.acm.org/detail.cfm?id=3746173)

---

## 6. HTML-in-Canvas: correcting a common misreading

This project was initially scoped around the belief that HTML-in-canvas is the
future of the web. The research does not support that, in two distinct ways, and
the standard library design changed as a result.

### It is Chrome-only, and behind a flag

- Chrome: **origin trial**, M148–M151, plus `chrome://flags/#canvas-draw-element`.
- Brave / Edge / Chromium forks: same, inherited.
- **Firefox: no implementation announced.**
- **Safari / WebKit: no implementation announced.**

A single-engine API in origin trial is not a foundation for a language's standard
library.

### It points the opposite way to the assumption

`drawElementImage()` and `texElementImage2D()` render **DOM elements onto a
canvas**. The proposal exists so that canvas-based applications can embed *real,
accessible, CSS-styled DOM* — live form controls, selectable text, working
screen-reader semantics. It is a fix for canvas UI's weaknesses, not a
replacement for the DOM.

### What canvas-only UI actually costs

Flutter's CanvasKit renderer is the largest deployed example, and the results are
sobering:

- Accessibility is implemented by maintaining a **parallel hidden DOM semantics
  tree** (`<flt-semantics-host>`, `<flt-semantics>`) mirroring the canvas.
- **It is off by default for performance.** Users must activate an invisible
  button labelled "Enable accessibility."
- Text fields are announced as "edit, blank" by NVDA and VoiceOver because
  Flutter does not emit `<label>` elements.
- Accessibility is named as *the* biggest remaining Flutter Web gap in 2026, and
  Lighthouse scores it inaccurately — a perfect score on a Flutter app is
  misleading.

Text input is the deeper problem: IME composition for Chinese, Japanese and
Korean input, password managers, autofill, native text selection, spellcheck and
right-to-left cursor behaviour are all browser features that a canvas renderer
must reimplement from nothing.

### The conclusion Kite draws

The underlying instinct — that GPU-composited, retained-mode UI is worth having
— is **correct for the applications that need it**. Figma, Zed and Google Docs
took that path for good reasons, and every one of those reasons is about a
specific, demanding surface: a document canvas, a code editor, a design tool.
None of them is a reason to render a settings page that way. What does not
follow from any of it is that the *standard library* should be able to target
only canvas.

Kite therefore specifies **one UI API with two renderers**
([docs/04](04-stdlib-ui.md)): the same `Box`/`Flex`/`Text` program emits either a
real DOM tree or canvas draw commands, chosen at build time. Layout is computed
in Kite so both paths agree exactly, and nothing in the language bets on a
Chrome-only origin trial.

**The two are peers, and the DOM is the default.** An earlier draft of this
document called canvas first-class; that was the wrong conclusion to draw from
the evidence above, and it is corrected here. Everything this section catalogues
— the parallel semantics tree, the reimplemented text input, IME, autofill,
selection, the Lighthouse score that lies — is the cost of *not* using the
platform. Kite's target is web applications, and a web application that renders
to a canvas is one that has to rebuild HTML and CSS badly before it can start.

So the DOM renderer uses the platform rather than working around it: a field is a
real `<input>` or `<textarea>`, a picture is a real `<img>`, a control carries a
real ARIA role and label. Canvas is the equal alternative for the work it is
actually better at — dense, animated, GPU-composited surfaces; charts; games;
anything where a thousand elements would be a thousand elements. Choosing it is a
decision about *that screen*, not a decision about the language.

Sources: [HTML-in-Canvas browser support](https://html-in-canvas.dev/docs/browser-support/) ·
[WICG HTML-in-Canvas explainer](https://wicg.github.io/html-in-canvas/) ·
[Exploring the HTML-in-Canvas proposal, Codrops](https://tympanus.net/codrops/2026/05/13/exploring-the-html-in-canvas-proposal/) ·
[Flutter web accessibility](https://docs.flutter.dev/ui/accessibility/web-accessibility) ·
[Lighthouse gives your Flutter app a perfect accessibility score — it's lying](https://dev.to/sahland/lighthouse-gives-your-flutter-app-a-perfect-accessibility-score-its-lying-51f2)

---

## 7. Go's error handling is frozen

In 2025 the Go team published an official post announcing they would **pursue no
further error-handling syntax proposals**. Several were live at the time —
`if err ...` shorthand, `? return`, a ternary form — and all are now closed.

**Consequence:** the `(T, error)` *shape* is worth keeping; its *enforcement* is
worth adding, because Go itself has closed the door on doing so. Kite keeps the
shape and adds compile-time taint tracking
([spec §7.3](../SPECIFICATION.md#73-correlated-results-and-taint-analysis)) so an
error cannot be dropped and a value cannot be read on a failure path.

Sources: [Go issue 73897](https://github.com/golang/go/issues/73897) ·
[Go issue 71528](https://github.com/golang/go/issues/71528) ·
[Go's last words on error handling syntax](https://leapcell.medium.com/gos-last-words-on-error-handling-syntax-b74162750665)

---

## 8. Prior art, and where Kite sits

| Language | Approach | Why Kite differs |
|---|---|---|
| **MoonBit** | Wasm-first, targets wasm/wasm-gc/js/native. No LLVM — emits WAT, then `wasm-tools`. Reference counting on plain Wasm, host GC on WasmGC. | Closest neighbour, and validates the architecture. MoonBit is ML-flavoured with a rich type system; Kite deliberately trades expressiveness for a smaller concept budget and Go-shaped errors. |
| **Grain** | Functional, Wasm-native, ships its own GC (2017 design). | Predates WasmGC. Kite assumes the host collector and is imperative. |
| **AssemblyScript** | TypeScript subset to Wasm, linear memory. | Tied to TS semantics and npm-adjacent expectations. Kite has no JS-compatibility constraint. |
| **Onyx** | Compiles solely to Wasm, complete toolchain, Wasmer/WASIX runtime. | Systems-leaning and server-oriented. Kite is application- and UI-oriented. |
| **Virgil** | Lightweight high-performance systems language by a Wasm co-creator. | Systems focus, no UI story. |
| **Kotlin / Dart** | Mature WasmGC backends. | Carry large language surfaces and existing ecosystems. Kite's premise is a small surface. |

**Kite's actual niche:** *a minimal, explicit, application-and-UI language with
Go-shaped-but-enforced errors, no shared-memory concurrency concepts, and a
renderer-agnostic UI standard library.* Nothing in the table above occupies it.

The architectural lesson worth copying from MoonBit: **do not use LLVM for the
Wasm target.** Emit Wasm directly. It keeps the compiler fast, the toolchain
small, and the generated code inspectable.

Sources: [MoonBit announcement](https://www.moonbitlang.com/blog/first-announce) ·
[Introduction to MoonBit, The New Stack](https://thenewstack.io/introduction-to-moonbit-a-new-language-toolchain-for-wasm/) ·
[Grain, The New Stack](https://thenewstack.io/meet-grain-the-high-level-language-optimized-for-webassembly/) ·
[Onyx](https://onyxlang.io/) ·
[Introduction to Virgil](https://thenewstack.io/introduction-to-virgil-a-new-language-by-wasms-co-creator/)

---

## 9. Native compilation

For the native target the practical choice in Rust is
**[Cranelift](https://cranelift.dev/)**: written in Rust, developed by the
Bytecode Alliance, designed as a backend for Wasm and language implementations,
and explicitly prioritising compilation speed and simplicity over peak runtime
performance.

Rust's own `rustc_codegen_cranelift` is a Rust project goal targeting
production-readiness, showing roughly **20% reduction in code generation time**
versus LLVM on large projects. The tradeoff is less optimised output.

**Consequence:** Kite uses Cranelift for AOT native compilation and accepts the
optimisation gap. An LLVM backend remains possible later for release builds where
peak performance justifies the toolchain weight, but it is not on the v1 path —
adding LLVM early would dominate build times and binary size for the entire
project.

Sources: [Cranelift](https://cranelift.dev/) ·
[rustc_codegen_cranelift](https://github.com/rust-lang/rustc_codegen_cranelift) ·
[Production-ready Cranelift, Rust project goals](https://rust-lang.github.io/rust-project-goals/2025h2/production-ready-cranelift.html)

---

## 10. Server-side and WASI

WASI 0.2 stabilised in **January 2026** on the Component Model. WASI 0.3, adding
native async I/O to the Component Model, was scheduled for **February 2026**.

**Consequence:** out of scope for v1, but Kite's `extern` host boundary is
designed so a WASI world can be generated from the same declarations that
generate the browser glue. Kite programs should eventually run server-side
without a language change. This is a v2 concern; the design merely avoids
foreclosing it.

Sources: [Component Model](https://component-model.bytecodealliance.org/) ·
[WASI and the Component Model: current status](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)

---

## Summary: the forcing constraints

1. **WasmGC is baseline** → ship no collector, use host GC, expect small binaries.
2. **GC refs cannot cross threads, and the fix is a draft** → thread-agnostic
   `async` surface plus a `Share` marker enforced from v1.
3. **No direct DOM access, and none imminent** → explicit `extern` boundary with
   generated glue.
4. **JS String Builtins are baseline** → `str` is a JS string; DOM calls are free.
5. **No interior pointers, no flat aggregates** → no `&` operator; a typed-buffer
   escape hatch where layout matters.
6. **HTML-in-Canvas is Chrome-only and points the other way** → one UI API, two
   renderers.
7. **Go froze its error syntax** → keep the shape, add the enforcement.
