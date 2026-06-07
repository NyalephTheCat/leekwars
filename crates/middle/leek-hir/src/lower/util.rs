//! AST/token helpers shared by HIR lowering.

use leek_parser::ast::{self, AstNode};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use leek_types::Type;

use crate::ir::{BinaryOp, PostfixOp, UnaryOp};

pub(crate) fn first_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

pub(crate) fn first_ident_after(node: &SyntaxNode, after: SyntaxKind) -> Option<SyntaxToken> {
    let mut seen = false;
    for el in node.children_with_tokens() {
        let Some(t) = el.into_token() else { continue };
        if !seen {
            if t.kind() == after {
                seen = true;
            }
            continue;
        }
        if t.kind() == SyntaxKind::Ident {
            return Some(t);
        }
    }
    None
}

pub(crate) fn field_name(f: &ast::ClassField) -> Option<SyntaxToken> {
    let mut last_ident: Option<SyntaxToken> = None;
    for el in f.syntax().children_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            if t.kind() == SyntaxKind::Ident {
                last_ident = Some(t);
            } else if matches!(t.kind(), SyntaxKind::Eq | SyntaxKind::Semicolon) {
                break;
            }
        }
    }
    last_ident
}

pub(crate) fn method_name(m: &ast::ClassMethod) -> Option<SyntaxToken> {
    let mut last_ident: Option<SyntaxToken> = None;
    for el in m.syntax().children_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            if t.kind() == SyntaxKind::Ident {
                last_ident = Some(t);
            } else if t.kind() == SyntaxKind::LParen {
                break;
            }
        }
    }
    last_ident
}

pub(crate) fn fn_return_type(node: &SyntaxNode) -> Option<Type> {
    // Function return type comes after `=>` / `->` in the param list
    // — find the first TypeRef *after* the ParamList.
    let mut past_params = false;
    for child in node.children_with_tokens() {
        match &child {
            rowan::NodeOrToken::Node(n) => {
                if past_params && n.kind() == SyntaxKind::TypeRef {
                    return Some(leek_types::type_from_node(n));
                }
                if n.kind() == SyntaxKind::ParamList {
                    past_params = true;
                }
            }
            rowan::NodeOrToken::Token(_) => {}
        }
    }
    None
}

pub(crate) fn collect_modifiers(member: &SyntaxNode) -> Vec<&'static str> {
    member
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .filter_map(|t| match t.kind() {
            SyntaxKind::KwPublic => Some("public"),
            SyntaxKind::KwPrivate => Some("private"),
            SyntaxKind::KwProtected => Some("protected"),
            SyntaxKind::KwFinal => Some("final"),
            SyntaxKind::KwStatic => Some("static"),
            SyntaxKind::Ident => match t.text() {
                "private" => Some("private"),
                "protected" => Some("protected"),
                "public" => Some("public"),
                "final" => Some("final"),
                "static" => Some("static"),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

pub(crate) fn include_path(inc: &ast::IncludeStmt) -> Option<String> {
    inc.syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)
        .map(|t| strip_string_quotes_and_unescape(t.text()))
}

pub(crate) fn import_path(imp: &ast::ImportStmt) -> Option<String> {
    if let Some(s) = imp
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::StringLiteral)
    {
        return Some(strip_string_quotes_and_unescape(s.text()));
    }

    let mut parts: Vec<String> = Vec::new();
    for t in imp
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
    {
        match t.kind() {
            SyntaxKind::Ident => parts.push(t.text().to_string()),
            SyntaxKind::KwImport
            | SyntaxKind::Dot
            | SyntaxKind::LParen
            | SyntaxKind::RParen
            | SyntaxKind::Semicolon => {}
            k if k.is_trivia() => {}
            _ => break,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

pub(crate) fn interval_brackets(node: &SyntaxNode) -> (bool, bool) {
    let tokens: Vec<_> = node
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .collect();
    let open = tokens.first().map(rowan::SyntaxToken::kind);
    let close = tokens.last().map(rowan::SyntaxToken::kind);
    let start_inclusive = matches!(open, Some(SyntaxKind::LBracket));
    let end_inclusive = matches!(close, Some(SyntaxKind::RBracket));
    (start_inclusive, end_inclusive)
}

/// Parse the body of a C99-style hex float literal (without the
/// leading `0x`). Example inputs: `1.p53`, `a.bcdp-42`,
/// `ff` (no fractional / exponent → plain hex int as float).
/// Returns `0.0` for malformed input rather than erroring — the
/// lexer already flags unparseable hex literals.
// Parsing a hex float into an `f64` is inherently lossy for large mantissas.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn parse_hex_float(body: &str) -> f64 {
    // Split off the binary exponent (`p`/`P`).
    let (mantissa, exp_str) = match body.find(['p', 'P']) {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, "0"),
    };
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    let int_val: u64 = u64::from_str_radix(int_part, 16).unwrap_or(0);
    let mut value = int_val as f64;
    for (i, c) in frac_part.chars().enumerate() {
        let digit = f64::from(c.to_digit(16).unwrap_or(0));
        value += digit * 16f64.powi(-(i32::try_from(i).unwrap_or(i32::MAX) + 1));
    }
    value * 2f64.powi(exp)
}

pub(crate) fn strip_string_quotes_and_unescape(text: &str) -> String {
    // Default (and historical) behavior — modern dialect, unescape
    // every standard sequence.
    strip_string_quotes_and_unescape_versioned(text, 4)
}

pub(crate) fn strip_string_quotes_and_unescape_versioned(text: &str, version: u8) -> String {
    // Shared codec — see `leek_text::unescape`. The v1 quote quirk
    // (`length("abc\"def") == 8` at v1, pinned by
    // `TestString::testString_lexerEdgeCases::7@v1`) lives there too,
    // selected by `EscapeMode`.
    leek_text::unescape(text, leek_text::EscapeMode::from_version(version))
}

pub(crate) fn binary_op_from_token(k: SyntaxKind) -> Option<BinaryOp> {
    use BinaryOp::{
        Add, AddAssign, And, Assign, BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor,
        BitXorAssign, Div, DivAssign, Eq, Ge, Gt, IdentityEq, IdentityNe, In, Instanceof, IntDiv,
        IntDivAssign, Le, Lt, Mod, ModAssign, Mul, MulAssign, Ne, NullCoalesce, NullCoalesceAssign,
        Or, Pow, PowAssign, ShiftL, ShiftLAssign, ShiftR, ShiftRAssign, Sub, SubAssign, UShiftR,
        UShiftRAssign, Xor,
    };
    Some(match k {
        SyntaxKind::Plus => Add,
        SyntaxKind::Minus => Sub,
        SyntaxKind::Star => Mul,
        SyntaxKind::Slash => Div,
        SyntaxKind::Percent => Mod,
        SyntaxKind::Backslash => IntDiv,
        SyntaxKind::StarStar => Pow,
        SyntaxKind::EqEq | SyntaxKind::KwIs => Eq,
        SyntaxKind::NotEq => Ne,
        SyntaxKind::EqEqEq => IdentityEq,
        SyntaxKind::NotEqEq => IdentityNe,
        SyntaxKind::Lt => Lt,
        SyntaxKind::Le => Le,
        SyntaxKind::Gt => Gt,
        SyntaxKind::Ge => Ge,
        SyntaxKind::AmpAmp | SyntaxKind::KwAnd => And,
        SyntaxKind::PipePipe | SyntaxKind::KwOr => Or,
        SyntaxKind::KwXor => Xor,
        SyntaxKind::Amp => BitAnd,
        SyntaxKind::Pipe => BitOr,
        // `^` is *version-specific*: bitwise XOR in v1, power in
        // v2+. We can't pick at HIR-lower time because the version
        // isn't threaded here yet; emit `BitXor` and let the
        // interpreter dispatch on its `version` field. The MIR
        // representation stays version-neutral.
        SyntaxKind::Caret => BitXor,
        SyntaxKind::ShiftLeft => ShiftL,
        SyntaxKind::ShiftRight => ShiftR,
        SyntaxKind::UShiftRight => UShiftR,
        SyntaxKind::QuestionQuestion => NullCoalesce,
        SyntaxKind::KwIn => In,
        SyntaxKind::KwInstanceof => Instanceof,
        SyntaxKind::Eq => Assign,
        SyntaxKind::PlusEq => AddAssign,
        SyntaxKind::MinusEq => SubAssign,
        SyntaxKind::StarEq => MulAssign,
        SyntaxKind::SlashEq => DivAssign,
        SyntaxKind::BackslashEq => IntDivAssign,
        SyntaxKind::PercentEq => ModAssign,
        SyntaxKind::StarStarEq => PowAssign,
        SyntaxKind::AmpEq => BitAndAssign,
        SyntaxKind::PipeEq => BitOrAssign,
        // `^=` is the assignment form of `^` — also version-
        // dispatched at runtime (see the comment on `Caret`).
        SyntaxKind::CaretEq => BitXorAssign,
        SyntaxKind::ShiftLeftEq => ShiftLAssign,
        SyntaxKind::ShiftRightEq => ShiftRAssign,
        SyntaxKind::UShiftRightEq => UShiftRAssign,
        SyntaxKind::QuestionQuestionEq => NullCoalesceAssign,
        _ => return None,
    })
}

pub(crate) fn unary_op_from_token(k: SyntaxKind) -> Option<UnaryOp> {
    Some(match k {
        SyntaxKind::Minus => UnaryOp::Neg,
        SyntaxKind::Plus => UnaryOp::Pos,
        SyntaxKind::Bang | SyntaxKind::KwNot => UnaryOp::Not,
        SyntaxKind::Tilde => UnaryOp::BitNot,
        SyntaxKind::PlusPlus => UnaryOp::PreInc,
        SyntaxKind::MinusMinus => UnaryOp::PreDec,
        SyntaxKind::At => UnaryOp::Ref,
        _ => return None,
    })
}

pub(crate) fn postfix_op_from_token(k: SyntaxKind) -> Option<PostfixOp> {
    Some(match k {
        SyntaxKind::PlusPlus => PostfixOp::PostInc,
        SyntaxKind::MinusMinus => PostfixOp::PostDec,
        SyntaxKind::Bang => PostfixOp::NonNull,
        _ => return None,
    })
}
