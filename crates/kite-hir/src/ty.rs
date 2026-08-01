//! The type representation.
//!
//! Phase 1 covers the primitives the vertical slice needs. Structs, enums,
//! slices, maps, optionals, traits, and generics arrive in Phase 2, which is
//! why this is an enum with room to grow rather than an interned index.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    /// The type of a function with no declared return, and of statements.
    Unit,
    Bool,
    /// 64-bit signed. The default integer.
    Int,
    /// 64-bit IEEE-754. The default float.
    Float,
    /// Immutable UTF-8 string. On the Wasm target this becomes a JS string
    /// reference; on native and bytecode it is a GC-managed byte array.
    Str,
    /// The type of an expression that never produces a value: `return`,
    /// `break`, `continue`. Coerces to anything, which is what lets
    /// `if c { return } else { 1 }` type check.
    Never,
    /// Poison. Produced where an error was already reported, and compatible
    /// with everything so one mistake yields one diagnostic.
    Error,
}

impl Ty {
    pub fn name(self) -> &'static str {
        match self {
            Ty::Unit => "()",
            Ty::Bool => "bool",
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Str => "str",
            Ty::Never => "!",
            Ty::Error => "<error>",
        }
    }

    /// The type's name with its indefinite article, for prose in diagnostics:
    /// "this is an `int`", not "this is a `int`".
    pub fn with_article(self) -> String {
        let article = match self {
            Ty::Int | Ty::Error => "an",
            _ => "a",
        };
        format!("{} `{}`", article, self.name())
    }

    /// Whether a value of type `self` is acceptable where `expected` is
    /// required.
    ///
    /// Kite performs no implicit numeric conversion, so this is equality plus
    /// two escape hatches: `Never` satisfies anything (it never produces a
    /// value), and `Error` satisfies anything (to stop cascades).
    pub fn satisfies(self, expected: Ty) -> bool {
        self == expected
            || self == Ty::Never
            || self == Ty::Error
            || expected == Ty::Error
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }

    /// Whether `==` and `!=` are defined on this type.
    pub fn is_equatable(self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Bool | Ty::Str)
    }

    /// Whether `<`, `<=`, `>`, `>=` are defined on this type.
    pub fn is_ordered(self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Str)
    }

    /// Whether `io.print` accepts this type.
    pub fn is_printable(self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Bool | Ty::Str)
    }

    /// Whether a diagnostic should be suppressed because this type is already
    /// the result of a reported error.
    pub fn is_poisoned(self) -> bool {
        matches!(self, Ty::Error | Ty::Never)
    }

    /// Resolve a surface type name.
    pub fn from_name(name: &str) -> Option<Ty> {
        Some(match name {
            "bool" => Ty::Bool,
            "int" => Ty::Int,
            "float" => Ty::Float,
            "str" => Ty::Str,
            _ => return None,
        })
    }

    /// Names suggested when an unknown type is written. Phase 2 replaces this
    /// with a proper edit-distance search over all types in scope.
    pub const PRIMITIVE_NAMES: [&'static str; 4] = ["bool", "int", "float", "str"];
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_satisfies_everything() {
        for t in [Ty::Unit, Ty::Bool, Ty::Int, Ty::Float, Ty::Str] {
            assert!(Ty::Never.satisfies(t), "! must satisfy {}", t);
        }
    }

    #[test]
    fn error_is_compatible_in_both_directions() {
        assert!(Ty::Error.satisfies(Ty::Int));
        assert!(Ty::Int.satisfies(Ty::Error));
    }

    #[test]
    fn no_implicit_numeric_conversion() {
        assert!(!Ty::Int.satisfies(Ty::Float));
        assert!(!Ty::Float.satisfies(Ty::Int));
    }

    #[test]
    fn bool_is_equatable_but_not_ordered() {
        assert!(Ty::Bool.is_equatable());
        assert!(!Ty::Bool.is_ordered());
    }

    #[test]
    fn primitive_names_all_resolve() {
        for n in Ty::PRIMITIVE_NAMES {
            assert!(Ty::from_name(n).is_some(), "{} does not resolve", n);
        }
    }
}
