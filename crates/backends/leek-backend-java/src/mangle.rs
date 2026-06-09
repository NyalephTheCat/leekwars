//! Identifier mangling. Mirrors `doc/java-backend.md` §4.
//!
//! Exact mode: `u_x` for locals/params, `g_x` for globals, `f_x` for
//! top-level functions, `u_C` for user classes, `m_m` for methods,
//! `s_n` for static class fields. Non-ASCII characters in identifiers
//! escape to `_uXXXX`. Java reserved words can never collide with
//! prefixed names so we don't special-case them in exact mode.
//!
//! Clean mode drops the prefix when the bare name doesn't collide
//! with a Java keyword.

use std::fmt::Write as _;

use crate::options::Options;

/// Java reserved words / restricted identifiers we must avoid in
/// clean mode. Kept small — the prefix system in exact mode already
/// avoids them. List per JLS §3.9, with the modern restricted ids.
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "yield",
    "record",
    "sealed",
    "permits",
    "var",
    "true",
    "false",
    "null",
];

fn is_java_keyword(s: &str) -> bool {
    JAVA_KEYWORDS.contains(&s)
}

/// Names live on the generated `AI` superclass (or imported statics)
/// that the emitter targets. Stripping the `f_`/`u_` prefix off a
/// user identifier with the same spelling would shadow them.
const RUNTIME_RESERVED: &[&str] = &[
    "add",
    "sub",
    "mul",
    "div",
    "mod",
    "pow",
    "neg",
    "ops",
    "increaseRAM",
    "decreaseRAM",
    "equals_equals",
    "notequals_equals",
    "compare",
    "concat",
    "clone",
    "bool",
    "this",
    "super",
    "session",
    "runIA",
    "staticInit",
];

fn collides_with_runtime(s: &str) -> bool {
    RUNTIME_RESERVED.contains(&s)
}

fn safe_chars(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            // `_uXXXX` escape, matching doc/java-backend.md §4.
            for unit in c.to_string().encode_utf16() {
                write!(out, "_u{unit:04X}").unwrap();
            }
        }
    }
    out
}

/// Top-level user function: `function attack` → `f_attack`.
pub fn function(opts: &Options, name: &str) -> String {
    let safe = safe_chars(name);
    if opts.is_clean() && !is_java_keyword(&safe) && !collides_with_runtime(&safe) {
        safe
    } else {
        format!("f_{safe}")
    }
}

/// Local / parameter / `for`-bound: `damage` → `u_damage`.
pub fn local(opts: &Options, name: &str) -> String {
    let safe = safe_chars(name);
    if opts.is_clean() && !is_java_keyword(&safe) && !collides_with_runtime(&safe) {
        safe
    } else {
        format!("u_{safe}")
    }
}

/// Global variable: `var global x` → `g_x`.
pub fn global(_opts: &Options, name: &str) -> String {
    // Globals stay prefixed even in clean mode — they share scope
    // with synthetic helpers in the generated class.
    format!("g_{}", safe_chars(name))
}

/// User class declaration: `class Cat` → `u_Cat`. Always `u_`-prefixed, even in
/// clean mode: the runtime recovers the leek class name from the Java class name
/// by stripping the first two chars (`getSimpleName().substring(2)`) when
/// formatting a visibility-denial error, so a bare clean-mode name (`Cat`) makes
/// that `substring(2)` throw `StringIndexOutOfBoundsException` instead of
/// returning null. The prefix is a runtime contract, not just disambiguation.
pub fn class_name(_opts: &Options, name: &str) -> String {
    format!("u_{}", safe_chars(name))
}

/// User method on an inner class: `m_run`. Superseded by the inline `u_<name>`
/// naming in `emit/class.rs`; kept for the deferred static-method work.
#[allow(dead_code)]
pub fn method(opts: &Options, name: &str) -> String {
    let safe = safe_chars(name);
    if opts.is_clean() && !is_java_keyword(&safe) && !collides_with_runtime(&safe) {
        safe
    } else {
        format!("m_{safe}")
    }
}

/// Static field on a user class: `s_n`. For the deferred static-field work.
#[allow(dead_code)]
pub fn static_field(_opts: &Options, name: &str) -> String {
    format!("s_{}", safe_chars(name))
}

/// Reassignable function (overwritten via `=`).
#[allow(dead_code)]
pub fn rfunction(_opts: &Options, name: &str) -> String {
    format!("rfunction_{}", safe_chars(name))
}

/// Uplifted anonymous function wrapper.
#[allow(dead_code)]
pub fn ufunction(_opts: &Options, name: &str) -> String {
    format!("ufunction_{}", safe_chars(name))
}
