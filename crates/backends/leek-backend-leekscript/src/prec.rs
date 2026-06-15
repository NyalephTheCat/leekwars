//! Operator precedence, mirroring the parser's Pratt binding powers
//! (`crates/frontend/leek-parser/src/grammar/expr/mod.rs`).
//!
//! The HIR has dropped the source parentheses, so the emitter must
//! re-insert them to preserve the parse. We model each operator with a
//! single `prec` level (higher binds tighter) plus associativity, then
//! thread a minimum-precedence threshold through expression emission.
//!
//! A node placed in a context requiring threshold `min` is parenthesized
//! iff its own precedence is `< min`. The thresholds passed to children
//! are:
//!
//! - left child:  `prec`     if left-assoc, else `prec + 1`
//! - right child: `prec + 1` if left-assoc, else `prec`
//!
//! This reproduces the parser exactly: e.g. `a - (b - c)` keeps its
//! parens (subtraction is left-assoc), and `2 ** 3 ** 2` does not (power
//! is right-assoc), while `(2 ** 3) ** 2` regains them.

use leek_hir::BinaryOp;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Assoc {
    Left,
    Right,
}

// Precedence ladder, loosest to tightest. Atoms sit above everything.
pub(crate) const PREC_ASSIGN: u8 = 1;
pub(crate) const PREC_TERNARY: u8 = 2;
pub(crate) const PREC_PREFIX: u8 = 14;
pub(crate) const PREC_POSTFIX: u8 = 15;
pub(crate) const PREC_ATOM: u8 = 100;

/// Binding info for a binary operator: `(prec, assoc)`.
pub(crate) fn binary(op: BinaryOp) -> (u8, Assoc) {
    use BinaryOp as B;
    match op {
        // Assignment family — right-associative, loosest.
        B::Assign
        | B::AddAssign
        | B::SubAssign
        | B::MulAssign
        | B::DivAssign
        | B::IntDivAssign
        | B::ModAssign
        | B::PowAssign
        | B::BitAndAssign
        | B::BitOrAssign
        | B::BitXorAssign
        | B::ShiftLAssign
        | B::ShiftRAssign
        | B::UShiftRAssign
        | B::NullCoalesceAssign => (PREC_ASSIGN, Assoc::Right),

        B::Or | B::NullCoalesce => (3, Assoc::Left),
        B::And | B::Xor => (4, Assoc::Left),
        B::BitOr => (5, Assoc::Left),
        B::BitXor => (6, Assoc::Left),
        B::BitAnd => (7, Assoc::Left),
        B::Eq | B::Ne | B::IdentityEq | B::IdentityNe | B::Is => (8, Assoc::Left),
        B::Lt | B::Le | B::Gt | B::Ge | B::In | B::NotIn | B::Instanceof => (9, Assoc::Left),
        B::ShiftL | B::ShiftR | B::UShiftR => (10, Assoc::Left),
        B::Add | B::Sub => (11, Assoc::Left),
        B::Mul | B::Div | B::Mod | B::IntDiv => (12, Assoc::Left),
        B::Pow => (13, Assoc::Right),
    }
}

/// Threshold for the left operand of a binary operator.
pub(crate) fn left_min(prec: u8, assoc: Assoc) -> u8 {
    match assoc {
        Assoc::Left => prec,
        Assoc::Right => prec + 1,
    }
}

/// Threshold for the right operand of a binary operator.
pub(crate) fn right_min(prec: u8, assoc: Assoc) -> u8 {
    match assoc {
        Assoc::Left => prec + 1,
        Assoc::Right => prec,
    }
}

/// Source spelling of a binary operator.
pub(crate) fn binary_str(op: BinaryOp) -> &'static str {
    use BinaryOp as B;
    match op {
        B::Add => "+",
        B::Sub => "-",
        B::Mul => "*",
        B::Div => "/",
        B::Mod => "%",
        B::IntDiv => "\\",
        B::Pow => "**",
        B::Eq => "==",
        B::Ne => "!=",
        B::IdentityEq => "===",
        B::IdentityNe => "!==",
        B::Lt => "<",
        B::Le => "<=",
        B::Gt => ">",
        B::Ge => ">=",
        B::And => "&&",
        B::Or => "||",
        B::Xor => "xor",
        B::BitAnd => "&",
        B::BitOr => "|",
        B::BitXor => "^",
        B::ShiftL => "<<",
        B::ShiftR => ">>",
        B::UShiftR => ">>>",
        B::NullCoalesce => "??",
        B::In => "in",
        B::NotIn => "not in",
        B::Is => "is",
        B::Instanceof => "instanceof",
        B::Assign => "=",
        B::AddAssign => "+=",
        B::SubAssign => "-=",
        B::MulAssign => "*=",
        B::DivAssign => "/=",
        B::IntDivAssign => "\\=",
        B::ModAssign => "%=",
        B::PowAssign => "**=",
        B::BitAndAssign => "&=",
        B::BitOrAssign => "|=",
        B::BitXorAssign => "^=",
        B::ShiftLAssign => "<<=",
        B::ShiftRAssign => ">>=",
        B::UShiftRAssign => ">>>=",
        B::NullCoalesceAssign => "??=",
    }
}
