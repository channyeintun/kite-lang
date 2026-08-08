# Codex review of the three security-fix commits

Reviewed:

- `b0c9fac` — the initial security fixes
- `38d750d` — the first F20 lifetime fix
- `c440bdd` — the adversarial follow-up

The review used `security-review.md` as the claim set and checked the final
combined state, including paths not covered by the added regression tests.

## Current verdict

The string-lifetime blocker found in this review is now resolved. Wasm has one
language-owned `str`: a traced Unicode-scalar GC array. The permanent host
registry, representation switches, hidden environment selector, engine
proposal flag, and duplicate import ABIs have been removed. Internal string
operations stay in Wasm, and a fixed one-page bridge handles JavaScript
boundaries. Executable regressions cover Unicode operations, large
multi-chunk exports, declared hosts, maps and aggregate equality.

The six findings below were review suggestions outside that string migration.
Each was re-checked against the tree, and each is now addressed. Two of them
were not what the review thought they were, and saying how is the point of
keeping this file rather than deleting it.

## Findings

### High — the TOML overflow fix creates a `min_int` exponent trap

**Fixed, and the reason it could not fire was a worse bug.**

The mechanism was real: `int_of` accepts `-9223372036854775808`, `float_of`
parses an exponent with it, and `power_of_ten` negated its argument before
clamping — and `min_int` has no negation. But the reproducer
`x = 1e-9223372036854775808` never reached that code, because
`looks_like_date` claimed any `-` past the first character, so **every
negative exponent was being returned as text**. `1e-5` parsed to the string
`1e-5`, `float_at` answered with its fallback, and a number in a
configuration file silently stopped being one. Nothing caught it because
every exponent in the test suite was positive.

Both are fixed together, because fixing the date check alone would have made
the trap live: `looks_like_date` now exempts a `-` preceded by `e` or `E`,
and `power_of_ten` clamps into `[-400, 400]` before it negates. Tests cover
`1e-5`, `-1.5e-3`, `1E-5`, the `min_int` exponent, and a date carrying a
`-07:00` offset, which must still read as a date.

### High — lock verification happens after unverified bytes are installed

**Fixed.** Confirmed, and the build path made it worse than described:
nothing outside `pkg.rs` reads `kite.lock` at all, so the non-zero exit was
the only thing that refused while the rejected bytes sat in the directory the
next `kitec run` compiles. `--update` exists so that accepting changed bytes
is a decision somebody makes, and installing them first made that decision
for them.

Resolution no longer installs anything. `lock_dependencies` hashes the
checkout a candidate was cloned into and returns a `Resolution`;
`Resolution::install` is the only thing that writes `.kite/vendor/<name>`,
and `run` does not call it until the lockfile has agreed or `--update` has
accepted that it has not.

Still open, and worth doing: a build verifies nothing. Either `kitec build`
should hash the installed tree against `kite.lock` or the toolchain should
stop describing itself as locked.

### Medium — vendoring and hashing follow attacker-controlled symlinks

**Fixed.** Both walkers followed links — `copy_dir` via `Path::is_dir` and
`fs::copy`, `collect_kite_files` via `Path::is_dir` — so a directory link
aimed at `.` recursed until the stack ended and a file link aimed anywhere on
the machine was copied into the vendor tree and fed to the digest that is
supposed to identify that tree.

Both now read `file_type` from the directory entry, which answers about the
link rather than its target, and refuse a symlink rather than skipping it: a
digest whose meaning depends on a path outside the directory it names is not a
digest of that directory, and silently omitting the file would let a
dependency change what it contains without changing what it hashes to.

### Medium — the frame-pointer guard reads the first flag, not the effective flag

**Fixed, but the premise was wrong and the severity was lower than stated.**
rustc does not take the last occurrence: `parse_frame_pointer` *ratchets*, so
a later `no` cannot lower an earlier `yes`, and `-C force-frame-pointers=yes
-C force-frame-pointers=no` compiles *with* frame pointers. The dangerous
case the review describes — a false pass — therefore does not exist on any
toolchain this workspace supports.

What did exist was the opposite, all fail-safe: `no yes` was rejected though
it is safe, the stable spelling `=always` was rejected, and the panic's own
advice ("append `-C force-frame-pointers=yes`") could not work, because an
appended flag is not the first one. `says_yes` now reads every occurrence and
accepts `always` and `non-leaf` alongside the boolean spellings.

### Low — `SourceMap` still has an unchecked span-to-string path

**Fixed.** Confirmed as a hardening gap rather than a live panic: no pass
currently produces an out-of-range or mid-code-point span start, and the one
that used to — an unterminated literal ending in a multi-byte character — was
fixed at its source in `b0c9fac`. But `clamped`'s guarantee was overstated,
because the renderer asks for a *position* for every label before it asks for
any text, so `line_col` would have died before the clamping was reached.

`SourceFile::line_col` now clamps to the end of the text and walks back to a
character boundary, the same normalisation `clamped` performs. The LSP's
`position_at` had the identical residual gap — it clamped the range but still
sliced at a possibly non-boundary offset — and got the same two lines.

### Low — the server body ceiling is not measured in bytes

**Fixed, and the corruption half mattered more than the ceiling half.**

`body += chunk` decoded each `data` event on its own, so a character whose
UTF-8 bytes straddled two events became two replacement characters. That
needed no attacker: any non-ASCII body large enough to arrive in two pieces
was corrupted before the handler saw it, undetectably from the program's
side. The accounting error was the milder one — `body.length` counts UTF-16
code units, so a body of three-byte characters could reach about 24 MiB
against an "8 MiB" ceiling, permissive but bounded.

The adapter now keeps `Buffer` chunks, counts `chunk.length` (which is
bytes), and decodes once with `Buffer.concat` at end of stream. A test posts
a character split across two writes with a pause between them, so the split
is the test's rather than the kernel's.

## Additional suggestions

- **The 400-operation exponent loops: not worth a change.** Both loops are
  capped at 400 float operations, which is sub-microsecond; an attacker
  spends six bytes of exponent to buy 400 trivial operations, worse
  amplification than simply writing 400 short numbers. The real problem — a
  document sizing an unbounded loop — was already fixed by the cap.
- **Path components are now normalised to `/` before hashing.** Confirmed:
  `to_string_lossy` on the stripped path rendered nested names with the
  platform separator, so `src\a.kite` and `src/a.kite` were one file hashing
  two ways and a committed `kite.lock` matched only on the machine that wrote
  it. The digest itself is unambiguous where it matters — names and bodies
  are length-prefixed — though `to_string_lossy` still folds invalid UTF-8 to
  U+FFFD, which is unreachable as an attack because the contents must already
  be identical to collide.

## Areas that held up

No new correctness issue was found in the scoped alias map, E0403/E0404
collision diagnostics, Vite cache-directory confinement, no-host
accessibility execution, JSON infinity refusal, diagnostic control/bidi
escaping, or the SHA-256 compression implementation. JSON's exponent handling
was checked for the same `min_int` bug as TOML's and does not have it: it
keeps the sign as a flag and never negates a parsed value.
