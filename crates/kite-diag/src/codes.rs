//! Stable diagnostic codes.
//!
//! A code is part of the language's public surface: it appears in output, in
//! `kite --explain`, and in test annotations. Codes are never reused for a
//! different meaning, even if a rule is removed.
//!
//! Every code here is one the compiler actually emits. One that no longer is —
//! or never was — comes out rather than staying to be explained: `--explain`
//! answering for a diagnostic nobody can provoke is a worse kind of wrong than
//! not answering, because it reads as documentation of a rule.
//!
//! Ranges:
//!   E0000–E0099  lexical
//!   E0100–E0199  syntax and bindings
//!   E0200–E0299  types, traits, patterns
//!   E0300–E0399  error handling (taint analysis)
//!   E0400–E0499  modules and visibility
//!   E0500–E0599  concurrency and Share
//!   E0600–E0699  cryptography
//!   E0700–E0799  derivation
//!   E0800–E0899  exclusivity
//!   E0900–E0999  the compiler failing, rather than the program

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Code(pub &'static str);

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Code {
    /// Whether the lexer reports this (E0000–E0099): the file could not be
    /// read as tokens whole, and what did not lex is missing from them.
    pub fn is_lexical(self) -> bool {
        self.0.len() == 5 && self.0.starts_with("E00")
    }
}

macro_rules! codes {
    ($($name:ident = $code:literal, $short:literal, $explain:literal;)*) => {
        $(pub const $name: Code = Code($code);)*

        /// Long-form rationale for `kite --explain`. The specification is the
        /// source text for these.
        pub fn explain(code: &str) -> Option<(&'static str, &'static str)> {
            match code {
                $($code => Some(($short, $explain)),)*
                _ => None,
            }
        }

        pub fn all() -> &'static [(&'static str, &'static str)] {
            &[$(($code, $short)),*]
        }
    };
}

codes! {
    // ---- lexical ----------------------------------------------------------
    E0001 = "E0001", "unterminated string literal",
        "A string literal was opened and never closed. The lexer reports it where the string starts. The situations:\n\
         \n\
         - A `\"` string that reaches the end of its line. A `\"` string may not span lines, so a closing quote forgotten at the end of a line is this error, and so is a line break written inside one. What it swallowed on that line — a `)` closing a call — is not reported missing as well. The next line is read as code again.\n\
         - A `\"\"\"` block string with no closing `\"\"\"`. It reads to the end of the file as its text, so this is the file's one error: the braces it swallowed are not reported as unclosed on top of it. When block strings are nested in holes, the innermost one that ran to the end of the file is the one reported. A block string is how text is written over several lines (§2.4).\n\
         - A character literal, `'a'`, with no closing `'`. Kite has no character type, so a closed one is refused too (`E0200`): write a one-character `str` in double quotes.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       // E0001: the string on the next line is never closed\n\
         \x20       let s = \"abc\n\
         \x20       io.print(s)\n\
         \x20   }\n\
         \n\
         The fix is the closing quote:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let s = \"abc\"\n\
         \x20       io.print(s)\n\
         \x20   }";

    E0002 = "E0002", "invalid character in source",
        "The lexer found a character that cannot begin any Kite token. It skips the character and reports it where it stands. The ones that come in by habit:\n\
         \n\
         - `;`. Kite has no semicolons: a statement ends at its line break (§3), so the `;` is deleted.\n\
         - `#` for a comment. Write `//`.\n\
         - `?`, `~`, a backtick and `\\`. None is an operator or a delimiter in Kite.\n\
         - A character that only looks like one: a curly quote in place of `\"`, an en dash in place of `-`. These arrive by copy and paste from a document or a web page; retype them.\n\
         \n\
         An identifier may use any Unicode letter, so a name in another script is not this error. A parse error beside it — `expected a line break` after a stray character in the middle of a line — is the gap the skipped character left; removing the character removes both.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let a = 1\n\
         \x20       ~\n\
         \x20       io.print(a)\n\
         \x20   }\n\
         \n\
         The fix is to remove it (or, if it was meant to be a comment, to write `//`):\n\
         \n\
         \x20   fn main() {\n\
         \x20       let a = 1\n\
         \x20       io.print(a)\n\
         \x20   }";

    E0003 = "E0003", "invalid escape sequence",
        "A backslash escape in a string that Kite does not know. The escapes are `\\n`, `\\t`, `\\r`, `\\0`, `\\\\`, `\\\"`, `\\'` and `\\u{…}`, and `\\(` which opens an interpolation (§2.4). The situations:\n\
         \n\
         - An unknown letter or character: `\"C:\\path\"`, `\"\\d\"`, `\"\\e\"`. A backslash that is meant as one is written twice: `\"C:\\\\path\"`.\n\
         - `\\u` not followed by `{`: `\"\\u0041\"`. A unicode escape is hex digits between braces, `\"\\u{41}\"`.\n\
         - A `\\u{` that is never closed: `\"\\u{41\"`.\n\
         - A `\\u{…}` that holds no character: no digits, a digit that is not hexadecimal, a value past `10FFFF`, or one in the surrogate range `D800` to `DFFF`.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(\"C:\\path\")\n\
         \x20   }\n\
         \n\
         The fix is the escaped backslash:\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(\"C:\\\\path\")\n\
         \x20   }";

    E0004 = "E0004", "invalid number literal",
        "A numeric literal is malformed. A digit separator `_` sits between two \
         digits, in every radix and every part of a literal — `1_000`, \
         `0xFF_FF`, `1.5e1_0` — so it cannot end the digits, follow a radix \
         prefix, double up, or touch a `.` or an exponent: `1_`, `0x_F`, \
         `1__0` and `1_.5` are all refused. A float must have digits on both \
         sides of the point. A digit outside the literal's radix — `0b102`, \
         `0o8`, `0xFG` — makes the whole literal one bad number; it is not \
         read as a number followed by another.\n\n\
         A literal too large for its type is refused too: an `int` above \
         9223372036854775807, and a `float` too large to be finite, such as \
         `1e999`, which would otherwise become infinity without a word.\n\n\
         A type suffix — `42i32`, `2.5f32` — is also refused. Kite has one \
         integer type and one float, so a suffix names nothing. It used to be \
         consumed and thrown away, which made `300i8` read as a width the \
         compiler was checking when there was no `i8` for the value to \
         overflow.";

    E0006 = "E0006", "string interpolation nested too deeply",
        "A string may hold an interpolation, which may hold a string, which may hold another interpolation. The scanner follows that by calling itself, so the nesting in the file is stack depth in the compiler, and three bytes of source buy a level.\n\
         \n\
         Running out of stack aborts the process rather than reporting anything — which would mean a file that kills the language server the moment it is opened — so there is a ceiling instead: 64 levels of `\\(\"…\")` inside one another. No program written on purpose comes near it; a file that does is nearly always generated, or a string that lost its closing quote partway and read the rest of the file as nested holes. The error is reported once, at the string that opened the first level past the ceiling.\n\
         \n\
         Example (the line is 66 strings deep, one past the ceiling and a long way past anything readable):\n\
         \n\
         \x20   fn main() {\n\
         \x20       let s = \"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(\"\\(1)\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\")\"\n\
         \x20       io.print(s)\n\
         \x20   }\n\
         \n\
         The fix is to name the pieces: a `let` per level reads better than any nesting, and it is the same text:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let a = \"\\(1)\"\n\
         \x20       let b = \"\\(a)\"\n\
         \x20       let s = \"\\(b)\"\n\
         \x20       io.print(s)\n\
         \x20   }";

    E0005 = "E0005", "block comments are not supported",
        "Kite's comments are line comments: `//` for a comment, `///` for the documentation of the declaration that follows, and `//!` for the module's own, at the top of its file. There is no block comment. Nested block comments are a recurring source of lexer bugs and every editor has supported toggling line comments for decades.\n\
         \n\
         The lexer reads on to the closing `*/` and skips what it found, so a comment that was meant is one error and the code after it is read normally. A `/*` inside a string or a line comment is just text.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       /* print one */\n\
         \x20       io.print(1)\n\
         \x20   }\n\
         \n\
         The fix is a line comment per line:\n\
         \n\
         \x20   fn main() {\n\
         \x20       // print one\n\
         \x20       io.print(1)\n\
         \x20   }";

    // ---- syntax and bindings ---------------------------------------------
    E0100 = "E0100", "unexpected token",
        "The parser found something that cannot be written where it is. The \
         message says what was expected and what was there instead, and the \
         caret is on the token where the parser gave up, which can be one \
         past the mistake: a line that ends in an operator carries on to \
         the next line, so `let x = 1 +` followed by a line holding `}` is \
         reported at the `}`.\n\
         \n\
         Every syntax error that has no code of its own is this one. The \
         situations:\n\
         \n\
         - A token that does not fit. `let x = (1 + )` has no operand \
         after the `+`, `let x = 1 let y = 2` has no line break between \
         two statements, `f(1 2)` and `{\"a\": 1 \"b\": 2}` have no comma \
         between their arguments or entries, and `[1 2]` is `expected \
         `]``. Look at the line the caret is on and at the one before it.\n\
         - A comma where a line break goes. Struct fields and enum \
         variants are separated by line breaks, not commas, so `struct P \
         { a: int, b: int }` and `enum E { A, B }` are refused. Put each \
         on a line of its own.\n\
         - A declaration where only statements or members go. A function \
         cannot be written inside a function (`expected a statement, \
         found `fn``), and a method cannot be written inside a `struct` \
         (`expected a field, found `fn``). Write a helper at the top \
         level, and write methods in an `impl` block. The reverse also \
         holds, because only declarations may stand at the top of a file: \
         `return 1` there is `expected a declaration`.\n\
         - A chain. `1 < a < 3` is refused: write `1 < a && a < 3`. \
         `1..2..3` is refused too, since a range has one start and one \
         end.\n\
         - An `if` used as a value without an `else`: `let x = if c { 1 \
         }` would have no value when `c` is false. Write the `else`.\n\
         - An attribute Kite does not have. `@host(\"group\")` and \
         `@derive(...)` are the only two, so `@oops` is refused.\n\
         - `pub use`. A module's imports are its own and are not exported \
         (§4.3), so there is no `pub use`; each module that wants a name \
         writes its own `use`. And an `extern fn` cannot be `async`: a \
         host call is answered when it is made, and the Kite function \
         that wraps the declaration is the one that is `async`.\n\
         - A string interpolation that does not close, `\"a \\(1 + \"`, or \
         one that holds more than a single expression, `\"a \\(1 2)\"`. An \
         interpolated string is not a pattern either: `match n { \"a\\(n)\" \
         => … }` is `only a literal may appear here`.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct P { a: int, b: int }    // E0100: a comma between fields\n\
         \n\
         The fix is a line break where the comma was:\n\
         \n\
         \x20   struct P {\n\
         \x20       a: int\n\
         \x20       b: int\n\
         \x20   }";

    E0101 = "E0101", "unclosed delimiter",
        "A `{` was opened and never closed. Only a brace is reported this way: a missing `)` is `E0100`, `expected `)``, at the place the parser needed it.\n\
         \n\
         One mistake is one report, however many braces it leaves open. Where the file's indentation shows which block lost its `}` — a line indented like a new declaration, with a block still open above it — the report names that `{`, and the parser carries on with the declarations after it, so one missing brace does not become a hundred errors. Where nothing says, it is one report at the end of the file for however many braces are still open. The situations are all the same mistake: a function body, an `if`, `for` or `match` block, an `impl`, a `struct` or an `enum` body.\n\
         \n\
         Example:\n\
         \n\
         \x20   // E0101: the `{` below is never closed\n\
         \x20   fn main() {\n\
         \x20       io.print(1)\n\
         \n\
         The fix is the closing brace:\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(1)\n\
         \x20   }";

    E0102 = "E0102", "expression nested too deeply",
        "The parser is recursive descent, so how deeply an expression, type, \
         pattern or block nests in the file is how deep the native stack goes \
         in the compiler.\n\n\
         Source is untrusted input — the language server compiles a document \
         the moment it is opened — and exhausting the stack is a guard-page \
         abort, not a panic, so nothing can catch it. A ceiling turns that \
         into this diagnostic. The bytecode VM has bounded call depth for the \
         same reason; this is the same rule applied to the front end.\n\n\
         There are two ceilings. Brackets, blocks, types, patterns and prefix \
         operators such as `-` and `!` may nest 256 levels deep. A chain — \
         each `+` of `a + b + c + …`, each `.f` and each call of \
         `x.f().g()…`, each `else if` — is counted apart, and may be 8,192 \
         links long, 1,024 in the compiler built for WebAssembly, which runs \
         on the JavaScript engine's stack. The parser reads a chain in a \
         loop, but the tree it builds is as deep as the chain is long, and \
         every pass after the parser walks that tree by recursion.";

    E0110 = "E0110", "use of possibly-uninitialised binding",
        "A `let` declared without a value, `let x: int`, may be given one later, but only if the compiler can prove that exactly one assignment has happened on every path before the value is read. A read it cannot prove is refused, because there is no zero value to fall back on. The situations:\n\
         \n\
         - A read with no assignment before it: `let x: int` and then `io.print(x)`.\n\
         - An assignment on only some paths: `if ok { x = 1 }` and then a read. The note says the `let` must be assigned on every path before it is read.\n\
         - A `var` declared without a value, `var x: int`: a `var` is initialised where it is declared (`var x = 0`), and the diagnostic says so at the declaration. `let` is the one that may wait.\n\
         - `x += 1` on a binding that has no value yet: it reads `x` first.\n\
         \n\
         A second assignment to the `let` is a different mistake, `E0114`, and so is an assignment to it inside a loop, which could run twice.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let ok = false\n\
         \x20       let x: int\n\
         \x20       if ok {\n\
         \x20           x = 1\n\
         \x20       }\n\
         \x20       io.print(x)    // E0110: when `ok` is false, `x` has no value\n\
         \x20   }\n\
         \n\
         The fix is to assign it on the other path too:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let ok = false\n\
         \x20       let x: int\n\
         \x20       if ok {\n\
         \x20           x = 1\n\
         \x20       } else {\n\
         \x20           x = 0\n\
         \x20       }\n\
         \x20       io.print(x)\n\
         \x20   }";

    E0111 = "E0111", "unknown name",
        "A name was used that nothing in scope defines. A name is looked up \
         in the blocks around the use, then among the module's own \
         declarations, then in the prelude. Nothing is brought in from \
         another module unqualified (§13.1), so another module's names are \
         written `module.name`. The situations:\n\
         \n\
         - A variable, function or constant that is not there: \
         `io.print(cuont)` where the variable is `count`. A name is in \
         scope from its `let` to the end of the block it is declared in, \
         so using it before the `let`, or after the `}` that closes the \
         block, is this error too.\n\
         - A module that was not imported. `text.trim(s)` without `use \
         std/text` is `cannot find `text``. Write the `use`.\n\
         - A name an imported module does not declare: `text.trimm(s)` is \
         `cannot find `trimm` in module `text``, and when a name it does \
         declare is close enough the note offers it. A name the module \
         declares but does not mark `pub` is `E0401` instead.\n\
         - A type that does not exist, written where a value is expected: \
         `Foo.make()`. Written where a type is expected it is `E0204`.\n\
         - A variant its enum does not have: `Dir.Left` where `Dir` has \
         `Up` and `Down`. The note lists the variants it has. The same \
         holds in a pattern.\n\
         - A loop label no enclosing loop carries: `break outer` with no \
         `outer: for …` around it.\n\
         - `self` in a function that is not a method. `self` is the \
         receiver of a method, so there is none to name.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let count = 0\n\
         \x20       io.print(cuont)    // E0111: `cuont` is not in scope\n\
         \x20   }\n\
         \n\
         The fix is the name that is in scope:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let count = 0\n\
         \x20       io.print(count)\n\
         \x20   }";

    E0112 = "E0112", "duplicate definition",
        "Two items in the same scope share a name. Shadowing is permitted in a \
         nested scope, but not in the same one, where it is almost always a \
         typo.\n\nA type's methods are one scope across all of its `impl` blocks, \
         trait implementations included, so an inherent method and a trait's of \
         the same name, or two traits', are a clash too: `x.m()` must name one \
         method, not whichever block came first. Likewise a type parameter \
         bounded by two traits that both declare `m`.\n\n\
         A loop label is a name too: a loop inside another may not reuse the \
         outer one's label, or `break outer` would leave the inner loop. And a \
         map literal giving one known key twice, `{\"a\": 1, \"a\": 2}`, is a \
         warning under this code: the later value replaces the earlier, which \
         is never seen. A key is known when it is a literal or a constant, a \
         negated number, or a tuple of those; floats are compared by value, \
         so `0.0` and `-0.0` are one key.";

    E0113 = "E0113", "wrong number of arguments",
        "Kite has no default arguments, no variadics, no named arguments and no overloading, so what is written at a call, a variant or a pattern has to match the declaration exactly. A function needing many optional inputs takes a struct, whose literal names every field. The situations:\n\
         \n\
         - A call with too many or too few arguments: `f(1, 2, 3)` for `fn f(a: int, b: int)`. The message counts both, and the declaration is the secondary label.\n\
         - A named argument: `f(a: 1, b: 2)`. Write `f(1, 2)`; a struct is the way to name what is passed.\n\
         - A variant given a payload it does not carry (`Shape.Dot(1)`) or a different number of values than it carries; and a variant literal missing a field it names.\n\
         - A tuple pattern in a `match` arm that names a different number of elements than the tuple has. (A `let (a, b) = t` that does not fit its tuple is `E0200`, `expected 3 bindings, found 2`.)\n\
         - A method in an `impl` of a trait that takes a different number of parameters than the trait declares.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn add(a: int, b: int) -> int {\n\
         \x20       return a + b\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(add(1))    // E0113: `add` takes 2 arguments, but 1 was given\n\
         \x20   }\n\
         \n\
         The fix is the missing argument:\n\
         \n\
         \x20   fn add(a: int, b: int) -> int {\n\
         \x20       return a + b\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(add(1, 2))\n\
         \x20   }";

    E0114 = "E0114", "cannot assign to immutable binding",
        "A binding made with `let` is immutable: it names one value for as long as it is in scope, and writing to it, or to anything it holds, is refused. `var` is the binding that changes. Immutability is the default because it removes the value-versus-pointer question (a `let` is never a place something else can change under you), maps onto WasmGC's per-field mutability flag, and makes most types safe to share between tasks. The situations:\n\
         \n\
         - Assigning to a `let` binding, `total = 1` or `total += i`. A loop that accumulates is the usual case. The fix offered is `var`, wherever `var` would compile.\n\
         - Changing a field that is not `var`: `c.n = 2` where the struct says `n: int`. A field is immutable unless the struct marks it `var`, or the value can be rebuilt with `Counter{ ..c, n: 2 }`.\n\
         - Changing what a `let` holds in place: `xs.push(2)`, `xs[0] = 3` or `m[k] = v` on a `let` slice or map. A slice or a map is a value, so changing its contents changes the binding that holds it.\n\
         - Changing `self` in a method whose receiver is plain `self`: write `var self`, and the caller must then hold the value in a `var` (which is what makes the change visible where the call is written). Calling a `var self` method on a `let` is the same error.\n\
         - Assigning inside a loop to a `let` declared without a value (`let x: int`, which may be assigned once). The body could run twice.\n\
         - Assigning to something that is not a place at all: `f() = 1`, or `a + b = 1`. A place is a name, a field or an element.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Counter {\n\
         \x20       n: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let total = 0\n\
         \x20       for i in 0..3 {\n\
         \x20           total += i    // E0114: `total` is a `let`\n\
         \x20       }\n\
         \x20       let c = Counter{ n: 1 }\n\
         \x20       c.n = 2           // E0114: so is a field that is not `var`\n\
         \x20       io.print(total)\n\
         \x20   }\n\
         \n\
         The fix is `var` on the binding and on the field. Make something `var` only when it has to change; everything else stays a `let`:\n\
         \n\
         \x20   struct Counter {\n\
         \x20       var n: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       var total = 0\n\
         \x20       for i in 0..3 {\n\
         \x20           total += i\n\
         \x20       }\n\
         \x20       var c = Counter{ n: 1 }\n\
         \x20       c.n = 2\n\
         \x20       io.print(total + c.n)\n\
         \x20   }";

    E0115 = "E0115", "`break` or `continue` outside a loop",
        "`break` and `continue` only mean something inside a loop, and they are checked against the loop that is *lexically* around them. The situations:\n\
         \n\
         - Nowhere near a loop: in a function body, or in an `if` or a `match` that no `for` encloses.\n\
         - Inside a closure that is written in a loop. A closure is a function of its own and runs when it is called, not as one step of the loop, so it has no loop to leave. Have the closure return a value and let the loop decide.\n\
         - After the loop's `}`, which is outside it.\n\
         \n\
         To leave a function early, `return`; to leave an outer loop from an inner one, label it (`break outer`).\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       if n > 2 {\n\
         \x20           break    // E0115: no loop for this `break` to leave\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         The fix, where a return was meant:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       if n > 2 {\n\
         \x20           return\n\
         \x20       }\n\
         \x20   }";

    E0116 = "E0116", "unreachable code",
        "This is a warning, not an error: the program still compiles, but the code it points at can never run, which is nearly always a mistake rather than an intention. The situations:\n\
         \n\
         - A statement after one that always leaves: a `return`, a `break` or a `continue`.\n\
         - A `match` arm whose every value is taken by the arms above it. The commonest cause is a name meant as a variant that is not one: `Down` where the enum is `Dir` and the variant is `Dir.Down`, which is a binding, and a binding matches everything, so every arm after it is dead.\n\
         - A guarded arm shadows nothing, because its guard may fail.\n\
         - A literal or a range inside a range above it (`5` after `1..=9`) is dead the same way, and so is a range that matches nothing: ends the wrong way round, `9..=4`, or `5..5`, which leaves out its one value.\n\
         \n\
         Example:\n\
         \n\
         \x20   enum Dir {\n\
         \x20       Up\n\
         \x20       Down\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let d = Dir.Down\n\
         \x20       match d {\n\
         \x20           Dir.Up => io.print(\"up\")\n\
         \x20           Down => io.print(\"down\")    // a binding: it matches everything\n\
         \x20           Dir.Down => io.print(\"never\")    // E0116\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         The fix is to name the variant, so that each arm covers its own value. For a statement that follows a `return`, delete it or move it above:\n\
         \n\
         \x20   enum Dir {\n\
         \x20       Up\n\
         \x20       Down\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let d = Dir.Down\n\
         \x20       match d {\n\
         \x20           Dir.Up => io.print(\"up\")\n\
         \x20           Dir.Down => io.print(\"down\")\n\
         \x20       }\n\
         \x20   }";

    E0117 = "E0117", "statement has no effect",
        "A closure written as a statement is built and thrown away. Nothing \
         calls it, so nothing it says can happen.\n\n\
         Almost always this is a line continuation that did not continue. A \
         statement carries on to the next line when it *ends* in an operator, \
         so an `||` at the start of a line is not the tail of the expression \
         above it — it is a closure with no parameters, which is what `||` \
         means where a value is expected:\n\n\
         \x20   let ok = (c >= 48 && c <= 57)\n\
         \x20       || (c >= 65 && c <= 90)     // a closure, discarded\n\n\
         The first line is a complete statement and the answer is wrong with \
         nothing said about it. Put the operator at the end of the line it \
         continues.\n\n\
         The closure is one case of a wider rule: an expression statement \
         with no call and no `await` anywhere in it computes a value and \
         throws it away, so it is refused too. A line opening with `-` is a \
         negation, not a subtraction carried over from the line above; one \
         opening with `[` is a slice literal, not an index; and `done == true` \
         compares where `done = true` was meant:\n\n\
         \x20   let n = 10\n\
         \x20       - 2                         // n is 10; this line does nothing\n\
         \x20   done == true                    // write `done = true`\n\n\
         A statement holding a call, such as `f() + 1`, does something, and \
         is left alone.\n\n\
         An arm of a `match` statement is a statement as well, since its value \
         is thrown away: `0 => 1` and `0 => { done == true }` are refused there \
         as they are on a line of their own. `0 => ()` and `0 => {}` say \
         that nothing happens, and are fine.\n\n\
         An index that can trap and an interpolation whose hole runs `show` \
         are not counted as effects, any more than an addition that can \
         overflow is. To evaluate one for that alone, say so:\n\n\
         \x20   _ = xs[i]                       // traps if `i` is out of range\n\n\
         An arm holds an expression, not an assignment, so there it is \
         `0 => { _ = xs[i] }`.";

    E0118 = "E0118", "module-level binding is not a constant",
        "A `let` at the top of a module names a value the compiler works out \
         while compiling, and every use of the name is that value. So the \
         right-hand side has to be something the compiler can work out: a \
         literal, an operator applied to constants, an interpolation whose \
         holes are all constants, or another constant.\n\n\
         A call is not, even one that would always return the same thing. \
         Running a program's code during its own compilation is a second \
         evaluation order, and which functions are available to it becomes a \
         language rule nobody can predict — so there is one evaluation order \
         here, and it is the one that already exists.\n\n\
         Arithmetic on constants is done while compiling, so an operation \
         with no `int` result — dividing by zero, overflowing, shifting by a \
         negative amount or by 64 or more — is reported here rather than \
         trapping in one build and wrapping in another.\n\n\
         There is no module-level `var` at all. A mutable binding two \
         functions can both reach is shared state neither signature mentions, \
         which is what `Share` and the closure capture rule exist to prevent. \
         Put it in a struct and pass it to what changes it.";

    E0119 = "E0119", "constant defined in terms of itself",
        "A module-level constant's value is computed from the constants it names (§4.2), so a cycle among them has no value to compute. One report is made for the cycle, not one for each member, and the note lists the chain in the order it was followed, ending where it came back to where it began: `A → B → A`. The situations:\n\
         \n\
         - A constant that names itself: `let A = A + 1`.\n\
         - A chain that comes back: `A` uses `B`, which uses `C`, which uses `A`.\n\
         \n\
         A constant cannot call a function (`E0118`), so a cycle can only run through other constants.\n\
         \n\
         Example:\n\
         \n\
         \x20   let A = B + 1\n\
         \x20   let B = A + 1    // E0119: `A` is defined in terms of itself\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(A)\n\
         \x20   }\n\
         \n\
         The fix is to break the chain with a value that stands on its own:\n\
         \n\
         \x20   let A = B + 1\n\
         \x20   let B = 1\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(A)\n\
         \x20   }";

    // ---- types ------------------------------------------------------------
    E0200 = "E0200", "type mismatch",
        "An expression has one type and this position needs another. Kite converts nothing implicitly — no numeric widening, no truthiness, no `nil` for a missing value — so a type that is not the expected one is this error and the message says which two met. The situations:\n\
         \n\
         - A value of the wrong type where one is annotated or declared: `let n: int = 1.5`, a call argument against its parameter, a returned value against the function's `->` type, a field against its struct's.\n\
         - `int` and `float` in each other's place. The note offers the cast: `n as float`.\n\
         - `nil` for a type that cannot be absent. The note says to write `Option<T>` where a value may be missing.\n\
         - A field or method taken from a type that has none (`n.foo` on an `int`: `int has no fields`), and a `for` over something that is not a range or a slice.\n\
         \n\
         Operators on mismatched operands are `E0201`; a condition that is not a `bool` is `E0202`.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn half(n: float) -> float {\n\
         \x20       return n / 2.0\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       io.print(half(n))    // E0200: expected `float`, found `int`\n\
         \x20   }\n\
         \n\
         The fix is the explicit cast:\n\
         \n\
         \x20   fn half(n: float) -> float {\n\
         \x20       return n / 2.0\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       io.print(half(n as float))\n\
         \x20   }";

    E0201 = "E0201", "cannot apply operator to these types",
        "The operator has no meaning for these operand types. Kite has no operator overloading, so `+` is always numeric addition or string concatenation, never a call a type supplied, and the operands of a binary operator have the same type. The situations:\n\
         \n\
         - Two numbers of different types: `1 + 2.5`. There is no implicit conversion; the note says to write an explicit `as` cast.\n\
         - An operator the type does not have: `\"a\" - 1`, `-\"x\"` (`-` applies to `int` and `float`), `!1` (`!` applies to `bool`).\n\
         - An ordering between types that have none, or between different ones: `1 < \"x\"`.\n\
         - `&&` or `||` on something that is not a `bool`: Kite has no truthiness, so write the comparison.\n\
         \n\
         A cast between types that cannot convert (`1 as str`) is `E0212`; to put a number in a string, interpolate it: `\"\\(n)\"`.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let a = 1\n\
         \x20       let b = 2.5\n\
         \x20       io.print(a + b)    // E0201: `+` cannot be applied to `int` and `float`\n\
         \x20   }\n\
         \n\
         The fix is to say which type is meant:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let a = 1\n\
         \x20       let b = 2.5\n\
         \x20       io.print(a as float + b)\n\
         \x20   }";

    E0202 = "E0202", "condition must be `bool`",
        "Kite has no truthiness: nothing but a `bool` is true or false, so a number, a string, a slice or an optional is not a condition. The compiler would otherwise have to choose, and `0`, `\"\"`, `[]` and `nil` each mean something different in the languages people come from. The message says what was found. The conditions are:\n\
         \n\
         - the condition of an `if`, an `else if` or a `for` loop's `for cond {`;\n\
         - a `match` guard, `Pat if cond =>`;\n\
         - the first argument of `assert` and `require`, `assert(ok, \"message\")`.\n\
         \n\
         Write the comparison that was meant: `n != 0`, `s != \"\"`, `xs.len() > 0`, `o != nil`.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       if n {    // E0202: this is an `int`\n\
         \x20           io.print(1)\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         The fix is the comparison:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = 3\n\
         \x20       if n != 0 {\n\
         \x20           io.print(1)\n\
         \x20       }\n\
         \x20   }";

    E0203 = "E0203", "missing return value",
        "A function that declares a return type has to hand one back on every path through it, and `return` has to carry what the signature says. The situations:\n\
         \n\
         - A path that reaches the closing `}` without returning: an `if` with no `else` that returns, a `for` after which nothing is returned. A loop is not assumed to run, and a `match` has to return in every arm.\n\
         - A bare `return` in a function that declares a value: `return` alone in `-> int`.\n\
         - One value from a fallible function. A function declared `-> (T, error)` returns two: `return value, nil` on success and `return _, err` on failure (§7). `return 1` is this error, and the note says so.\n\
         \n\
         A function with no return type may fall off its end, and `return` alone leaves it early.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn sign(n: int) -> int {\n\
         \x20       if n > 0 {\n\
         \x20           return 1\n\
         \x20       }\n\
         \x20   }    // E0203: not every path returns a value\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(sign(1))\n\
         \x20   }\n\
         \n\
         The fix is a return for the path that was missing:\n\
         \n\
         \x20   fn sign(n: int) -> int {\n\
         \x20       if n > 0 {\n\
         \x20           return 1\n\
         \x20       }\n\
         \x20       return 0\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(sign(1))\n\
         \x20   }";

    E0204 = "E0204", "a type that cannot be named or worked out",
        "The compiler could not settle a type: either a name written where \
         a type goes names no type, or there is nothing to work a type out \
         from. The situations:\n\
         \n\
         - A type name that does not resolve: `let x: Foo = 1`, in an \
         annotation, a signature, a field or a struct literal `Foo{ … }`. \
         A close type that is in scope is offered in a note. `Self` \
         outside an `impl` or a trait is one. So is a qualified name \
         whose module was not imported: `json.Json` without `use \
         std/json`, and the note asks whether the module is imported. An \
         unknown name followed by type arguments, `Foo<int>`, is reported \
         as a name that takes none.\n\
         - A name that is the wrong sort of thing. A trait used as a \
         type, `fn f(x: Display)`, is refused, and `dyn Display` is the \
         way to name one. `dyn S` where `S` is a struct, `impl S for S` \
         and `impl Nope for S` name something that is not a trait. An \
         `impl` is for a struct or an enum declared in the same module, \
         so `impl int` is refused: wrap the built-in type in a struct of \
         your own and implement the methods on that.\n\
         - Nothing to infer from. `let x = nil`, `let xs = []`, `let m = \
         {}`, a `let x` with neither a type nor a value, and a `let (a, \
         b)` with nothing to take apart all leave the type open, and \
         inference never guesses. Annotate the binding.\n\
         - A host declaration that cannot be one. An `@host` `extern fn` \
         takes and returns `int`, `float`, `bool`, `str` or `JsValue`, so \
         `[int]` is refused, and it answers with one value, so `-> (int, \
         error)` is too: the Kite function that wraps the declaration is \
         the one that returns the `error`.\n\
         - A target that cannot hold the program. `kitec run --native` \
         refuses a program that uses `JsValue`, because a `JsValue` is a \
         reference the JavaScript engine owns and there is no engine \
         outside a browser. It is reported once, at the first function \
         that touches one, with a note counting the others; `--emit wasm` \
         is the target that has a host. The same code reports a struct, \
         variant, tuple, closure or host call made of more than 4096 \
         parts on the native target, which is a limit of the generated \
         code and not of the language; the bytecode target runs it.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let xs = []    // E0204: an empty slice has no elements to infer from\n\
         \x20       io.print(xs.len())\n\
         \x20   }\n\
         \n\
         The fix is the annotation:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let xs: [int] = []\n\
         \x20       io.print(xs.len())\n\
         \x20   }";

    E0205 = "E0205", "no such method, function, or callable value",
        "A name was called and there is nothing of that name to call: a method a type does not have, an associated function that is not one, or a value that is not a function at all. Kite has no extension methods, so a type's methods are all declared in the module that declares the type, in an `impl` block. That is what makes `x.foo()` answerable by looking in one place, and what makes the error here a statement about the type rather than about what happens to be imported. The situations:\n\
         \n\
         - A method the type does not have: `p.length()` on a struct with no `length`. The note lists what the type does have.\n\
         - `int`, `float` and `bool` have no methods at all (`(1).abs()` is this error); `str`, slices, maps and `error` have the few their note lists. The things to do with a number are functions in `std/math`.\n\
         - A `dyn Trait` exposes the methods of its trait and no others, however many the concrete type has, because the concrete type is not known where the call is written.\n\
         - An associated function called as a method (`p.make()`) or a method called as an associated function (`Point.length()`): the note says which of the two it is.\n\
         - A call whose callee is a value that is not a function: `let f = 3` and then `f(1)`.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 3, y: 4 }\n\
         \x20       io.print(p.length())    // E0205: `Point` has no method `length`\n\
         \x20   }\n\
         \n\
         The fix is to write the method, in an `impl` block of the type, or to spell the name the type has:\n\
         \n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   impl Point {\n\
         \x20       fn length(self) -> int {\n\
         \x20           return self.x * self.x + self.y * self.y\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 3, y: 4 }\n\
         \x20       io.print(p.length())\n\
         \x20   }";

    E0206 = "E0206", "trait cannot be a trait object",
        "A `dyn Trait` is a value whose type is known only by what it implements: the call `x.m()` looks at the value at run time and runs that type's `m`. For that to make sense whichever type it turns out to be, every method of the trait has to take `self` (a method without a receiver has nothing to dispatch on), must not be generic (a generic method has a body per type argument, not one body to call), and must not mention `Self` anywhere else (`Self` is a different type behind every `dyn`, so the call could not be checked). A trait that breaks one of those cannot be a trait object. The message names the method and the rule it breaks.\n\
         \n\
         Such a trait is still a good bound. In `fn f<T: Trait>(x: T)` the type is known where the function is written, so every method is callable, `Self` included; a bound costs a body per type, where a `dyn` costs a lookup per call. So the situations are the three above, written as `dyn Trait` in a parameter or a return type, or in a slice of them (`[dyn Trait]`).\n\
         \n\
         Example:\n\
         \n\
         \x20   trait Zero {\n\
         \x20       fn zero() -> int\n\
         \x20       fn show(self) -> str\n\
         \x20   }\n\
         \n\
         \x20   struct A {\n\
         \x20       n: int\n\
         \x20   }\n\
         \n\
         \x20   impl Zero for A {\n\
         \x20       fn zero() -> int {\n\
         \x20           return 0\n\
         \x20       }\n\
         \x20       fn show(self) -> str {\n\
         \x20           return \"a\"\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         \x20   fn describe(z: dyn Zero) -> str {    // E0206: `zero` has no `self`\n\
         \x20       return z.show()\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(describe(A{ n: 1 }))\n\
         \x20   }\n\
         \n\
         The fix is a type parameter in place of the trait object (or, if a `dyn` is what is needed, moving the methods that cannot dispatch into a trait of their own, so that the one the `dyn` names is only the methods that can):\n\
         \n\
         \x20   trait Zero {\n\
         \x20       fn zero() -> int\n\
         \x20       fn show(self) -> str\n\
         \x20   }\n\
         \n\
         \x20   struct A {\n\
         \x20       n: int\n\
         \x20   }\n\
         \n\
         \x20   impl Zero for A {\n\
         \x20       fn zero() -> int {\n\
         \x20           return 0\n\
         \x20       }\n\
         \x20       fn show(self) -> str {\n\
         \x20           return \"a\"\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         \x20   fn describe<T: Zero>(z: T) -> str {\n\
         \x20       return z.show()\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(describe(A{ n: 1 }))\n\
         \x20   }";

    E0207 = "E0207", "value cannot be interpolated",
        "String interpolation, `\"\\(x)\"`, renders `int`, `float`, `bool` and `str`. Any other type needs a `Display` implementation, which says how the type appears to a human. That is deliberately not derived: how a type presents itself is a design decision, not a mechanical one (a `Password` whose derived form printed its field is the case where being wrong matters). The situations:\n\
         \n\
         - A struct or an enum with no `impl Display`.\n\
         - A slice or an optional: `\"\\(xs)\"` has no text form either, because there is no one right way to join the pieces. Build the text (`join_ints(xs, \", \")`), or for an optional narrow it first or give it a fallback.\n\
         - An `error`, which cannot be given an `impl`: interpolate `err.message()` after testing it.\n\
         \n\
         `io.print(x)` takes the same types and anything with a `Display`; the interpolation is where a missing one is reported.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 1, y: 2 }\n\
         \x20       io.print(\"at \\(p)\")    // E0207: `Point` has no text form\n\
         \x20   }\n\
         \n\
         The fix is an `impl Display`, whose one method returns the text. Then `\"\\(p)\"` and `io.print(p)` both work:\n\
         \n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   impl Display for Point {\n\
         \x20       fn show(self) -> str {\n\
         \x20           return \"(\\(self.x), \\(self.y))\"\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 1, y: 2 }\n\
         \x20       io.print(\"at \\(p)\")\n\
         \x20   }";

    E0208 = "E0208", "invalid type parameter",
        "A type parameter stands for a type the caller chooses, and this code is for the places where what was written does not say one coherently. A parameter has to be named once and every bound has to name a trait; a generic type has to be given as many type arguments as it declares; and every argument has to meet its bounds. Without a bound nothing is known about a type, so nothing can be done to a value of it beyond moving it around; the bound is what makes anything possible. The situations:\n\
         \n\
         - A generic type written without its arguments, or with the wrong number: `Box` for `Box<int>`, or `Task<int, str>` (a `Task` takes one).\n\
         - A type argument that does not satisfy the bound: `total<T: Shape>` called with a slice of a type that has no `impl Shape`. The note says which trait it lacks.\n\
         - A type parameter declared twice in one list (`<T, T>`), or a bound that is not a trait.\n\
         - An `impl` that disagrees with its trait about the type's parameters or their bounds, or that is for one instance of a generic type (`impl Named for Box<int>`): an `impl` is for every `Box`, at its own type parameters.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Box<T> {\n\
         \x20       value: T\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let b: Box = Box{ value: 1 }    // E0208: `Box` needs 1 type argument\n\
         \x20       io.print(b.value)\n\
         \x20   }\n\
         \n\
         The fix is to say which `Box` (or to let a literal say it, `let b = Box{ value: 1 }`):\n\
         \n\
         \x20   struct Box<T> {\n\
         \x20       value: T\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let b: Box<int> = Box{ value: 1 }\n\
         \x20       io.print(b.value)\n\
         \x20   }";

    E0209 = "E0209", "type argument cannot be inferred",
        "A generic function's type arguments are worked out from the types of the arguments passed to it. When that cannot give each parameter one type, the call is refused rather than guessed, because a guess would pick a different type when someone later adds an argument. Kite has no turbofish (`make::<int>()`), so there is nowhere in a call to write the type; what fixes it is something the compiler can see. The situations:\n\
         \n\
         - A parameter that appears in no parameter type: `fn make<T>() -> Option<T>` called as `make()`. Nothing in the call says what `T` is.\n\
         - Two arguments that disagree about one parameter: `same(1, \"two\")` for `fn same<T>(a: T, b: T)`. The message is `conflicting types`.\n\
         - A generic enum's variant or a generic type's associated function with nothing to fix its parameter: `Maybe.None`, or `Stack.empty()`.\n\
         - A closure whose return type nothing fixes.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn make<T>() -> Option<T> {\n\
         \x20       return nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let x = make()    // E0209: nothing says what `T` is\n\
         \x20       io.print(x == nil)\n\
         \x20   }\n\
         \n\
         The fix is a type where the value lands. A binding with a type, `let x: Option<int> = make()`, tells the call what it has to give; or take a value of the type as an argument so the call can read it off, or return a concrete type:\n\
         \n\
         \x20   fn make<T>() -> Option<T> {\n\
         \x20       return nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let x: Option<int> = make()\n\
         \x20       io.print(x == nil)\n\
         \x20   }";

    E0210 = "E0210", "non-exhaustive match",
        "A `match` has to cover every value its subject can take, because exhaustiveness is what makes adding a variant safe: the compiler then shows every `match` that has to change. The message names a value that no arm covers. What counts as covering:\n\
         \n\
         - An enum: an arm for each variant, or a catch-all `_` or binding. The message names a variant that is missing.\n\
         - A `bool`: `true` and `false`.\n\
         - An `int`, a `float` or a `str`: literals can never cover them all, so an arm of `_` (or a name) is needed; the message says `_` is not covered.\n\
         - An optional: `nil` and an arm for a present value.\n\
         - A match with no arms at all, `match x { }`, which could never produce a value.\n\
         \n\
         A guard (`Pat if cond =>`) does not count towards covering, since it may be false.\n\
         \n\
         Example:\n\
         \n\
         \x20   enum Dir {\n\
         \x20       Up\n\
         \x20       Down\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let d = Dir.Up\n\
         \x20       match d {    // E0210: `Down` is not covered\n\
         \x20           Dir.Up => io.print(1)\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         The fix is the arm that was missing (or `_` if every other value is handled alike):\n\
         \n\
         \x20   enum Dir {\n\
         \x20       Up\n\
         \x20       Down\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let d = Dir.Up\n\
         \x20       match d {\n\
         \x20           Dir.Up => io.print(1)\n\
         \x20           Dir.Down => io.print(2)\n\
         \x20       }\n\
         \x20   }";

    E0211 = "E0211", "invalid closure",
        "A closure's parameter types come from where it is used. Where that is \
         not known, they must be annotated.\n\n\
         Captures are by value and taken when the closure is made, so a `var` \
         cannot be captured: later writes to it would not be seen, and code \
         reading it as if they were is a bug waiting to happen. Copy it into a \
         `let`, or pass it as a parameter. For the same reason a closure may \
         not assign to anything it captures: the write would land on its copy \
         and nowhere else.\n\n\
         A closure's body is a function of its own. Its `return` answers to the \
         closure's `-> T`, and where the body is an expression and nothing \
         states that type, a `return` inside it has nothing to be checked \
         against — write the type.";

    E0212 = "E0212", "invalid cast",
        "`as` converts between `int` and `float`, and between nothing else: not `bool`, not `str`, not an optional, not a struct. Kite performs no implicit numeric conversion, so a conversion is always written, which means every place precision can be lost is a place someone chose. A cast from one type to a type with no conversion is this error. The situations:\n\
         \n\
         - `bool as int` (`true as int`): write the choice, `if ok { 1 } else { 0 }`.\n\
         - `int as str` (`65 as str`): a number goes into a string by interpolation, `\"\\(n)\"`; and a string into a number by `parse_int` or `parse_float`, which answer `Option` because the text may not be a number.\n\
         - A cast to a type that takes arguments, `3 as Option<int>`: wrap the value in the optional where it is used.\n\
         \n\
         A cast that is allowed but loses something is the same code as a warning, not an error: `float as int` discards the fractional part (it truncates toward zero; a value too large for an `int` saturates, and `NaN` gives 0). The program compiles, and the warning is there so that the loss is something the reader has been told about.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let ok = true\n\
         \x20       io.print(ok as int)    // E0212: `as` converts `int` and `float` only\n\
         \x20   }\n\
         \n\
         The fix is to write what was meant:\n\
         \n\
         \x20   fn main() {\n\
         \x20       let ok = true\n\
         \x20       io.print(if ok { 1 } else { 0 })\n\
         \x20   }";

    E0213 = "E0213", "type has no identity",
        "`ptr.same` asks whether two names refer to one heap cell. Structs, \
         enums and maps have such a cell; nothing else does.\n\n\
         A number, a string or a `bool` is a value with no cell to share, so \
         the question has no answer other than the one `==` already gives.\n\n\
         A slice is excluded for a different reason: slices are \
         copy-on-write, so two of them sharing a buffer is a fact about the \
         allocator that stops being true as soon as either is written to. A \
         program that could observe it would be reading an implementation \
         detail.\n\n\
         Functions and `dyn` values are excluded because they have no stable \
         identity to report — which is the same reason `==` is not defined on \
         them.";

    E0214 = "E0214", "invalid type alias",
        "`type Name = T` gives `T` a second spelling. The two are \
         interchangeable everywhere, because the alias is replaced by what it \
         names before anything else is checked.\n\n\
         That replacement is the whole feature, and it is what the two \
         rejected forms cannot survive.\n\n\
         A circular alias names nothing: `type A = B` with `type B = A` has no \
         underlying type to be replaced by, only another alias.\n\n\
         A generic alias would need arguments substituted through it at every \
         use, which is a second instantiation path alongside the one structs \
         and enums already have. Kite has one. Write the generic type itself, \
         or a struct that wraps it.";

    E0220 = "E0220", "generic instantiation does not terminate",
        "Kite specialises generics: every set of type arguments a program uses \
         gets a copy of its own, made at compile time. A generic function that \
         calls itself at a larger type — `depth([x], n - 1)` inside \
         `depth<T>(x: T, …)` — or a generic type that contains itself at a \
         larger one — `inner: Option<Nested<[T]>>` inside `Nested<T>` — asks \
         for a new copy at every level, forever.\n\n\
         Languages that box their generics can run this, because a boxed \
         generic is one copy whatever the argument. Kite cannot, and says so \
         rather than giving up partway and emitting a program with calls into \
         copies that were never made.\n\n\
         Recurse at the same type, or hold the growing part in something whose \
         type does not grow — a slice of the original type, say, rather than a \
         nesting of slices.\n\n\
         The same code, in other words, reports a program that does finish but \
         asks for more than the compiler makes: a type argument nested more \
         than 256 levels deep or holding more than 65,536 parts, or more than \
         65,536 specialisations in all. The message says which, and only a \
         runaway is said to instantiate itself without end.";

    // ---- error handling ---------------------------------------------------
    E0301 = "E0301", "value used before its error was checked",
        "A function returning `(T, error)` returns a correlated pair. The \
         value is only meaningful when the error is nil, so reading it before \
         checking is rejected.\n\n\
         This is the flaw Kite fixes in Go's error convention. In Go the value \
         on a failure path is the zero value, which flows onward looking \
         valid. In Kite there is no value on that path at all.\n\n\
         Write `check err` to propagate, or test `err != nil` explicitly — in \
         the branch where the error is nil, the value becomes readable.";

    E0302 = "E0302", "error is never checked",
        "An `error` is a failure, and a failure has to be looked at. Kite keeps Go's `(T, error)` shape and takes away the habit that makes it unsafe: in Go a program can write `v, _ := f()`, or never test `err`, and carry on with a value from a call that failed. Here the compiler follows every error from where a call hands it over to where it is tested, passed on or returned, and an error that goes out of reach unlooked-at is this one. It is the most common error in the language, and it is almost always right. The situations:\n\
         \n\
         - A call written as a statement whose result is an `error` or a `(T, error)`: `save()` on a line of its own. The result is dropped on the spot.\n\
         - `let (v, err) = f()` and then `err` is never inspected. It is reported where `err` goes out of scope, which is also at each `return`, `break` or `continue` that leaves it behind: checking it on the path that falls through is not enough when an exit above the test takes another way.\n\
         - `_` in the error slot, `let (v, _) = f()`: the Go habit, spelled out.\n\
         - A whole call result bound to one name and never looked at: `let e = save()`. Naming it does not buy silence. Only `nil` starts out checked.\n\
         - Writing over an error that still holds a failure nobody looked at: `var e = f()` then `e = nil`. So does a loop, whose next pass writes over what this one left.\n\
         - Copying is not inspecting: after `let e = err` either name may be inspected, but one of them must be.\n\
         - The `Task` of an `async fn` that answers with an error, called and never awaited, returned or passed on; and a `defer` of a call that answers with one, which runs after the function has returned, so its error has nowhere to go. Defer a function that handles its own error instead.\n\
         - Projecting one element of a tuple no binding holds, `(f(1), f(2)).0` or `pair().0`: the other elements are thrown away with any failure in them, and nothing can take them later. Bind the tuple and take it apart.\n\
         \n\
         What counts as looking: testing it (`err != nil`, `err == nil`), calling `check err` in a function that returns an error, returning or passing it on, or handing it to something that is. Reading the value of a `(T, error)` before that is a different mistake, `E0301`.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn load() -> (int, error) {\n\
         \x20       return 1, nil\n\
         \x20   }\n\
         \n\
         \x20   fn save() -> error {\n\
         \x20       return nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       save()                  // E0302: the error is dropped\n\
         \x20       let (n, err) = load()   // E0302: `err` is never looked at\n\
         \x20       io.print(\"done\")\n\
         \x20   }\n\
         \n\
         The fix is to deal with each one. In a function that can return an error, `check` passes it on; in `main`, which has nobody to return one to, test it and say what to do. `_ = f()` is the one deliberate way to throw an error away, written so that a reader sees it and `grep` finds it:\n\
         \n\
         \x20   fn load() -> (int, error) {\n\
         \x20       return 1, nil\n\
         \x20   }\n\
         \n\
         \x20   fn save() -> error {\n\
         \x20       return nil\n\
         \x20   }\n\
         \n\
         \x20   fn run() -> error {\n\
         \x20       check save()\n\
         \x20       let (n, err) = load()\n\
         \x20       check err\n\
         \x20       io.print(n)\n\
         \x20       return nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let err = run()\n\
         \x20       if err != nil {\n\
         \x20           io.print(err.message())\n\
         \x20           return\n\
         \x20       }\n\
         \x20       _ = save()\n\
         \x20   }";

    E0303 = "E0303", "`check` outside a fallible function",
        "`check err` is the return of an error: if `err` is not nil, the enclosing function returns it. So the function around it has to be able to — its return type is `(T, error)` or `error`. The situations:\n\
         \n\
         - `check` in `main`, which cannot return an error to anyone. This is the error met first when writing a scratch program.\n\
         - `check` in a function with no error in its return type.\n\
         - `check` inside a closure. A closure's `return` leaves the closure, and a closure has no error to give back to the function around it, so it is refused even in a fallible function.\n\
         \n\
         In `main`, test the error and say what to do about it. In another function, either declare the error and let callers decide, or handle it here.\n\
         \n\
         Example:\n\
         \n\
         \x20   fn read() -> (int, error) {\n\
         \x20       return 1, nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let (n, err) = read()\n\
         \x20       check err    // E0303: `main` returns nothing, so there is nowhere to send `err`\n\
         \x20       io.print(n)\n\
         \x20   }\n\
         \n\
         The fix, in `main`, is to deal with it:\n\
         \n\
         \x20   fn read() -> (int, error) {\n\
         \x20       return 1, nil\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let (n, err) = read()\n\
         \x20       if err != nil {\n\
         \x20           io.print(err.message())\n\
         \x20           return\n\
         \x20       }\n\
         \x20       io.print(n)\n\
         \x20   }";

    // ---- modules ----------------------------------------------------------
    E0400 = "E0400", "module not found",
        "A `use` names a module that cannot be found. The note under the \
         error lists every place that was looked in.\n\
         \n\
         A module is a file or a directory (§13.1). A path is looked for \
         beside the module that writes the `use`, then in each directory \
         enclosing that one, stopping at the root of the package the module \
         belongs to, and the nearest match wins. A first segment that names \
         one of the package's declared dependencies roots in that \
         dependency instead. Every segment counts, so `use shapes/circle` \
         is the `circle` module inside `shapes`, never the `circle` beside \
         it. The situations:\n\
         \n\
         - A path with nothing at it: `use shape` when the file is \
         `shapes.kite`, or a file in another directory.\n\
         - A path that matches a name on disk only when case is ignored: \
         `use Shapes` beside `shapes.kite`. A `use` matches a file or \
         directory's name exactly, case included, on every system, so a \
         program that compiles on macOS does not fail on Linux; the note \
         names the file that is there.\n\
         - A directory that is not a module. A directory is a module when \
         a `.kite` file is directly in it; one that holds only other \
         directories is found, refused, and said to be refused in the \
         note.\n\
         - A standard-library module that does not exist. The path is \
         exactly `std/<name>`, so `use std/nothing` and `use \
         std/json/extra` are refused, and the note lists the modules \
         there are. `io` is built in and reached without a `use`, so `use \
         std/io` is refused too.\n\
         - A dependency that is declared but not in place. A `git` \
         dependency is read from `.kite/vendor`, which `kitec pkg` fills, \
         so a program that has not run it cannot find the dependency. \
         When `kite.toml` itself does not read (`E0405`) nothing it \
         declares can be found, so that is the one error: a `use` \
         of the package it belongs to is not reported as well.\n\
         - A file that was found and could not be read; the operating \
         system's reason is in the message.\n\
         \n\
         Example:\n\
         \n\
         \x20   use shape    // E0400: the file is shapes.kite\n\
         \n\
         The fix is the name that is there:\n\
         \n\
         \x20   use shapes";

    E0401 = "E0401", "private item",
        "Something is used outside its module that its module did not mark \
         `pub`. `pub` is the only visibility modifier (§4.3): an unmarked \
         declaration is visible within the module that declares it, and \
         `pub` makes it visible to anything that imports the module. A \
         module is a directory, so every file in it shares what is \
         unmarked. The situations:\n\
         \n\
         - A function, constant, type or trait: `m.hidden()` where \
         `hidden` is not `pub`. A type that is not `pub` cannot be named \
         outside its module at all, not in an expression, a signature, a \
         field, an annotation or an `impl` header, and a trait that is \
         not `pub` cannot be implemented outside its module. A `pub` \
         alias may not name a type that is not `pub`, since an alias is \
         the type it names.\n\
         - A variant of an enum that is not `pub`. A variant has no \
         visibility of its own: it is exactly as visible as its enum, so \
         `m.Dir.Up` and a bare `Up` in a pattern are both refused while \
         `Dir` is private, and both are fine once it is `pub`.\n\
         - A field. `p.n` where `n` is not marked `pub` is refused, and \
         so is building the struct with a literal, `m.P{ n: 1, k: 2 }`, \
         even one that starts from `..base`, since a literal has to give \
         every field. A `pub struct` with unmarked fields is an opaque \
         type: callers can hold it and pass it on, and its module exports \
         the function that builds one. Nor can a pattern take it apart, \
         even one naming only its `pub` fields: `m.P{ k: k }` is refused, \
         and `p.k` reads the field.\n\
         - A method or an associated function. Those in an `impl` block \
         are private unless they say `pub`: `s.secret()` and \
         `m.S.assoc()` are refused. The methods of a trait implementation \
         are as visible as the trait, so `s.go()` is refused while the \
         trait that declares `go` is not `pub`.\n\
         \n\
         Example, with `m` declaring `fn hidden() -> int { return 1 }`:\n\
         \n\
         \x20   use m\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(m.hidden())    // E0401: `hidden` is not `pub`\n\
         \x20   }\n\
         \n\
         The fix is `pub fn hidden()` in `m`, or a `pub` function in `m` \
         that does what is needed and keeps `hidden` to itself.";

    E0403 = "E0403", "module name is reserved by the standard library",
        "A use site spells a module by the last segment of its `use` path, so \
         `use crypto` naming a sibling would be spelled `crypto` in the file \
         that imported it — and shadow `std/crypto` there. It was worse when a \
         module was *known* by that segment: a dependency shipping a `crypto` \
         directory, imported anywhere before the first `use std/crypto`, took \
         over every `crypto.hash` call in the program with no diagnostic.\n\n\
         So the standard library's names belong to it, and so does `prelude`, \
         whose declarations are in scope everywhere without a `use`: a module \
         of that name became every module's unqualified fallback. So do `io`, \
         `draw` and `ptr`, which its builtins are reached through.\n\n\
         The name after `as` is a spelling too. `use util as errors` was \
         accepted, and `errors` is in scope in every file without a `use`, so \
         one spelling reached two modules: `errors.new` stayed the standard \
         library's while every other `errors.…` reached `util`. A `std` module \
         spelled as itself — `use std/json as json` — is the one spelling \
         that is its own.\n\n\
         Rename the module, or spell it another way with `use … as …`.";

    E0404 = "E0404", "two modules of the same name",
        "A module is identified by where its source is, and a use site writes \
         a *spelling* — the last segment of the path, or the name after `as`. \
         A spelling belongs to the file that writes it, so two files may spell \
         different modules alike; what one file may not do is spell two \
         modules alike, because every `utils.…` above the second `use` would \
         quietly change meaning. Give one of them a name of its own with \
         `use … as …`.\n\n\
         The same code reports two packages of one name reached from two \
         different places. A package's name means one thing across the whole \
         program — `kitec pkg` refuses the same manifests for the same \
         reason — so the manifests naming it must agree on where it is.\n\n\
         And it reports one file reached as two modules. Every `.kite` file \
         in a module directory is part of that module, so `use shape` written \
         inside `lib/`, or `use lib/shape` beside `use lib`, would compile \
         `shape.kite` a second time as a module of its own, and each of its \
         types would become two. Inside `lib`, its declarations are already \
         in scope unqualified; elsewhere, write `lib.…`.\n\n\
         Last, one module reached as part of two packages. A module's own \
         `use`s are looked for within its package, so a dependency vendored \
         inside the program — reachable both by the name its manifest \
         declares and by a path from the program — would resolve its imports \
         one way or the other depending on which `use` came first. Reach it \
         one way only.";

    E0405 = "E0405", "a `kite.toml` that does not read",
        "The manifest, `kite.toml`, says where a package's dependencies are, so a build reads it before it can find any of them. One that does not read is reported at the line where it stops, and is the one error: the `use`s it leaves unresolvable are not reported as well (they used to be, as `cannot find module` at every `use` of a dependency, which is a diagnostic about the wrong file). The reader is a deliberate subset of TOML (tables, inline tables, quoted strings), and what is refused is named in the message. The situations:\n\
         \n\
         - Something that is not a quoted string where a string is wanted: `version = 1.0`. Values are `\"…\"` or `'…'`; there are no numbers or arrays.\n\
         - A key or a table written twice, and a trailing comma in an inline table.\n\
         - A key the section does not take. `[package]` takes `name` and `version`; a dependency takes `path`, `git`, `tag` and `version`, so a misspelt `verison` is an error rather than a setting dropped without a word.\n\
         - A dependency without exactly one of `path` and `git`, a `tag` without a `git`, or a `tag` together with a `version`.\n\
         - A name that cannot be imported: package and dependency names are ASCII letters, digits and `_`, not starting with a digit, because the name is what a `use` writes. `my-app` is refused.\n\
         - A `version` that is not a version (`1.2.0`, without leading zeros), a target without an `entry`, and a file that is not UTF-8 text.\n\
         \n\
         Example, a manifest (everything after `version =` must be a quoted string):\n\
         \n\
         \x20   [package]\n\
         \x20   name = \"app\"\n\
         \x20   version = 1.0    # E0405: expected a quoted string\n\
         \n\
         The fix is to quote it:\n\
         \n\
         \x20   [package]\n\
         \x20   name = \"app\"\n\
         \x20   version = \"1.0.0\"";

    E0402 = "E0402", "module cycle",
        "Modules import each other in a circle, directly or through others. \
         Cyclic modules make separate compilation, incremental rebuilds and \
         initialisation order all harder (§13.3), so a cycle is an error \
         and not something the compiler picks an order for.\n\
         \n\
         The note prints the chain, such as `b → a`. The error sits on the \
         `use` that closes the cycle, except when the cycle leads back to \
         the entry file: then it sits on the entry's own `use` that began \
         it, so that checking the entry alone, which is what an editor \
         does, shows it. A module that imports itself — a file of `ui/` \
         writing `use ui`, or a file naming itself — is a cycle of one, \
         and the message says it imports itself: delete that `use`, since \
         what a module declares is already in scope in its own files.\n\
         \n\
         Example, `a.kite` and `b.kite` in one directory, checking `a.kite`:\n\
         \n\
         \x20   // a.kite\n\
         \x20   use b    // E0402: the chain is b → a\n\
         \n\
         \x20   // b.kite\n\
         \x20   use a    // the import that closes it\n\
         \n\
         Every cycle can be broken by extracting what both modules need \
         into a third module that imports neither, and having both import \
         that:\n\
         \n\
         \x20   // shared.kite holds what a and b both use\n\
         \x20   // a.kite:  use shared        b.kite:  use shared\n\
         \n\
         A cycle among packages — a `kite.toml` whose dependencies lead \
         back to it — is this code too, at the program's manifest, naming \
         the chain of packages (`app -> a -> b -> app`) as `kitec pkg` \
         does when it refuses the same graph.";

    E0406 = "E0406", "implementation outside its type's module",
        "An `impl` block belongs to the module that declares its type — or, \
         for a trait implementation, to the module that declares the trait.\n\n\
         There are no extension methods (§8.2). Every method a type has is \
         declared where the type is, so `x.foo()` is answered by looking in one \
         place, and nobody else's module can add to a type or read its private \
         fields from outside.\n\n\
         A trait is implemented for a type at most once, by one of the two \
         modules that own them (§10.2, the orphan rule). Were a third module \
         allowed, two of them could each write the same `impl`, and which one \
         a call reached would depend on which happened to be loaded.\n\n\
         To give another module's type new behaviour, write a function that \
         takes it, or declare a trait of your own and implement that for it \
         here.";

    // ---- concurrency ------------------------------------------------------
    E0520 = "E0520", "type cannot be moved to another task",
        "A value whose type is not `Share` is being handed to something \
         that requires `Share`. `Share` is the marker for a value that may \
         be moved to another task, and it is inferred, never written: a \
         type is `Share` when it is a primitive or a `str`, when it is a \
         struct or an enum all of whose fields are `Share` and none of \
         which is `var`, when it is a slice, map or tuple of `Share` \
         elements, or when it is wrapped in `sync.Mutex<T>` or \
         `sync.Atomic` (§12.3). It is not `Share` when a `var` field is \
         anywhere in its structure, because two tasks holding one mutable \
         value is a data race. It is not `Share` when it holds a `JsValue`, \
         because a host reference belongs to the isolate that created it, \
         and no change to the type makes it travel. And a function or a \
         `dyn Trait` is not `Share`, because what it holds is not known \
         where the type is.\n\
         \n\
         The places that ask are a `T: Share` bound, which is how \
         `task.parallel` asks for its items and its results, and any bound \
         written the same way. `task.parallel` is not parallel on any \
         target yet; the rule is enforced now so that the same source \
         starts using cores when a reference can cross a thread. The \
         situations:\n\
         \n\
         - A type that fails the rule is passed: `task.parallel(counters, \
         |c| c.count)` where `Counter` has a `var` field. The error \
         points at the argument, at the bound that requires `Share`, and \
         at the field that breaks it.\n\
         - A closure that captures such a value: it goes to the other \
         task with everything it captured, so `|x| x + k.count` is \
         refused where `k` is a `Counter`. The error says which capture.\n\
         - A function whose captures cannot be seen. A closure held in a \
         binding or returned from a call, `let f = |x: int| x + 1` and \
         then `task.parallel(xs, f)`, may carry anything, so it is \
         refused. Write the closure where it is handed over, or name a \
         function.\n\
         - A type parameter that is not known to be `Share`. In `fn \
         f<T>(x: T)` that calls a function bounded by `T: Share`, `T` \
         could be any type, so it is refused until `f` says `T: Share` as \
         well.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Counter {\n\
         \x20       var count: int\n\
         \x20   }\n\
         \n\
         \x20   fn need<T: Share>(x: T) -> int { return 1 }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(need(Counter{ count: 0 }))    // E0520: `count` is `var`\n\
         \x20   }\n\
         \n\
         The fix is to stop mutating, so that a new value is returned in \
         place of the old one, and drop the `var`; or to wrap the counter \
         in `sync.Mutex<Counter>`; or to send the task what it needs, a \
         number or a string, and keep the counter where it is.";

    E0521 = "E0521", "`await` outside an async function",
        "`await`, `task.yield`, `task.park` and `task.wait_host` suspend \
         the function they are written in, so that function has to be \
         declared `async`; an ordinary `fn` cannot be suspended, and in one \
         they would do nothing. The situations:\n\
         \n\
         - `await`, or one of those three builtins, in a function that is \
         not `async`. That includes `main`, which is `async fn main()` \
         when it awaits.\n\
         - `await`, or one of those builtins, inside a closure. A closure is a \
         function of its own and is never `async`, so one inside an \
         `async fn` is refused too: `|| await work()`. Await before \
         making the closure, or have the closure return the task and \
         await it where it is called; `task.yield` goes in the enclosing \
         `async fn`, before or after the closure runs.\n\
         \n\
         Example:\n\
         \n\
         \x20   async fn work() -> int { return 1 }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let n = await work()    // E0521: `main` is not `async`\n\
         \x20   }\n\
         \n\
         The fix is to declare the function that waits as `async`:\n\
         \n\
         \x20   async fn main() {\n\
         \x20       let n = await work()\n\
         \x20   }";

    // ---- cryptography -----------------------------------------------------
    E0600 = "E0600", "comparing a secret with `==`",
        "This is a warning. Structural equality (`==` on a `str`) stops at the first byte that differs, so how long a comparison takes says how much of a guess was right. That is a timing oracle, and it is how a token, a signature or a digest is guessed one character at a time by someone who can time the replies.\n\
         \n\
         `crypto.equal(a, b)` compares in time that does not depend on where the values first differ, and is what to write wherever one side is a secret. The compiler looks only at the shape people write: a comparison with `==` or `!=` that has a call into `std/crypto` as one of its operands. It does not follow a value through a binding, a field or a function, since a check that stopped partway would give confidence it had not earned. So binding the result to a `let` first hides the warning and not the problem.\n\
         \n\
         Example:\n\
         \n\
         \x20   use std/crypto\n\
         \n\
         \x20   fn main() {\n\
         \x20       let given = \"6f3a\"\n\
         \x20       if crypto.hex_of(\"secret\") == given {    // E0600: stops at the first difference\n\
         \x20           io.print(\"ok\")\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         The fix is the constant-time comparison:\n\
         \n\
         \x20   use std/crypto\n\
         \n\
         \x20   fn main() {\n\
         \x20       let given = \"6f3a\"\n\
         \x20       if crypto.equal(crypto.hex_of(\"secret\"), given) {\n\
         \x20           io.print(\"ok\")\n\
         \x20       }\n\
         \x20   }";

    // ---- derivation -------------------------------------------------------
    E0700 = "E0700", "malformed `@derive`",
        "`@derive(…)` names traits whose bodies the compiler writes from a type's fields (`Debug`, `Hash`, `Encode`, `Decode`), so it goes in front of a `struct` or an `enum` and names at least one trait. A trait it cannot write is `E0701`. The situations:\n\
         \n\
         - `@derive` in front of something that is not a type: a function, an `impl` or a constant. A derived body is written from fields, so there has to be a type.\n\
         - `@derive()` with nothing in the parentheses.\n\
         \n\
         Example:\n\
         \n\
         \x20   @derive(Debug)    // E0700: not a type declaration\n\
         \x20   fn f() {\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       f()\n\
         \x20   }\n\
         \n\
         The fix is to put it on the type it is for:\n\
         \n\
         \x20   @derive(Debug)\n\
         \x20   struct P {\n\
         \x20       x: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20   }";

    E0701 = "E0701", "nothing derives that",
        "`@derive(…)` has the compiler write a trait's methods from a type's fields. It writes `Debug`, `Hash`, `Encode` and `Decode`, and nothing else; a trait name it cannot write is this error, at the name. The situations:\n\
         \n\
         - `Display`. It is absent on purpose: a mechanical rendering is wrong more often than right, and a `Password` whose derived form printed its field is exactly the case where being wrong matters. Write `impl Display` for the type by hand.\n\
         - `Eq` and its relatives. `==` is already structural on every Kite value, so a derived `Eq` would be a second spelling for what the language does anyway. `Clone` and `Ord` are refused the same way: the compiler writes only the four.\n\
         - A trait of your own, or any other trait: the compiler knows no body to write for it.\n\
         - A derived trait that the type also implements by hand (`@derive(Debug)` beside a written `fn debug`): the same method twice.\n\
         \n\
         Example:\n\
         \n\
         \x20   @derive(Debug, Display)    // E0701: the compiler writes no `Display`\n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 1, y: 2 }\n\
         \x20       io.print(p.x)\n\
         \x20   }\n\
         \n\
         The fix is to derive what can be derived and write the rest:\n\
         \n\
         \x20   @derive(Debug)\n\
         \x20   struct Point {\n\
         \x20       x: int\n\
         \x20       y: int\n\
         \x20   }\n\
         \n\
         \x20   impl Display for Point {\n\
         \x20       fn show(self) -> str {\n\
         \x20           return \"(\\(self.x), \\(self.y))\"\n\
         \x20       }\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       let p = Point{ x: 1, y: 2 }\n\
         \x20       io.print(p)\n\
         \x20   }";

    E0702 = "E0702", "a field the derive cannot write",
        "A derived body is a walk over a type's fields, so every field has to be something the walk knows how to handle: a primitive, a slice, a map, an optional, a tuple, another type that derives the same trait, or (for `Debug` and `Hash`) one of the type's own parameters, which the derived `impl` then bounds by the trait. The message names the field it stopped at. The situations:\n\
         \n\
         - A field whose type has no implementation of that trait: a struct or an enum of your own with neither the `@derive` nor an `impl` written by hand.\n\
         - A function value (`fn(int) -> int`, which has nothing to show but its type), a `dyn Trait` (the concrete type is hidden, and a walk needs it) and an `error`.\n\
         - `Encode` or `Decode` on a generic type. Decoding builds a value of a parameter through an associated function that no bound can reach, so those two derive only for a type with no parameters (`Debug` and `Hash` derive for either).\n\
         \n\
         Where the derive cannot write a body, write the implementation by hand: the derive is a convenience, not the only way in.\n\
         \n\
         Example:\n\
         \n\
         \x20   struct Handle {\n\
         \x20       id: int\n\
         \x20   }\n\
         \n\
         \x20   @derive(Debug)\n\
         \x20   struct Job {\n\
         \x20       name: str\n\
         \x20       handle: Handle    // E0702: `Handle` does not derive `Debug`\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(1)\n\
         \x20   }\n\
         \n\
         The fix is the derive on the field's type too:\n\
         \n\
         \x20   @derive(Debug)\n\
         \x20   struct Handle {\n\
         \x20       id: int\n\
         \x20   }\n\
         \n\
         \x20   @derive(Debug)\n\
         \x20   struct Job {\n\
         \x20       name: str\n\
         \x20       handle: Handle\n\
         \x20   }\n\
         \n\
         \x20   fn main() {\n\
         \x20       io.print(1)\n\
         \x20   }";

    // ---- exclusivity ------------------------------------------------------
    E0800 = "E0800", "one object under two argument names",
        "A struct is a GC reference and is always passed by reference, so a \
         `var` parameter is a handle the callee writes through. When one call \
         passes the same object to two parameters and either of them is \
         `var`, each write lands where the other name expects its own value:\n\n\
         \x20   transfer(a, a, 50)\n\n\
         takes 50 from the balance and puts it back. Nothing traps, nothing is \
         unsafe — Kite collects, so the memory is real either way — and the \
         program is silently wrong.\n\n\
         Two arguments name the same object when one path is a prefix of the \
         other, so `f(o, o.inner)` counts as well as `f(a, a)`. A slice, map, \
         tuple or optional of structs counts too: it is copied on write, but \
         shallowly, so two copies hold the same structs. A generic parameter \
         is judged by the argument passed to it. A call through a function \
         value is checked when the call or a `let` says which function it is; \
         through any other, it is not, because a function type does not say \
         which parameters are `var`. \
         Pass distinct objects, or take one `var` parameter and return the \
         second result.\n\n\
         This sees one call site. Aliasing arranged elsewhere — two fields \
         holding one reference — is not detected, and needs no detection to be \
         memory-safe; Kite has no borrowing and no lifetimes, and this rule is \
         not the beginning of either.";

    // ---- the compiler failing ---------------------------------------------
    E0900 = "E0900", "the compiler emitted an invalid module",
        "This is a bug in Kite, not in the program being compiled. Codegen \
         produced a WebAssembly module that the engine would refuse to load, \
         and the compiler checked its own output before handing it over.\n\n\
         The check exists because the alternative is worse. An invalid module \
         is not rejected when it is built; it is rejected when a browser tries \
         to instantiate it, which is a blank page and a `CompileError` in a \
         console, a long way from the function that caused it. Failing here \
         names the offending function and costs microseconds.\n\n\
         There is no source change that is the right fix. Please report it, \
         with the program if it can be shared: the validator's message \
         identifies the function, which is usually enough to find the bad \
         lowering.";

    E0901 = "E0901", "internal compiler error",
        "This is a bug in Kite, not in the program being compiled. A stage of \
         the compiler found something an earlier stage promised it would never \
         see — a `break` with no loop to leave, an `await` the state-machine \
         transform did not remove, a value the checker left without a type \
         and reported nothing about — and stopped rather than guess.\n\n\
         Guessing is what it used to do. The `break` was dropped and the \
         program ran on as if it had not been written; the `await` crashed \
         the compiler. Stopping with this names the function, which is \
         usually enough to find the stage at fault.\n\n\
         There is no source change that is the right fix, though rewriting the \
         construct the note points at may avoid it. Please report it, with \
         the program if it can be shared.";

    E0902 = "E0902", "the program exceeds a limit of this target",
        "The program is valid Kite, and the target it is being compiled for \
         cannot represent it.\n\n\
         The bytecode VM addresses a frame's registers with sixteen bits, so \
         one function may use at most 65,536 of them: one per local and \
         temporary, plus the widest call it makes. A function past that — \
         tens of thousands of locals — used to be emitted anyway, with \
         register numbers silently cut short, so that unrelated values shared \
         a register.\n\n\
         Split the function.\n\n\
         WebAssembly has a limit of its own: an engine accepts at most 50,000 \
         locals in one function, parameters included, and every local and \
         temporary is one, and at most 7,654,321 bytes of code. A long slice \
         or map literal is no trouble on any target: past a short window, each \
         element is added to what has been built, and on WebAssembly each \
         run of up to 4,096 constants is built by a function of its own, so no \
         one function grows with the literal's constants. An element that is \
         not a constant, such as a struct literal, or a string in a \
         `[Option<str>]`, which is wrapped first, is a statement of the function the literal is in, \
         and V8 may fail to compile a function of a hundred thousand of \
         those below either limit; split it. The native target has neither \
         limit.";
}
