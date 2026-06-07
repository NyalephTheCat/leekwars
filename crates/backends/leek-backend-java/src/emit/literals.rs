use leek_hir::Literal;
use std::fmt::Write as _;

use super::escape_string;
impl super::Emitter<'_> {
    pub(crate) fn write_literal(&self, buf: &mut String, lit: &Literal, parens_if_negative: bool) {
        match lit {
            Literal::Int(n) => {
                if parens_if_negative && *n < 0 {
                    write!(buf, "({n}l)").unwrap();
                } else {
                    write!(buf, "{n}l").unwrap();
                }
            }
            Literal::Real(r) => {
                if r.is_infinite() && *r > 0.0 {
                    buf.push_str("Double.POSITIVE_INFINITY");
                } else if r.is_infinite() {
                    buf.push_str("Double.NEGATIVE_INFINITY");
                } else if r.is_nan() {
                    buf.push_str("Double.NaN");
                } else if parens_if_negative && *r < 0.0 {
                    write!(buf, "({r})").unwrap();
                } else {
                    // Match Java's `String.valueOf(double)` (always
                    // a decimal point — `42.0` not `42`).
                    let s = format!("{r}");
                    if s.contains('.') || s.contains('e') || s.contains('E') {
                        buf.push_str(&s);
                    } else {
                        buf.push_str(&s);
                        buf.push_str(".0");
                    }
                }
            }
            Literal::String(s) => {
                buf.push('"');
                buf.push_str(&escape_string(s, self.opts.version_byte() >= 2));
                buf.push('"');
            }
            Literal::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
            Literal::Null => buf.push_str("null"),
        }
    }
}
