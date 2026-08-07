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

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Code(pub &'static str);

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
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
        "A numeric literal is malformed. Digit separators may appear between \
         digits but not at either end, and a float must have digits on both \
         sides of the point.\n\n\
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
        "Kite has line comments (//) and doc comments (///) only. Nested block \
         comments are a recurring source of lexer bugs and every editor has \
         supported toggling line comments for decades.";

    // ---- syntax and bindings ---------------------------------------------
    E0100 = "E0100", "unexpected token",
        "The parser found a token that cannot appear in this position.";

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
         same reason; this is the same rule applied to the front end.";

    E0110 = "E0110", "use of possibly-uninitialised binding",
        "A `let` binding may be assigned after declaration, but only if the \
         compiler can prove exactly one assignment happens on every path \
         before the first read.";

    E0111 = "E0111", "unknown name",
        "This identifier does not resolve to anything in scope.";

    E0112 = "E0112", "duplicate definition",
        "Two items in the same scope share a name. Shadowing is permitted in a \
         nested scope, but not in the same one, where it is almost always a \
         typo.";

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
        "This statement follows one that always diverges, so it can never run.";

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
         continues.";

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

    E0204 = "E0204", "unknown type",
        "This type name does not resolve.";

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
        "A `dyn Trait` dispatches by looking at the value it holds, so every \
         method must take `self`. A method without a receiver has nothing to \
         dispatch on.\n\n\
         Either give the method a `self` parameter, or accept the concrete \
         type instead of the trait object.";

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
         `let`, or pass it as a parameter.";

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
        "An `error` binding went out of scope without being inspected. Silently \
         dropping errors is the single most common source of production \
         failures in languages that permit it.\n\n\
         To propagate, write `check`. To handle it where it happened, test \
         `err != nil`.";

    E0303 = "E0303", "`check` outside a fallible function",
        "`check` returns the error to the caller, so the enclosing function \
         must declare `(T, error)` as its return type.";

    // ---- modules ----------------------------------------------------------
    E0400 = "E0400", "module not found",
        "The import path does not resolve to a module.";

    E0401 = "E0401", "private item",
        "This item is not marked `pub`, so it is visible only within its own \
         module.";

    E0403 = "E0403", "module name is reserved by the standard library",
        "A module is known by the last segment of its `use` path, so `use \
         std/crypto` and `use crypto` both name a module called `crypto` — and \
         whichever was loaded first won, silently, for the whole program.\n\n\
         That made the standard library replaceable by any module that got \
         there first: a dependency shipping a `crypto` directory, imported \
         anywhere before the first `use std/crypto`, took over every \
         `crypto.hash` call in the program with no diagnostic. Since `std` is \
         not part of a module's identity, nothing afterwards could tell the \
         two apart.\n\n\
         So the standard library's names belong to it. Rename the module.";

    E0404 = "E0404", "two modules of the same name",
        "A module is known by the last segment of its `use` path, so two \
         modules in different directories with the same final name are one \
         module as far as the rest of the compiler is concerned — and the one \
         loaded first won, for the whole program.\n\n\
         Which one that is depends on the order of the `use` lines in the \
         entry file, and nothing was reported either way. A dependency \
         shipping a `utils` directory could therefore answer every \
         `utils.…` call in the importing program's own source, with no \
         diagnostic and nothing changed in that program. `E0403` reserves the \
         standard library's names for the same reason; this is the general \
         case.\n\n\
         Rename one of them. Full paths as identities — so `dep/utils` and \
         `utils` are two modules rather than a collision — is the better \
         answer and is not what this compiler does yet.";

    E0402 = "E0402", "module cycle",
        "Modules may not depend on each other cyclically. Extract the shared \
         part into a third module.";

    // ---- concurrency ------------------------------------------------------
    E0520 = "E0520", "type cannot be moved to another task",
        "Only `Share` values may cross a task boundary. A type is `Share` when \
         it is deeply immutable, or explicitly synchronised.\n\n\
         Because struct fields are immutable by default, most types satisfy \
         this without the author doing anything. A `var` field anywhere in the \
         transitive structure disqualifies the type, because two tasks holding \
         the same mutable value is a data race.";

    E0521 = "E0521", "`await` outside an async function",
        "`await` suspends the enclosing function, so that function must be \
         declared `async`.";

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
         map, an optional, a tuple, or another type that derives the same \
         trait.\n\n\
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
         other, so `f(o, o.inner)` counts as well as `f(a, a)`. Pass distinct \
         objects, or take one `var` parameter and return the second result.\n\n\
         This sees one call site. Aliasing arranged elsewhere — two fields \
         holding one reference — is not detected, and needs no detection to be \
         memory-safe; Kite has no borrowing and no lifetimes, and this rule is \
         not the beginning of either.";
}
