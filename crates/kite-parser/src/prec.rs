//! Operator precedence.
//!
//! Encoded as (left, right) binding powers for a Pratt loop. Left-associative
//! operators use `(n, n + 1)`; right-associative use `(n + 1, n)`.
//!
//! Two departures from C, both deliberate and both documented in the
//! specification:
//!
//! * Bitwise operators bind **tighter** than comparison, so `a & b == c` means
//!   `(a & b) == c` — what everyone intends and what C gets wrong.
//! * Comparison is **non-associative**: `a < b < c` is rejected by the parser
//!   rather than silently comparing a bool against an int.

use kite_ast::BinaryOp;
use kite_lexer::TokenKind as T;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InfixOp {
    Binary(BinaryOp),
    Range { inclusive: bool },
    Cast,
}

impl InfixOp {
    pub fn from_token(k: T) -> Option<InfixOp> {
        use BinaryOp as B;
        Some(match k {
            T::DotDot => InfixOp::Range { inclusive: false },
            T::DotDotEq => InfixOp::Range { inclusive: true },
            T::As => InfixOp::Cast,
            T::QuestionQuestion => InfixOp::Binary(B::Coalesce),
            T::PipePipe => InfixOp::Binary(B::Or),
            T::AmpAmp => InfixOp::Binary(B::And),
            T::EqEq => InfixOp::Binary(B::Eq),
            T::Ne => InfixOp::Binary(B::Ne),
            T::Lt => InfixOp::Binary(B::Lt),
            T::Le => InfixOp::Binary(B::Le),
            T::Gt => InfixOp::Binary(B::Gt),
            T::Ge => InfixOp::Binary(B::Ge),
            T::Amp => InfixOp::Binary(B::BitAnd),
            T::Caret => InfixOp::Binary(B::BitXor),
            T::Pipe => InfixOp::Binary(B::BitOr),
            T::Shl => InfixOp::Binary(B::Shl),
            T::Shr => InfixOp::Binary(B::Shr),
            T::Plus => InfixOp::Binary(B::Add),
            T::Minus => InfixOp::Binary(B::Sub),
            T::Star => InfixOp::Binary(B::Mul),
            T::Slash => InfixOp::Binary(B::Div),
            T::Percent => InfixOp::Binary(B::Rem),
            _ => return None,
        })
    }
}

pub fn infix_binding_power(op: InfixOp) -> (u8, u8) {
    use BinaryOp as B;
    match op {
        // Loosest. `0..n + 1` is `0..(n + 1)`.
        InfixOp::Range { .. } => (2, 3),
        // Right-associative: `a ?? b ?? c` is `a ?? (b ?? c)`.
        InfixOp::Binary(B::Coalesce) => (5, 4),
        InfixOp::Binary(B::Or) => (6, 7),
        InfixOp::Binary(B::And) => (8, 9),
        // Non-associative; the parser reports chaining rather than encoding it
        // in the binding powers, so the message can explain the fix.
        InfixOp::Binary(o) if o.is_comparison() => (10, 11),
        InfixOp::Binary(B::BitOr) => (12, 13),
        InfixOp::Binary(B::BitXor) => (14, 15),
        InfixOp::Binary(B::BitAnd) => (16, 17),
        InfixOp::Binary(B::Shl) | InfixOp::Binary(B::Shr) => (18, 19),
        InfixOp::Binary(B::Add) | InfixOp::Binary(B::Sub) => (20, 21),
        InfixOp::Binary(B::Mul) | InfixOp::Binary(B::Div) | InfixOp::Binary(B::Rem) => (22, 23),
        // Tighter than any binary operator, looser than prefix and postfix.
        InfixOp::Cast => (24, 25),
        InfixOp::Binary(_) => unreachable!("every BinaryOp has a binding power"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitwise_binds_tighter_than_comparison() {
        let (and_l, _) = infix_binding_power(InfixOp::Binary(BinaryOp::BitAnd));
        let (eq_l, _) = infix_binding_power(InfixOp::Binary(BinaryOp::Eq));
        assert!(and_l > eq_l, "a & b == c must group as (a & b) == c");
    }

    #[test]
    fn arithmetic_binds_tighter_than_comparison() {
        let (add_l, _) = infix_binding_power(InfixOp::Binary(BinaryOp::Add));
        let (lt_l, _) = infix_binding_power(InfixOp::Binary(BinaryOp::Lt));
        assert!(add_l > lt_l);
    }

    #[test]
    fn coalesce_is_right_associative() {
        let (l, r) = infix_binding_power(InfixOp::Binary(BinaryOp::Coalesce));
        assert!(r < l, "?? must be right-associative");
    }

    #[test]
    fn range_is_loosest() {
        let (range_l, _) = infix_binding_power(InfixOp::Range { inclusive: false });
        for op in [BinaryOp::Or, BinaryOp::Add, BinaryOp::Eq, BinaryOp::Coalesce] {
            let (l, _) = infix_binding_power(InfixOp::Binary(op));
            assert!(range_l < l, "{:?} must bind tighter than `..`", op);
        }
    }
}
