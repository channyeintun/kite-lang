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
        "A string literal was opened but never closed. Kite strings may not \
         span lines unless written as a triple-quoted block string.";

    E0002 = "E0002", "invalid character in source",
        "The lexer found a byte that cannot begin any Kite token.";

    E0003 = "E0003", "invalid escape sequence",
        "Recognised escapes are \\n \\t \\r \\0 \\\\ \\\" \\' and \\u{...}.";

    E0004 = "E0004", "invalid number literal",
        "A numeric literal is malformed. A digit separator `_` sits between two \
         digits, in every radix and every part of a literal — `1_000`, \
         `0xFF_FF`, `1.5e1_0` — so it cannot end the digits, follow a radix \
         prefix, double up, or touch a `.` or an exponent: `1_`, `0x_F`, \
         `1__0` and `1_.5` are all refused. A float must have digits on both \
         sides of the point.\n\n\
         A literal too large for its type is refused too: an `int` above \
         9223372036854775807, and a `float` too large to be finite, such as \
         `1e999`, which would otherwise become infinity without a word.\n\n\
         A type suffix — `42i32`, `2.5f32` — is also refused. Kite has one \
         integer type and one float, so a suffix names nothing. It used to be \
         consumed and thrown away, which made `300i8` read as a width the \
         compiler was checking when there was no `i8` for the value to \
         overflow.";

    E0006 = "E0006", "string interpolation nested too deeply",
        "A string may hold an interpolation, which may hold a string, which \
         may hold another interpolation. The scanner follows that by calling \
         itself, so the nesting in the file is stack depth in the compiler, \
         and three bytes of source buy a level.\n\n\
         Running out of stack aborts the process rather than reporting \
         anything — which would mean a file that kills the language server the \
         moment it is opened — so there is a ceiling instead. No program \
         written on purpose comes near it.";

    E0005 = "E0005", "block comments are not supported",
        "Kite's comments are line comments: `//` for a comment, `///` for the \
         documentation of the declaration that follows, and `//!` for the \
         module's own, at the top of its file. There is no block comment. \
         Nested block comments are a recurring source of lexer bugs and every \
         editor has supported toggling line comments for decades.";

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
        "A bracket, brace, or parenthesis was opened and never closed.";

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
        "A `let` binding may be assigned after declaration, but only if the \
         compiler can prove exactly one assignment happens on every path \
         before the first read.";

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
        "Kite has no default arguments, no variadics, and no overloading, so \
         the argument count must match the declaration exactly. A function \
         needing many optional inputs should take a struct.";

    E0114 = "E0114", "cannot assign to immutable binding",
        "Bindings introduced with `let` are immutable. Use `var` for a binding \
         that must change.\n\n\
         Immutability is the default in Kite because it removes the \
         value-versus-pointer distinction, maps onto WasmGC's per-field \
         mutability flag, and makes most types automatically shareable across \
         threads.";

    E0115 = "E0115", "`break` or `continue` outside a loop",
        "These statements are only meaningful inside a `for` loop.";

    E0116 = "E0116", "unreachable code",
        "This statement follows one that always diverges, so it can never run.\n\n\
         The same holds for a `match` arm whose every value is taken by the arms \
         above it. The commonest cause is a name meant as a variant that is not \
         one — `Dir` where the enum says `Directory` — which is a binding: it \
         matches everything, and every arm after it is dead. A guarded arm \
         shadows nothing, because its guard may fail. A literal or a range \
         inside a range above it (`5` after `1..=9`) is dead the same way, \
         and so is a range that matches nothing: ends the wrong way round, \
         `9..=4`, or `5..5`, which leaves out its one value.";

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
        "A constant's value is computed from the constants it names, so a \
         cycle among them has no value to compute. The chain is listed in the \
         order it was followed, ending where it returned to where it began.";

    // ---- types ------------------------------------------------------------
    E0200 = "E0200", "type mismatch",
        "An expression's type does not match what this position requires. Kite \
         performs no implicit numeric conversion: write an explicit `as` cast.";

    E0201 = "E0201", "cannot apply operator to these types",
        "The operand types have no such operation. Kite has no operator \
         overloading, so `+` is always numeric addition or string \
         concatenation, never a user-defined call.";

    E0202 = "E0202", "condition must be `bool`",
        "Kite has no truthiness. A condition must be exactly `bool`; compare \
         explicitly, for example `if n != 0`.";

    E0203 = "E0203", "missing return value",
        "This function declares a return type but a path through it returns \
         nothing.";

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
        "A name was called and there is nothing of that name to call here: a \
         method a type does not have, an associated function that is not one, \
         or an expression that is not a function at all.\n\n\
         Kite has no extension methods, so a type's methods are all declared in \
         the module that declares the type — which is what makes `x.foo()` \
         answerable by looking in one place. A `dyn Trait` is narrower still: it \
         exposes its trait's methods and no others, because the concrete type is \
         not known where the call is written.";

    E0206 = "E0206", "trait cannot be a trait object",
        "A `dyn Trait` dispatches by looking at the value it holds, and the \
         call has to make sense whichever type that turns out to be. So every \
         method must take `self` — a method without a receiver has nothing to \
         dispatch on — must not be generic, because a generic method has a body \
         per type argument rather than one to call, and must not mention \
         `Self`, which is a different type behind every `dyn`.\n\n\
         Such a trait is still a perfectly good bound: `fn f<T: Trait>(x: T)` \
         knows the type, so every method is callable. Take a type parameter \
         instead of the trait object, or move what a `dyn` needs into a trait \
         of its own.";

    E0207 = "E0207", "value cannot be interpolated",
        "String interpolation renders `int`, `float`, `bool` and `str`. Any \
         other type needs a `Display` implementation, which says how it should \
         appear to a human.\n\n\
         That is deliberately not derived: how a type presents itself is a \
         design decision, not a mechanical one.";

    E0208 = "E0208", "invalid type parameter",
        "A type parameter stands for a type the caller chooses. Each must be \
         named once, and each bound must name a trait.\n\n\
         A bound is what makes anything possible with the parameter: without \
         one, nothing is known about the type, so nothing can be done to a \
         value of it beyond moving it around.";

    E0209 = "E0209", "type argument cannot be inferred",
        "A generic function's type arguments are worked out from the types of \
         the arguments passed to it. A parameter that appears nowhere in the \
         parameter list has nothing to infer from.\n\n\
         Kite has no turbofish. If a parameter cannot be inferred, take a \
         value of that type, or return a concrete type instead.";

    E0210 = "E0210", "non-exhaustive match",
        "A `match` must cover every possible value. Exhaustiveness is what \
         makes adding an enum variant safe: the compiler shows you every place \
         that must change.";

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
        "`as` converts between `int` and `float`. There is no conversion \
         between any other pair of types.\n\n\
         Kite performs no implicit numeric conversion, so a conversion is \
         always written — which means every place precision can be lost is a \
         place someone chose.";

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
        "An `error` binding went out of scope without being inspected, or was \
         written over while it still held a failure nobody had looked at: \
         `var e = f()` then `e = nil` drops `f`'s failure as surely as leaving \
         the block does. Copying it is not inspecting it: after `let e = err` \
         either name may be inspected, but one of them must be. The `Task` \
         of an `async fn` that answers with an error is held to the same \
         rule: it has to be awaited, returned or passed on. A call whose \
         result holds a failure cannot be deferred: it runs after the \
         function has returned, so its error has nowhere to go — defer a \
         function that handles it instead. Silently \
         dropping errors is the single most common source of production \
         failures in languages that permit it.\n\n\
         To propagate, write `check`. To handle it where it happened, test \
         `err != nil`.";

    E0303 = "E0303", "`check` outside a fallible function",
        "`check` returns the error to the caller, so the enclosing function \
         must declare `(T, error)` as its return type.";

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
         declares can be found, and the note says so.\n\
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
         the function that builds one.\n\
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
         in scope unqualified; elsewhere, write `lib.…`.";

    E0405 = "E0405", "a `kite.toml` that does not read",
        "The manifest is what says where a package's dependencies are, so a \
         build reads it before it can find any of them. One that does not \
         parse used to be treated as no manifest at all, and the error a \
         typo in `[package]` produced was `cannot find module` at every `use` \
         of a dependency — a diagnostic about the wrong file. The manifest is \
         reported instead, at the line that did not read.";

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
         does, shows it. A module that imports itself is a cycle of one.\n\
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
         A cycle among packages in `kite.toml` is not this code: `kitec \
         pkg` refuses it and names the chain of packages.";

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
        "Structural equality short-circuits at the first difference, so how \
         long a comparison takes says how much of a guess was right. That is a \
         timing oracle, and it is how tokens and signatures are guessed one \
         character at a time.\n\n\
         `crypto.equal` compares in time that does not depend on the values.";

    // ---- derivation -------------------------------------------------------
    E0700 = "E0700", "malformed `@derive`",
        "`@derive(…)` names traits to write bodies for, and goes in front of a \
         `struct` or an `enum`. A derived body is written from a type's \
         fields, so there has to be a type and there has to be something to \
         derive.";

    E0701 = "E0701", "nothing derives that",
        "The compiler writes bodies for `Debug`, `Hash`, `Encode` and \
         `Decode`, and for nothing else.\n\n\
         `Display` is deliberately absent: a mechanical rendering is wrong \
         more often than right, and a `Password` whose derived form printed \
         its field is exactly the case where being wrong matters. `Eq` is \
         absent because `==` is already structural on every Kite value, so a \
         derived one would be a second spelling for what the language does \
         anyway.";

    E0702 = "E0702", "a field the derive cannot write",
        "A derived body is a walk over a type's fields, so every field has to \
         be something the walk knows how to handle: a primitive, a slice, a \
         map, an optional, a tuple, another type that derives the same \
         trait, or — for `Debug` and `Hash` — one of the type's own \
         parameters, which the derived `impl` then bounds by the trait. \
         `Encode` and `Decode` do not derive for a generic type at all.\n\n\
         Where it does not, write the implementation by hand — the derive is a \
         convenience, not the only way in.";

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
         transform did not remove — and stopped rather than guess.\n\n\
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
         temporary is one. A long slice or map literal is no trouble on any \
         target: past a short window, each element is added to what has been \
         built. The native target has neither limit.";
}
