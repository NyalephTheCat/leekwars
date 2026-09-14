//! Expression productions, Pratt-style.
//!
//! Binding powers follow `doc/grammar.md` §7. We use a left/right
//! binding-power pair per binary operator: `(l_bp, r_bp)` with
//! `l_bp == r_bp - 1` for left-associative ops and
//! `l_bp == r_bp + 1` for right-associative ops. The Pratt loop in
//! [`expr_bp`] consumes operators whose `l_bp` ≥ a caller-supplied
//! `min_bp`.
//!
//! Submodules:
//! - [`atom`] — primary atom dispatch (literals, names, prefixes,
//!   collection openers).
//! - [`lambda`] — the five lambda shapes plus the look-aheads that
//!   disambiguate `(` between lambda head, inner-arrow lambda, and
//!   paren expression.
//! - [`collection`] — array/map/set/object literals (`[…]`, `{…}`,
//!   `<…>` forms).
//! - [`interval`] — bracket-delimited interval literals.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

mod atom;
mod collection;
mod interval;
mod lambda;

use atom::atom_or_prefix;
use interval::{has_matching_rbracket, looks_like_interval_open};
use lambda::looks_like_ternary_after_type;

/// Parse an expression at the lowest precedence (allows assignments).
/// Recognises **typed bare-param lambdas** like `integer x => x + 1`
/// or `integer? c => c != null` — like the upstream fast-path, they
/// are valid anywhere an expression starts (call args included): the
/// `type name =>` token shape can't be anything else.
pub(crate) fn expr(p: &mut Parser) {
    if lambda::looks_like_typed_bare_lambda(p) {
        lambda::lambda_typed_bare(p);
        return;
    }
    expr_bp(p, 0);
}

/// Top-of-statement / top-of-init expression. Same as `expr` but
/// additionally recognises **multi-param bare lambdas** like
/// `x, y -> x + y`. That shape is only valid at a position where a
/// `,` can't already mean "next item in a list" (so not inside call
/// args / array literals).
pub(crate) fn expr_top(p: &mut Parser) {
    if let Some(count) = lambda::multi_bare_lambda_param_count(p) {
        lambda::lambda_multi_bare(p, count);
        return;
    }
    expr(p);
}

/// Tokens that can start an expression. Used by callers (e.g.
/// `return`) to decide whether an expression follows.
pub(crate) fn can_start_expr(kind: S) -> bool {
    matches!(
        kind,
        S::IntLiteral
            | S::RealLiteral
            | S::StringLiteral
            | S::KwTrue
            | S::KwFalse
            | S::KwNull
            | S::Lemniscate
            | S::Pi
            | S::Ident
            | S::LParen
            | S::LBracket
            | S::LBrace
            | S::Minus
            | S::Plus
            | S::Bang
            | S::Tilde
            | S::KwNot
            | S::PlusPlus
            | S::MinusMinus
            | S::At
            | S::KwNew
            | S::KwThis
            | S::KwSuper
            | S::KwClass
            | S::KwFunction
            | S::Arrow
            | S::FatArrow
            | S::Lt
            | S::RBracket // open-start interval `]a..b]`
    )
}

/// Pratt loop: parse an atom/prefix, then fold in operators whose
/// left binding power is at least `min_bp`. Depth-guarded so deeply
/// nested input (prefix chains, parens, binary trees) degrades to an
/// error node rather than overflowing the stack.
pub(super) fn expr_bp(p: &mut Parser, min_bp: u8) {
    if p.enter_recursion() {
        p.error("expression nests too deeply to parse");
        p.leave_recursion();
        return;
    }
    expr_bp_inner(p, min_bp);
    p.leave_recursion();
}

fn expr_bp_inner(p: &mut Parser, min_bp: u8) {
    let cp = p.checkpoint();
    if !atom_or_prefix(p) {
        return;
    }

    loop {
        let kind = p.current();

        // Postfix `(args)`: function call, highest binding power.
        if kind == S::LParen && CALL_BP >= min_bp {
            p.start_node_at(cp, S::CallExpr);
            arg_list(p);
            p.finish_node();
            continue;
        }

        // Postfix `[index]` or `[i:j[:k]]` slice.
        // Guard: only consume as subscript if there's a matching `]`
        // at the same depth. Otherwise this `[` is the close of an
        // outer interval expression (`[a..b[` etc.).
        // Second guard: a `[` whose contents hold a top-level `..` is
        // an interval literal (`[2..10]`), never a subscript — there is
        // no subscript syntax containing `..`. Without this, a
        // statement-initial interval after an expression-shaped line
        // (`}` / `]`) is swallowed as a subscript, `expr_bp` stops dead
        // at the `..`, and the `]` is left stranded (#415).
        if kind == S::LBracket
            && CALL_BP >= min_bp
            && has_matching_rbracket(p)
            && !looks_like_interval_open(p)
        {
            postfix_bracket(p, cp);
            continue;
        }

        // Postfix `?.field`: optional member access (#2272). Only taken
        // when an identifier-shaped name follows the `.` — otherwise the
        // `?` is a ternary opener (`a ? .5 : b` keeps parsing as ternary,
        // matching upstream's `? + DOT + STRING` lookahead).
        if kind == S::Question
            && CALL_BP >= min_bp
            && p.nth(1) == S::Dot
            && (p.nth(2) == S::Ident || p.nth(2).is_keyword())
        {
            p.start_node_at(cp, S::FieldExpr);
            p.bump(); // '?'
            p.bump(); // '.'
            p.bump(); // field name
            p.finish_node();
            continue;
        }

        // Postfix `.field`: member access.
        if kind == S::Dot && CALL_BP >= min_bp {
            p.start_node_at(cp, S::FieldExpr);
            p.bump(); // '.'
            // Identifier-shaped — accept any non-trivia non-bracket
            // token as a field name. Patterns like `obj.class`,
            // `expr.if`, `[0..1].class` etc. show up in upstream
            // tests where the field name happens to be a keyword.
            // Stricter checking belongs in the resolver.
            let next = p.current();
            if next == S::Ident || next.is_keyword() {
                p.bump();
            } else {
                p.error(format!("expected field name after `.`, found {next:?}"));
                p.finish_node();
                break;
            }
            p.finish_node();
            continue;
        }

        // Postfix `++` / `--` / `!` (non-null assertion).
        if POSTFIX_BP >= min_bp && matches!(kind, S::PlusPlus | S::MinusMinus | S::Bang) {
            p.start_node_at(cp, S::PostfixExpr);
            p.bump();
            p.finish_node();
            continue;
        }

        // `expr as Type` — cast at postfix-tier binding power.
        //
        // For the type's trailing `?` we look ahead: if a `:`
        // appears before the next statement-ending token, treat
        // `?` as a ternary opener (don't consume here); otherwise
        // the `?` is nullable type syntax (`as T?`).
        if kind == S::KwAs && POSTFIX_BP >= min_bp {
            p.start_node_at(cp, S::CastExpr);
            p.bump(); // 'as'
            if looks_like_ternary_after_type(p) {
                crate::grammar::types::ty_no_nullable(p);
            } else {
                crate::grammar::types::ty(p);
            }
            p.finish_node();
            continue;
        }

        // `expr instanceof Type` — relational level; rhs is a Type.
        // Same no-nullable variant: `x instanceof T ? a : b` is
        // common.
        if kind == S::KwInstanceof {
            let (l_bp, r_bp) = (40, 41);
            if l_bp < min_bp {
                break;
            }
            let _ = r_bp; // rhs is parsed as a type, not by expr_bp
            p.start_node_at(cp, S::BinaryExpr);
            p.bump();
            crate::grammar::types::ty_no_nullable(p);
            p.finish_node();
            continue;
        }

        // `not in` — two-token relational; lookahead before the
        // simpler `in` so we don't consume `not` as a unary.
        if kind == S::KwNot && p.nth(1) == S::KwIn {
            let l_bp = 40u8;
            if l_bp < min_bp {
                break;
            }
            p.start_node_at(cp, S::BinaryExpr);
            p.bump(); // 'not'
            p.bump(); // 'in'
            expr_bp(p, 41);
            p.finish_node();
            continue;
        }

        // Ternary `cond ? then : else` — right-assoc, level 1.
        if kind == S::Question && p.nth(1) != S::Question && TERNARY_BP_L >= min_bp {
            p.start_node_at(cp, S::TernaryExpr);
            p.bump(); // '?'
            expr_bp(p, 0); // then-branch
            p.expect(S::Colon);
            expr_bp(p, TERNARY_BP_R); // else-branch
            p.finish_node();
            continue;
        }

        // Note: bare `a..b` is *not* an interval in Leekscript.
        // Intervals are bracket-delimited: `[a..b]`, `]a..b[`,
        // etc., parsed as a primary expression in `atom_or_prefix`.

        // Binary operators.
        if let Some((l_bp, r_bp)) = binary_bp(kind) {
            // `>` inside `<...>` set/map literals is the closer,
            // not a greater-than comparison.
            if kind == S::Gt && !p.gt_is_binary {
                break;
            }
            if l_bp < min_bp {
                break;
            }
            p.start_node_at(cp, S::BinaryExpr);
            p.bump(); // operator
            expr_bp(p, r_bp);
            p.finish_node();
            continue;
        }

        break;
    }
}

/// Postfix `[ ... ]`: index `a[i]` or slice `a[i:j]` / `a[i:j:k]` /
/// `a[:j]` / `a[i:]` / `a[::k]` etc.
///
/// We always start as an `IndexExpr`; if we see a `:` inside, the
/// node kind is retroactively rewritten to `SliceExpr` by emitting
/// a checkpoint before the `[` and reopening on detection.
fn postfix_bracket(p: &mut Parser, cp: rowan::Checkpoint) {
    assert!(p.at(S::LBracket));
    // Cheap lookahead to decide index vs slice before committing.
    let mut is_slice = bracket_contains_top_level_colon(p);
    if !is_slice && p.nth(1) == S::Colon {
        is_slice = true;
    }
    if is_slice && p.version() < leek_syntax::Version::V4 {
        // Slice syntax `a[i:j]` is v4-only. Report that as its own
        // diagnostic, but still build the `SliceExpr` the source
        // describes: falling back to the `IndexExpr` production made
        // `expect(RBracket)` fail at the `:` and left the rest of the
        // slice stranded as top-level statements (#415).
        p.error("slice syntax `a[i:j]` requires language version 4");
    }
    if is_slice {
        p.start_node_at(cp, S::SliceExpr);
        p.bump(); // '['
        // first segment may be empty.
        if !matches!(p.current(), S::Colon | S::RBracket) {
            expr_bp(p, 0);
        }
        if p.eat(S::Colon) {
            if !matches!(p.current(), S::Colon | S::RBracket) {
                expr_bp(p, 0);
            }
            if p.eat(S::Colon) && !p.at(S::RBracket) {
                expr_bp(p, 0);
            }
        }
        p.expect(S::RBracket);
        p.finish_node();
    } else {
        p.start_node_at(cp, S::IndexExpr);
        p.bump(); // '['
        expr_bp(p, 0);
        p.expect(S::RBracket);
        p.finish_node();
    }
}

/// True if the tokens between `[` and the matching `]` contain a
/// top-level `:` (depth 0 with respect to the bracket).
fn bracket_contains_top_level_colon(p: &Parser) -> bool {
    assert!(p.at(S::LBracket));
    let mut i = 1usize;
    let mut depth = 0i32;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBrace => depth -= 1,
            S::RBracket if depth == 0 => return false,
            S::RBracket => depth -= 1,
            S::Colon if depth == 0 => return true,
            // A ternary `?` opens a depth that consumes a `:` — but
            // an array slice can't contain a ternary at depth 0
            // anyway, so we just treat `?` as neutral.
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Parse elements up to `closer`, with the separating comma
/// **optional**, then say whether the list ended *stuck*.
///
/// The comma is optional because upstream's element loops all have one
/// shape — run until the closing token, and skip a comma only if one
/// happens to be sitting there:
///
/// ```text
/// while (… != PAR_RIGHT) { function.addParameter(readExpression(true));
///                          if (get().getType() == VIRG) skip(); }
/// ```
///
/// (`WordCompiler.java:1787` for call arguments, `:2214` `readArray`,
/// `:2183` `readMap`). So `f(a b)`, `[1 2 3]` and `[k: v k2: v2]` are
/// the same programs as their comma'd forms, and minified sources use
/// that — `code/french.min.leek` is nothing but. Separation therefore
/// falls out of *precedence*: `[a -b]` is one element `a - b`, because
/// each element is a whole expression and `-` binds as the binary
/// operator before the element ends.
///
/// `element` parses one element and returns `false` when it has already
/// reported damage that makes the rest of the list meaningless (a map
/// entry with no `:`).
///
/// The return value is `true` when the list ended on a token `element`
/// could neither consume nor start from — it has already named that
/// token, so [`close_list`] must not name it a second time. Two
/// messages about one token is how a single missing comma used to grow
/// into fifteen diagnostics (#351).
pub(super) fn comma_optional_list(
    p: &mut Parser,
    closer: S,
    mut element: impl FnMut(&mut Parser) -> bool,
) -> bool {
    while !p.at(closer) && !p.at_eof() {
        // Flush pending trivia before taking the mark: `position`
        // counts raw tokens, and an element that only got as far as
        // flushing the newline in front of it would otherwise look like
        // it had made progress — which is a spinning loop, one
        // diagnostic per lap.
        p.finish_trivia();
        let before = p.position();
        let ok = element(p);
        if p.at(S::Comma) {
            p.bump();
        }
        // No progress means `element` reported the token at the cursor
        // and left it there; looping would report it forever.
        if !ok || p.position() == before {
            recover_to_closer(p, closer);
            return true;
        }
    }
    false
}

/// Swallow what is left of a list that gave up into one `ErrorNode`, so
/// the list still ends on its own closing token.
///
/// Silent by design: `element` has already named the token that stopped
/// it, and a message per skipped token is exactly the cascade this
/// exists to prevent. It only runs when the closer really is in reach
/// at this bracket depth — a list whose closer is missing altogether
/// stops where it stands rather than swallowing the rest of the file.
fn recover_to_closer(p: &mut Parser, closer: S) {
    if p.at(closer) || !closer_in_reach(p, closer) {
        return;
    }
    p.start_node(S::ErrorNode);
    while !p.at(closer) && !p.at_eof() {
        p.bump();
    }
    p.finish_node();
}

/// How far [`closer_in_reach`] looks before giving up. Same order as
/// the collection-literal look-aheads: enough for any hand-written
/// list, bounded so a pathological file cannot make recovery quadratic.
const RESYNC_CAP: usize = 128;

/// Does `closer` appear ahead at the *current* bracket depth? A closing
/// bracket that is not ours appearing first means our own is missing,
/// and skipping past it would swallow the construct we are nested in.
fn closer_in_reach(p: &Parser, closer: S) -> bool {
    let mut depth = 0i32;
    for i in 0..RESYNC_CAP {
        let kind = p.nth(i);
        if depth == 0 && kind == closer {
            return true;
        }
        match kind {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBracket | S::RBrace if depth == 0 => return false,
            S::RParen | S::RBracket | S::RBrace => depth -= 1,
            S::Eof => return false,
            _ => {}
        }
    }
    false
}

/// Consume a list's closing token, reporting it missing unless
/// [`comma_optional_list`] already reported why the list stopped.
pub(super) fn close_list(p: &mut Parser, closer: S, stuck: bool) {
    if stuck {
        let _ = p.eat(closer);
    } else {
        p.expect(closer);
    }
}

/// Parse `(arg, arg, ...)` for a function call. The comma between
/// arguments is optional — see [`comma_optional_list`].
pub(super) fn arg_list(p: &mut Parser) {
    assert!(p.at(S::LParen));
    p.start_node(S::ArgList);
    p.bump(); // '('
    let stuck = comma_optional_list(p, S::RParen, |p| {
        expr(p);
        true
    });
    close_list(p, S::RParen, stuck);
    p.finish_node();
}

/// `( … )` after an annotation. We accept a flat expression list —
/// same production as a call's, comma and all (it being optional);
/// richer arg syntax (key-value, identifiers) can land later.
pub(crate) fn annotation_args(p: &mut Parser) {
    arg_list(p);
}

// ---- Binding-power tables ----

/// Postfix-call binding power. Highest tier — calls bind tighter
/// than any binary operator (`a + b(c)` → `a + (b(c))`).
const CALL_BP: u8 = 100;

/// Binding power of postfix unary (`++`, `--`, `!`) and `as Type`.
/// Tighter than `**` so `-x++` parses as `-(x++)` etc.
const POSTFIX_BP: u8 = 90;

/// Prefix-unary binding power. Tighter than `**` — upstream
/// `-12 ** 2` parses as `(-12) ** 2 = 144`, not `-(12 ** 2)`.
pub(super) const PREFIX_BP: u8 = 85;

/// Ternary binding powers (right-assoc, level 1).
const TERNARY_BP_L: u8 = 7;
const TERNARY_BP_R: u8 = 6;

/// Left/right binding power for a binary operator. `None` for
/// tokens that don't open a binary expression.
fn binary_bp(kind: S) -> Option<(u8, u8)> {
    use S::{
        Amp, AmpAmp, AmpEq, Backslash, BackslashEq, Caret, CaretEq, Eq, EqEq, EqEqEq, Ge, Gt,
        KwAnd, KwIn, KwIs, KwOr, KwXor, Le, Lt, Minus, MinusEq, NotEq, NotEqEq, Percent, PercentEq,
        Pipe, PipeEq, PipePipe, Plus, PlusEq, QuestionQuestion, QuestionQuestionEq, ShiftLeft,
        ShiftLeftEq, ShiftRight, ShiftRightEq, Slash, SlashEq, Star, StarEq, StarStar, StarStarEq,
        UShiftRight, UShiftRightEq,
    };
    Some(match kind {
        // Level 0 (assignment): right-associative.
        Eq | PlusEq | MinusEq | StarEq | SlashEq | PercentEq | BackslashEq | StarStarEq | AmpEq
        | PipeEq | CaretEq | ShiftLeftEq | ShiftRightEq | UShiftRightEq | QuestionQuestionEq => {
            (5, 4)
        }

        // Level 2 (||, ??): left-associative. `or` is the keyword
        // form of `||`.
        PipePipe | QuestionQuestion | KwOr => (10, 11),

        // Level 3 (&&) — and Leekscript `xor` shares this tier.
        // `and` is the keyword form of `&&`.
        AmpAmp | KwXor | KwAnd => (20, 21),

        // Levels 4-6 (bitwise OR, XOR, AND): left-associative.
        Pipe => (22, 23),
        Caret => (25, 26),
        Amp => (28, 29),

        // Level 7 (equality). `is` is a v1+ keyword form of `==`.
        EqEq | NotEq | EqEqEq | NotEqEq | KwIs => (30, 31),

        // Level 8 (relational + `in`). `instanceof` and `not in` are
        // handled inline in `expr_bp` because their rhs / token
        // shape differs (rhs is a type / two-token operator).
        Lt | Le | Gt | Ge | KwIn => (40, 41),

        // Level 9 (shifts)
        ShiftLeft | ShiftRight | UShiftRight => (45, 46),

        // Level 10 (additive)
        Plus | Minus => (50, 51),

        // Level 11 (multiplicative)
        Star | Slash | Percent | Backslash => (60, 61),

        // Level 12 (exponentiation): right-associative.
        StarStar => (81, 80),

        _ => return None,
    })
}
