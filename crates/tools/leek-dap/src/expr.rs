//! The expression subset a breakpoint condition, a logpoint message and
//! `evaluate` are allowed to use.
//!
//! Three constraints shape this module.
//!
//! *It has to be owned.* A condition is compiled on the request loop's thread
//! (at `setBreakpoints`) and evaluated on the debuggee's (inside a safepoint),
//! so what crosses between them must be `Send`. A rowan [`SyntaxNode`] is the
//! thread-local cursor form of a tree and a [`Value`] is `Rc`-based; neither
//! may cross. [`CondExpr`] is plain owned data, and it is the only thing that
//! does.
//!
//! *It has to be a subset, and say so.* A breakpoint condition is evaluated
//! against a parked frame's locals and nothing else — there is no call, no
//! field, no index, because there is no interpreter here to run one in. What
//! is not supported is rejected by name when the condition is compiled, so
//! the client shows a hollow marker with a reason rather than a live one that
//! never fires.
//!
//! *It must not reimplement the language.* Every operator delegates to
//! [`leek_runtime`]'s shared semantics — the same functions the native backend
//! links as its runtime — so `==` at v1 means here exactly what it means in
//! the debuggee. Short-circuiting (`&&`, `||`, `??`) is the one thing done
//! here, because it is control flow rather than an operator.

use std::rc::Rc;

use leek_backend_native::DebugValue;
use leek_diagnostics::Severity;
use leek_parser::ast::{AstNode, Expr, SourceFile, Stmt};
use leek_runtime::Value;
use leek_span::SourceId;
use leek_syntax::SyntaxKind as S;
use leek_syntax::SyntaxNode;
use leek_syntax::version::Version;

/// The locals of the frame an expression is evaluated against.
pub(crate) type Frame = [(String, DebugValue)];

/// A compiled expression: owned, `Send`, and free of anything the evaluator
/// cannot answer from a frame's locals.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CondExpr {
    Int(i64),
    Real(f64),
    Str(String),
    Bool(bool),
    Null,
    /// A local of the frame the expression is evaluated against.
    Name(String),
    Unary(UnOp, Box<CondExpr>),
    Binary(BinOp, Box<CondExpr>, Box<CondExpr>),
    /// `cond ? then : else`.
    Ternary(Box<CondExpr>, Box<CondExpr>, Box<CondExpr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg,
    Not,
    /// Unary `+`, which the language keeps as a no-op.
    Pos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Rem,
    Pow,
    Eq,
    Ne,
    IdentityEq,
    IdentityNe,
    Lt,
    Le,
    Gt,
    Ge,
    /// `&&` — short-circuiting, so not one of the runtime's operators.
    And,
    /// `||` — short-circuiting.
    Or,
    /// `??` — short-circuiting on `=== null`.
    Coalesce,
}

/// The `SourceId` an expression is parsed under. Its own, not the debugged
/// program's: the text is the client's, it has no file, and no span from it
/// is ever reported.
fn expression_source() -> SourceId {
    SourceId::new(1).expect("source id 1 is non-zero")
}

/// Compile one expression at the debugged program's language version.
///
/// The version is not decoration: `==`, `/` and string escapes all mean
/// different things across versions, and a condition that disagrees with the
/// program it is testing is worse than no condition at all.
pub(crate) fn compile(text: &str, version: u8) -> Result<CondExpr, String> {
    // The debug adapter has no feature-flag channel of its own, so the
    // experimental toggles still come off the environment here — a
    // condition must parse under the same grammar the program did.
    let parsed = leek_parser::parse_with_features(
        text,
        expression_source(),
        Version::from_byte(version),
        leek_parser::ParseFeatures::from_env(),
    );
    if let Some(error) = parsed
        .diagnostics
        .iter()
        .find(|d| matches!(d.severity, Severity::Error))
    {
        return Err(error.message.clone());
    }
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green))
        .ok_or_else(|| "not an expression".to_string())?;
    let mut stmts = file.stmts();
    let Some(Stmt::Expr(stmt)) = stmts.next() else {
        return Err("not an expression".to_string());
    };
    if stmts.next().is_some() {
        return Err("more than one expression".to_string());
    }
    let expr = stmt.expr().ok_or_else(|| "not an expression".to_string())?;
    lower(&expr, version)
}

/// Lower one parsed expression to the owned form, or say what stopped it.
fn lower(expr: &Expr, version: u8) -> Result<CondExpr, String> {
    match expr {
        Expr::Literal(literal) => lower_literal(literal, version),
        Expr::Name(name) => name
            .ident()
            .map(|ident| CondExpr::Name(ident.text().to_string()))
            .ok_or_else(|| "not an expression".to_string()),
        Expr::Paren(paren) => {
            let inner = paren
                .inner()
                .ok_or_else(|| "empty parentheses".to_string())?;
            lower(&inner, version)
        }
        Expr::Unary(unary) => {
            let token = unary.op().ok_or_else(|| "not an expression".to_string())?;
            let op = match token.kind() {
                S::Minus => UnOp::Neg,
                S::Plus => UnOp::Pos,
                S::Bang | S::KwNot => UnOp::Not,
                _ => return Err(unsupported_op(token.text())),
            };
            let operand = unary
                .operand()
                .ok_or_else(|| "not an expression".to_string())?;
            Ok(CondExpr::Unary(op, Box::new(lower(&operand, version)?)))
        }
        Expr::Binary(binary) => {
            let token = binary.op().ok_or_else(|| "not an expression".to_string())?;
            let op = binary_op(token.kind()).ok_or_else(|| unsupported_op(token.text()))?;
            let (lhs, rhs) = (binary.lhs(), binary.rhs());
            let (lhs, rhs) = lhs
                .zip(rhs)
                .ok_or_else(|| "not an expression".to_string())?;
            Ok(CondExpr::Binary(
                op,
                Box::new(lower(&lhs, version)?),
                Box::new(lower(&rhs, version)?),
            ))
        }
        Expr::Ternary(ternary) => {
            // `TernaryExpr` exposes no named parts, so take its three operands
            // in source order: condition, then-value, else-value.
            let parts: Vec<Expr> = ternary.syntax().children().filter_map(Expr::cast).collect();
            let [cond, then, otherwise] = parts.as_slice() else {
                return Err("not an expression".to_string());
            };
            Ok(CondExpr::Ternary(
                Box::new(lower(cond, version)?),
                Box::new(lower(then, version)?),
                Box::new(lower(otherwise, version)?),
            ))
        }
        other => Err(format!(
            "{} are not available in a debugger expression",
            construct(other)
        )),
    }
}

/// What an expression form is called, for the message that rejects it.
fn construct(expr: &Expr) -> &'static str {
    match expr {
        Expr::Call(_) => "calls",
        Expr::Index(_) | Expr::Slice(_) => "indexing",
        Expr::Field(_) => "field accesses",
        Expr::Array(_) => "array literals",
        Expr::Map(_) => "map literals",
        Expr::Object(_) => "object literals",
        Expr::Set(_) => "set literals",
        Expr::Lambda(_) => "lambdas",
        Expr::New(_) => "constructors",
        Expr::Cast(_) => "casts",
        Expr::Postfix(_) => "postfix operators",
        Expr::Interval(_) => "intervals",
        // Every remaining form is lowered; this arm exists so a new `Expr`
        // variant is a compile error here rather than a silent acceptance.
        Expr::Literal(_)
        | Expr::Name(_)
        | Expr::Binary(_)
        | Expr::Unary(_)
        | Expr::Paren(_)
        | Expr::Ternary(_) => "expressions of this form",
    }
}

fn unsupported_op(text: &str) -> String {
    format!("`{text}` is not available in a debugger expression")
}

/// The runtime operator one token stands for, or `None` when the expression
/// language does not offer it (assignment, bitwise, `instanceof`, …).
fn binary_op(kind: S) -> Option<BinOp> {
    Some(match kind {
        S::Plus => BinOp::Add,
        S::Minus => BinOp::Sub,
        S::Star => BinOp::Mul,
        S::Slash => BinOp::Div,
        S::Backslash => BinOp::IntDiv,
        S::Percent => BinOp::Rem,
        S::StarStar => BinOp::Pow,
        // `is` is the language's word for `==`, as it is in the HIR.
        S::EqEq | S::KwIs => BinOp::Eq,
        S::NotEq => BinOp::Ne,
        S::EqEqEq => BinOp::IdentityEq,
        S::NotEqEq => BinOp::IdentityNe,
        S::Lt => BinOp::Lt,
        S::Le => BinOp::Le,
        S::Gt => BinOp::Gt,
        S::Ge => BinOp::Ge,
        S::AmpAmp | S::KwAnd => BinOp::And,
        S::PipePipe | S::KwOr => BinOp::Or,
        S::QuestionQuestion => BinOp::Coalesce,
        _ => return None,
    })
}

fn lower_literal(literal: &leek_parser::ast::LiteralExpr, version: u8) -> Result<CondExpr, String> {
    let token = literal
        .token()
        .ok_or_else(|| "not an expression".to_string())?;
    let text = token.text();
    match token.kind() {
        S::IntLiteral => text
            .parse::<i64>()
            .map(CondExpr::Int)
            .map_err(|_| format!("`{text}` is not an integer a debugger expression can use")),
        S::RealLiteral => text
            .parse::<f64>()
            .map(CondExpr::Real)
            .map_err(|_| format!("`{text}` is not a number a debugger expression can use")),
        S::StringLiteral => Ok(CondExpr::Str(leek_text::unescape(
            text,
            leek_text::EscapeMode::from_version(version),
        ))),
        S::KwTrue => Ok(CondExpr::Bool(true)),
        S::KwFalse => Ok(CondExpr::Bool(false)),
        S::KwNull => Ok(CondExpr::Null),
        _ => Err(unsupported_op(text)),
    }
}

/// Evaluate a compiled expression against a frame's locals.
///
/// Returns an error rather than panicking on anything it cannot answer — an
/// unknown name, a local with no scalar form. It runs on the debuggee thread
/// inside a safepoint, where a panic would take the debuggee down while the
/// request loop waits for a stop that never comes.
pub(crate) fn eval(expr: &CondExpr, vars: &Frame, version: u8) -> Result<Value, String> {
    match expr {
        CondExpr::Int(i) => Ok(Value::Int(*i)),
        CondExpr::Real(r) => Ok(Value::Real(*r)),
        CondExpr::Str(s) => Ok(Value::String(Rc::new(s.clone()))),
        CondExpr::Bool(b) => Ok(Value::Bool(*b)),
        CondExpr::Null => Ok(Value::Null),
        CondExpr::Name(name) => {
            let value = vars
                .iter()
                .find(|(var, _)| var == name)
                .map(|(_, value)| value)
                .ok_or_else(|| format!("unknown identifier `{name}`"))?;
            scalar(name, value)
        }
        CondExpr::Unary(op, operand) => {
            let value = eval(operand, vars, version)?;
            Ok(match op {
                UnOp::Neg => leek_runtime::neg(&value),
                UnOp::Not => Value::Bool(!value.is_truthy()),
                UnOp::Pos => value,
            })
        }
        // The three short-circuiting forms are control flow, not operators:
        // the right-hand side must not be evaluated at all when the left
        // settles the answer. `and`/`or` yield the constant the language's
        // own lowering yields, not the operand.
        CondExpr::Binary(BinOp::And, lhs, rhs) => {
            if eval(lhs, vars, version)?.is_truthy() {
                eval(rhs, vars, version)
            } else {
                Ok(Value::Bool(false))
            }
        }
        CondExpr::Binary(BinOp::Or, lhs, rhs) => {
            if eval(lhs, vars, version)?.is_truthy() {
                Ok(Value::Bool(true))
            } else {
                eval(rhs, vars, version)
            }
        }
        CondExpr::Binary(BinOp::Coalesce, lhs, rhs) => {
            let lhs = eval(lhs, vars, version)?;
            if matches!(lhs, Value::Null) {
                eval(rhs, vars, version)
            } else {
                Ok(lhs)
            }
        }
        CondExpr::Binary(op, lhs, rhs) => {
            let lhs = eval(lhs, vars, version)?;
            let rhs = eval(rhs, vars, version)?;
            Ok(apply(*op, &lhs, &rhs, version))
        }
        CondExpr::Ternary(cond, then, otherwise) => {
            if eval(cond, vars, version)?.is_truthy() {
                eval(then, vars, version)
            } else {
                eval(otherwise, vars, version)
            }
        }
    }
}

/// One local as a value an operator can take. A local with no scalar form —
/// an array, an object, a class instance — is an error rather than a guess.
fn scalar(name: &str, value: &DebugValue) -> Result<Value, String> {
    match value {
        DebugValue::Null => Ok(Value::Null),
        DebugValue::Bool(b) => Ok(Value::Bool(*b)),
        DebugValue::Int(i) => Ok(Value::Int(*i)),
        DebugValue::Real(r) => Ok(Value::Real(*r)),
        DebugValue::Str(s) => Ok(Value::String(Rc::new(s.clone()))),
        DebugValue::Opaque(_) => Err(format!(
            "`{name}` is not a number, string or boolean, so a debugger expression cannot use it"
        )),
    }
}

/// One non-short-circuiting operator, straight onto the runtime's own
/// semantics so the debugger and the debuggee cannot disagree.
fn apply(op: BinOp, lhs: &Value, rhs: &Value, version: u8) -> Value {
    match op {
        BinOp::Add => leek_runtime::add(lhs, rhs),
        BinOp::Sub => leek_runtime::sub(lhs, rhs),
        BinOp::Mul => leek_runtime::mul(lhs, rhs),
        BinOp::Div => leek_runtime::div(lhs, rhs, version),
        BinOp::IntDiv => leek_runtime::int_div(lhs, rhs),
        BinOp::Rem => leek_runtime::rem(lhs, rhs),
        BinOp::Pow => leek_runtime::pow(lhs, rhs),
        BinOp::Eq => leek_runtime::eq(lhs, rhs, version),
        BinOp::Ne => leek_runtime::ne(lhs, rhs, version),
        BinOp::IdentityEq => leek_runtime::identity_eq(lhs, rhs),
        BinOp::IdentityNe => leek_runtime::identity_ne(lhs, rhs),
        BinOp::Lt => leek_runtime::lt(lhs, rhs),
        BinOp::Le => leek_runtime::le(lhs, rhs),
        BinOp::Gt => leek_runtime::gt(lhs, rhs),
        BinOp::Ge => leek_runtime::ge(lhs, rhs),
        // Handled by `eval` before the operands are evaluated.
        BinOp::And | BinOp::Or | BinOp::Coalesce => Value::Null,
    }
}

/// Render a value the way the debuggee's own output would.
///
/// `Display` for a real is version-sensitive through a thread-local the
/// debuggee's runtime settles at start-up and the request loop never does, so
/// the version travels with the value rather than with the thread.
pub(crate) fn render(value: &Value, version: u8) -> String {
    let previous = leek_runtime::DISPLAY_VERSION.get();
    leek_runtime::DISPLAY_VERSION.set(version);
    let text = value.to_string();
    leek_runtime::DISPLAY_VERSION.set(previous);
    text
}

/// A logpoint's message: literal text with `{expression}` holes, compiled
/// once when the breakpoint is set rather than re-parsed at every hit.
#[derive(Debug, PartialEq)]
pub(crate) struct LogMessage(Vec<Segment>);

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    Expr(CondExpr),
}

/// Compile a logpoint message. An unmatched `{` is literal text — a message
/// is prose first and code second, and refusing to log because a brace was
/// meant literally would be the wrong way round.
pub(crate) fn compile_log(message: &str, version: u8) -> Result<LogMessage, String> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|at| open + at) else {
            break;
        };
        literal.push_str(&rest[..open]);
        if !literal.is_empty() {
            segments.push(Segment::Text(std::mem::take(&mut literal)));
        }
        segments.push(Segment::Expr(compile(&rest[open + 1..close], version)?));
        rest = &rest[close + 1..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        segments.push(Segment::Text(literal));
    }
    Ok(LogMessage(segments))
}

/// A logpoint's text for one hit. An expression that cannot be evaluated
/// prints its reason in place: the point of a logpoint is a line of output,
/// and a silently missing value is a worse answer than a visible complaint.
pub(crate) fn interpolate(message: &LogMessage, vars: &Frame, version: u8) -> String {
    let mut text = String::new();
    for segment in &message.0 {
        match segment {
            Segment::Text(literal) => text.push_str(literal),
            Segment::Expr(expr) => match eval(expr, vars, version) {
                Ok(value) => text.push_str(&render(&value, version)),
                Err(message) => {
                    text.push('<');
                    text.push_str(&message);
                    text.push('>');
                }
            },
        }
    }
    text
}

/// DAP's `hitCondition`: which hits of a breakpoint actually stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitCondition {
    /// `5` / `==5` — the fifth hit and no other.
    Eq(u64),
    /// `>=5` — from the fifth hit on.
    Ge(u64),
    /// `>5` — after the fifth hit.
    Gt(u64),
    /// `%3` — every third hit.
    Mod(u64),
}

impl HitCondition {
    /// Parse the DAP spelling. The forms are the ones every adapter agrees
    /// on; anything else is rejected by name rather than read as a count.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (build, rest): (fn(u64) -> Self, &str) = if let Some(rest) = text.strip_prefix(">=") {
            (Self::Ge, rest)
        } else if let Some(rest) = text.strip_prefix("==") {
            (Self::Eq, rest)
        } else if let Some(rest) = text.strip_prefix('>') {
            (Self::Gt, rest)
        } else if let Some(rest) = text.strip_prefix('%') {
            (Self::Mod, rest)
        } else {
            (Self::Eq, text)
        };
        let count: u64 = rest
            .trim()
            .parse()
            .map_err(|_| format!("`{text}` is not a hit count (try `5`, `>5`, `>=5` or `%3`)"))?;
        if count == 0 {
            return Err(format!("`{text}` is not a hit count: it never matches"));
        }
        Ok(build(count))
    }

    /// Whether the `hits`th hit of this breakpoint stops the debuggee.
    pub(crate) fn passes(self, hits: u64) -> bool {
        match self {
            Self::Eq(n) => hits == n,
            Self::Ge(n) => hits >= n,
            Self::Gt(n) => hits > n,
            Self::Mod(n) => hits.is_multiple_of(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CondExpr, HitCondition, compile, compile_log, eval, interpolate, render};
    use leek_backend_native::DebugValue;

    /// Latest language version, which is what a program without a `@version`
    /// pragma is compiled at.
    const LATEST: u8 = leek_span::pragma::LATEST_VERSION;

    fn frame() -> Vec<(String, DebugValue)> {
        vec![
            ("i".to_string(), DebugValue::Int(3)),
            ("x".to_string(), DebugValue::Int(11)),
            ("name".to_string(), DebugValue::Str("foo".to_string())),
            ("done".to_string(), DebugValue::Bool(false)),
            ("missing".to_string(), DebugValue::Null),
            ("list".to_string(), DebugValue::Opaque("[1, 2]".to_string())),
        ]
    }

    /// Compile and evaluate against [`frame`], reporting the rendered result.
    fn run(text: &str) -> Result<String, String> {
        let expr = compile(text, LATEST)?;
        eval(&expr, &frame(), LATEST).map(|value| render(&value, LATEST))
    }

    #[test]
    fn a_condition_reads_the_frames_locals() {
        assert_eq!(run("i == 3"), Ok("true".to_string()));
        assert_eq!(run("i == 4"), Ok("false".to_string()));
        assert_eq!(run("x > 10 && name == \"foo\""), Ok("true".to_string()));
        assert_eq!(run("x > 10 && name == \"bar\""), Ok("false".to_string()));
        assert_eq!(run("!done"), Ok("true".to_string()));
        assert_eq!(run("i * 2 + 1"), Ok("7".to_string()));
        assert_eq!(
            run("i > 2 ? \"big\" : \"small\""),
            Ok("\"big\"".to_string())
        );
    }

    #[test]
    fn the_short_circuiting_operators_do_not_touch_the_right_hand_side() {
        // `nope` is not a local: evaluating it at all is an error, so these
        // only succeed because the left-hand side settled the answer.
        assert_eq!(run("done && nope"), Ok("false".to_string()));
        assert_eq!(run("x > 1 || nope"), Ok("true".to_string()));
        assert_eq!(run("i ?? nope"), Ok("3".to_string()));
        // And `??` does fall through on a null, unlike `||` on a falsy value.
        assert_eq!(run("missing ?? 1"), Ok("1".to_string()));
    }

    #[test]
    fn an_operator_means_what_it_means_in_the_program_being_debugged() {
        // `"1" == 1` is true from v2 on and false at v1 — the debugger must
        // agree with the debuggee, which is why the version travels with the
        // compiled condition.
        let text = "name == 0";
        for version in [1, LATEST] {
            let expr = compile(text, version).expect("compiles");
            let value = eval(&expr, &frame(), version).expect("evaluates");
            assert_eq!(
                value.is_truthy(),
                leek_runtime::eq(
                    &leek_runtime::Value::String(std::rc::Rc::new("foo".to_string())),
                    &leek_runtime::Value::Int(0),
                    version
                )
                .is_truthy(),
                "v{version} disagreed with the runtime's own `==`"
            );
        }
    }

    #[test]
    fn an_unknown_name_is_an_error_that_names_it() {
        let error = run("nope > 1").expect_err("an unknown identifier");
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn a_local_with_no_scalar_form_is_an_error_not_a_guess() {
        let error = run("list > 1").expect_err("an array operand");
        assert!(error.contains("list"), "{error}");
    }

    #[test]
    fn what_the_subset_leaves_out_is_rejected_by_name() {
        for (text, named) in [
            ("f()", "calls"),
            ("list[0]", "indexing"),
            ("i.x", "field"),
            ("[1, 2]", "array"),
            ("i = 3", "`=`"),
            ("i & 1", "`&`"),
        ] {
            let error = compile(text, LATEST).expect_err(text);
            assert!(
                error.contains(named),
                "`{text}` was rejected without saying why: {error}"
            );
        }
    }

    #[test]
    fn an_expression_that_does_not_parse_reports_the_parse_error() {
        assert!(compile("i ==", LATEST).is_err());
        assert!(compile("", LATEST).is_err());
        assert!(compile("var a = 1", LATEST).is_err());
    }

    #[test]
    fn a_log_message_interpolates_its_braces_and_keeps_the_rest() {
        let message = compile_log("i={i} name={name}!", LATEST).expect("compiles");
        assert_eq!(interpolate(&message, &frame(), LATEST), "i=3 name=\"foo\"!");
        // A lone brace is prose, not a hole.
        let prose = compile_log("a { b", LATEST).expect("compiles");
        assert_eq!(interpolate(&prose, &frame(), LATEST), "a { b");
        // A hole that cannot be compiled fails the whole message, so the
        // breakpoint can be reported unverified instead of logging nonsense.
        assert!(compile_log("{f()}", LATEST).is_err());
    }

    #[test]
    fn a_log_hole_that_cannot_be_evaluated_says_so_in_place() {
        let message = compile_log("i={nope}", LATEST).expect("compiles");
        let text = interpolate(&message, &frame(), LATEST);
        assert!(text.starts_with("i=<") && text.contains("nope"), "{text}");
    }

    #[test]
    fn a_hit_condition_is_read_in_every_form_dap_spells_it() {
        assert_eq!(HitCondition::parse("5"), Ok(HitCondition::Eq(5)));
        assert_eq!(HitCondition::parse("== 5"), Ok(HitCondition::Eq(5)));
        assert_eq!(HitCondition::parse(">5"), Ok(HitCondition::Gt(5)));
        assert_eq!(HitCondition::parse(" >= 5 "), Ok(HitCondition::Ge(5)));
        assert_eq!(HitCondition::parse("%3"), Ok(HitCondition::Mod(3)));
        assert!(HitCondition::parse("<5").is_err());
        assert!(HitCondition::parse("many").is_err());
        assert!(HitCondition::parse("0").is_err(), "a count nothing matches");

        assert!(!HitCondition::Eq(2).passes(1));
        assert!(HitCondition::Eq(2).passes(2));
        assert!(!HitCondition::Eq(2).passes(3));
        assert!(HitCondition::Ge(2).passes(2) && HitCondition::Ge(2).passes(3));
        assert!(!HitCondition::Gt(2).passes(2) && HitCondition::Gt(2).passes(3));
        assert!(HitCondition::Mod(3).passes(3) && !HitCondition::Mod(3).passes(4));
    }

    #[test]
    fn a_literal_only_condition_needs_no_frame_at_all() {
        let expr = compile("true", LATEST).expect("compiles");
        assert_eq!(expr, CondExpr::Bool(true));
        assert!(eval(&expr, &[], LATEST).expect("evaluates").is_truthy());
    }
}
