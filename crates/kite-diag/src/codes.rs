//! Stable diagnostic codes.
//!
//! A code is part of the language's public surface: it appears in output, in
//! `kite --explain`, and in test annotations. Codes are never reused for a
//! different meaning, even if a rule is removed.
//!
//! Ranges:
//!   E0000–E0099  lexical
//!   E0100–E0199  syntax and bindings
//!   E0200–E0299  types, traits, patterns
//!   E0300–E0399  error handling (taint analysis)
//!   E0400–E0499  modules and visibility
//!   E0500–E0599  concurrency and Share

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
         sides of the point.";

    E0005 = "E0005", "block comments are not supported",
        "Kite has line comments (//) and doc comments (///) only. Nested block \
         comments are a recurring source of lexer bugs and every editor has \
         supported toggling line comments for decades.";

    // ---- syntax and bindings ---------------------------------------------
    E0100 = "E0100", "unexpected token",
        "The parser found a token that cannot appear in this position.";

    E0101 = "E0101", "unclosed delimiter",
        "A bracket, brace, or parenthesis was opened and never closed.";

    E0102 = "E0102", "expected a statement",
        "This position requires a statement. Statements are newline-terminated \
         in Kite; semicolons are never written.";

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

    E0205 = "E0205", "not callable",
        "This expression is not a function.";

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

    E0210 = "E0210", "non-exhaustive match",
        "A `match` must cover every possible value. Exhaustiveness is what \
         makes adding an enum variant safe: the compiler shows you every place \
         that must change.";

    // ---- error handling ---------------------------------------------------
    E0301 = "E0301", "value used before its error was checked",
        "A function returning `(T, error)` returns a correlated pair. The \
         value is only meaningful when the error is nil, so reading it before \
         checking is rejected.\n\n\
         This is the flaw Kite fixes in Go's error convention. In Go the value \
         on a failure path is the zero value, which flows onward looking \
         valid. In Kite there is no value on that path at all.\n\n\
         Write `check err` to propagate, or test `err != nil` explicitly — in the          branch where the error is nil, the value becomes readable.";

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
}
