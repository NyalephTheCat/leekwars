//! Token and node kinds.
//!
//! This is the first slice — enough kinds to lex the `library.leek`
//! fixture and a handful of others. The full catalog from
//! `doc/lexical.md` §10 lands incrementally as the parser grows.
//!
//! Layout: trivia first, then literals, then punctuation, then operators,
//! then keywords, then sentinels. Adding a kind in the middle is fine —
//! discriminants aren't load-bearing.

use crate::Version;

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyntaxKind {
    // ---- Trivia ----
    Whitespace,
    LineComment,
    BlockComment,

    // ---- Literals ----
    Ident,
    IntLiteral,
    RealLiteral,
    StringLiteral,
    /// Special identifier `∞` (U+221E) — positive infinity literal.
    Lemniscate,
    /// Special identifier `π` (U+03C0) — math constant.
    Pi,

    // ---- Punctuation ----
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    Colon,
    Dot,
    DotDot,

    // ---- Operators (subset for the first slice) ----
    Eq,                 // =
    EqEq,               // ==
    NotEq,              // !=
    EqEqEq,             // ===
    NotEqEq,            // !==
    Lt,                 // <
    Le,                 // <=
    Gt,                 // >
    Ge,                 // >=
    Plus,               // +
    Minus,              // -
    Star,               // *
    Slash,              // /
    Backslash,          // \
    Percent,            // %
    StarStar,           // **
    AmpAmp,             // &&
    PipePipe,           // ||
    Bang,               // !
    Question,           // ?
    QuestionQuestion,   // ??
    QuestionQuestionEq, // ??=
    Arrow,              // ->
    FatArrow,           // =>
    PlusPlus,           // ++
    MinusMinus,         // --
    PlusEq,             // +=
    MinusEq,            // -=
    StarEq,             // *=
    SlashEq,            // /=
    BackslashEq,        // \=
    PercentEq,          // %=
    StarStarEq,         // **=
    Caret,              // ^
    CaretEq,            // ^=
    Amp,                // &
    Pipe,               // |
    AmpEq,              // &=
    PipeEq,             // |=
    Tilde,              // ~
    At,                 // @
    ShiftLeft,          // <<
    ShiftLeftEq,        // <<=
    ShiftRight,         // >>
    ShiftRightEq,       // >>=
    UShiftRight,        // >>>
    UShiftRightEq,      // >>>=

    // ---- Keywords ----
    //
    // Layout: "implemented" first, then "reserved-only". The parser
    // only consumes the implemented set; reserved-only tokens
    // exist so identifier collision is detected.
    //
    // Implemented (parser consumes these):
    KwVar,
    KwGlobal,
    KwReturn,
    KwFunction,
    KwIf,
    KwElse,
    KwWhile,
    KwFor,
    KwDo,
    KwIn,
    KwBreak,
    KwContinue,
    KwNull,
    KwTrue,
    KwFalse,
    KwAnd, // mapped to AmpAmp on emit
    KwOr,  // mapped to PipePipe on emit
    KwNot, // mapped to Bang on emit
    KwXor, // logical xor
    KwInclude,
    KwClass,
    KwExtends,
    KwThis,
    KwSuper,
    KwNew,
    KwStatic,
    KwPrivate,
    KwPublic,
    KwProtected,
    KwConstructor,
    KwInstanceof,
    KwIs,
    KwAs,
    KwSwitch, // (parsed as switch block)
    KwCase,
    KwDefault,

    // Reserved-only (tokenize but produce no parser construct yet):
    // Note: `KwImport` is parsed, but still treated as an
    // experimental language feature.
    KwAbstract,
    KwAwait,
    KwImport,
    KwExport,
    KwGoto,
    KwCatch,
    KwFinally,
    KwTry,
    KwThrow,
    KwThrows,
    KwTypeof,
    KwVoid,
    KwInterface,
    KwLet,
    KwNative,
    KwPackage,
    KwByte,
    KwChar,
    KwFloat,
    KwDouble,
    KwInt,
    KwLong,
    KwShort,
    KwTransient,
    KwVolatile,
    KwSynchronized,
    KwEnum,
    KwEval,
    KwFinal,
    KwWith,
    KwYield,
    KwImplements,
    KwConst,
    KwBoolean,

    // ---- Sentinels ----
    Eof,
    Error,

    // ---- CST node kinds ----
    //
    // Anything below this comment is a *non-leaf* tree node, not a
    // token. Token kinds end at `Error`; node kinds begin at
    // `SourceFile`. `is_token`/`is_node` rely on this boundary.
    SourceFile,
    Block,

    // Top-level / declarations
    FnDecl,
    ParamList,
    Param,
    /// Experimental generic type-parameter list — `<T, U>` after a
    /// function/class/method name. Contains [`TypeParam`] children.
    TypeParamList,
    /// A single generic type parameter (one `Ident`) inside a
    /// [`TypeParamList`].
    TypeParam,
    IncludeStmt,
    ImportStmt,
    ClassDecl,
    ClassBody,
    ClassField,
    ClassMethod,
    ClassConstructor,

    // Statements
    VarDeclStmt,
    ExprStmt,
    ReturnStmt,
    IfStmt,
    WhileStmt,
    DoWhileStmt,
    ForStmt,
    ForeachStmt,
    SwitchStmt,
    SwitchCase,
    BreakStmt,
    ContinueStmt,

    // Expressions
    LiteralExpr,
    NameRef,
    BinaryExpr,
    UnaryExpr,
    ParenExpr,
    CallExpr,
    ArgList,
    /// `[a, b, c]` or `[]` — array literal.
    ArrayExpr,
    /// `expr[index]` — subscript.
    IndexExpr,
    /// `expr.field` — member access.
    FieldExpr,
    /// `[k: v, …]` and `[:]` — map literal (any-key dictionary).
    MapExpr,
    /// `{f: v, …}` — object literal (identifier-keyed record).
    ObjectExpr,
    /// `<a, b, c>` or `{a, b, c}` — set literal.
    SetExpr,
    /// `(params) -> body` or `name -> body` — anonymous function.
    LambdaExpr,
    /// `new Class(args)`.
    NewExpr,
    /// `expr as Type` — runtime-checked cast.
    CastExpr,
    /// `cond ? then : else` — ternary.
    TernaryExpr,
    /// `e++`, `e--`, `e!` — postfix unary.
    PostfixExpr,
    /// `start..end` or `start..end:step` — v4 interval literal.
    IntervalExpr,
    /// `a[i:j]` or `a[i:j:k]` — slicing, sibling of `IndexExpr`.
    SliceExpr,
    /// `@name [(args)]` — declaration annotation.
    Annotation,

    // Type system
    /// A complete type expression: atom plus optional `?` / `|` chain.
    TypeRef,
    /// Experimental `type Name = T` alias declaration (behind
    /// `LEEK_EXPERIMENTAL_TYPES`). Holds the alias-name `Ident` and a
    /// [`TypeRef`] body; invisible to HIR lowering, so codegen and
    /// runtime behavior are unaffected.
    TypeAliasDecl,
    /// Experimental `interface Name { … }` declaration (behind
    /// `LEEK_EXPERIMENTAL_INTERFACES`). Holds the interface-name
    /// `Ident` and [`InterfaceMember`] children; invisible to HIR
    /// lowering like [`TypeAliasDecl`], so runtime behavior is
    /// unaffected.
    InterfaceDecl,
    /// One interface member: a typed field (`integer hp`) or a
    /// bodiless method signature (`real area()`). A member with a
    /// `ParamList` child is a method; without one, a field.
    InterfaceMember,
    /// Experimental `implements I1, I2` clause inside a `class`
    /// header (behind `LEEK_EXPERIMENTAL_INTERFACES`). Wrapping the
    /// interface names in a node keeps the class's own name/parent
    /// token scans unaffected.
    ImplementsClause,
    /// Experimental `enum Name { A, B = 10 }` declaration (behind
    /// `LEEK_EXPERIMENTAL_ENUMS`). Holds the enum-name `Ident` and
    /// [`EnumMember`] children. Unlike [`TypeAliasDecl`] /
    /// [`InterfaceDecl`] it is NOT invisible to HIR: lowering turns it
    /// into a class with static final integer fields.
    EnumDecl,
    /// One enum variant: an `Ident`, optionally followed by
    /// `= (-)? IntLiteral` for an explicit value. Without one the
    /// value auto-increments from the previous variant (from 0).
    EnumMember,

    /// Error-recovery container — holds the tokens we skipped over.
    ErrorNode,
}

impl SyntaxKind {
    /// True for whitespace, line comments, and block comments. Skipped
    /// between tokens by the parser but preserved in the CST.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }

    pub fn is_keyword(self) -> bool {
        (self as u16) >= (Self::KwVar as u16) && (self as u16) <= (Self::KwBoolean as u16)
    }

    /// True for token (leaf) kinds, false for node (internal) kinds.
    pub fn is_token(self) -> bool {
        (self as u16) < (Self::SourceFile as u16)
    }

    pub fn is_node(self) -> bool {
        !self.is_token()
    }

    /// True for keywords the parser actually consumes (vs. reserved-only).
    /// The `WordCompiler.java` reference reserves many Java-style words
    /// it never implements; we mirror the reservation but expose this
    /// helper so consumers can warn or error on unimplemented usage.
    pub fn is_implemented_keyword(self) -> bool {
        ((self as u16) >= (Self::KwVar as u16) && (self as u16) <= (Self::KwDefault as u16))
            || self == Self::KwImport
    }
}

/// Look up a word in the keyword table for the active version.
///
/// Versions ≤ 2 use case-insensitive matching, except `class` which is
/// always case-sensitive (`LexicalParser.java:449`). v ≥ 3 is fully
/// case-sensitive.
pub fn keyword_lookup(word: &str, version: Version) -> Option<SyntaxKind> {
    // The slice's keyword set. Stays in sync with the SyntaxKind list above.
    // Each entry: (word, kind, min_version).
    const TABLE: &[(&str, SyntaxKind, Version)] = &[
        ("var", SyntaxKind::KwVar, Version::V1),
        ("global", SyntaxKind::KwGlobal, Version::V1),
        ("return", SyntaxKind::KwReturn, Version::V1),
        ("function", SyntaxKind::KwFunction, Version::V1),
        ("if", SyntaxKind::KwIf, Version::V1),
        ("else", SyntaxKind::KwElse, Version::V1),
        ("while", SyntaxKind::KwWhile, Version::V1),
        ("for", SyntaxKind::KwFor, Version::V1),
        ("do", SyntaxKind::KwDo, Version::V1),
        ("in", SyntaxKind::KwIn, Version::V1),
        ("break", SyntaxKind::KwBreak, Version::V1),
        ("continue", SyntaxKind::KwContinue, Version::V1),
        ("null", SyntaxKind::KwNull, Version::V1),
        ("true", SyntaxKind::KwTrue, Version::V1),
        ("false", SyntaxKind::KwFalse, Version::V1),
        ("and", SyntaxKind::KwAnd, Version::V1),
        ("or", SyntaxKind::KwOr, Version::V1),
        ("not", SyntaxKind::KwNot, Version::V1),
        ("include", SyntaxKind::KwInclude, Version::V1),
        // v2: object orientation
        ("class", SyntaxKind::KwClass, Version::V2),
        ("extends", SyntaxKind::KwExtends, Version::V2),
        ("this", SyntaxKind::KwThis, Version::V2),
        ("super", SyntaxKind::KwSuper, Version::V2),
        ("new", SyntaxKind::KwNew, Version::V2),
        ("static", SyntaxKind::KwStatic, Version::V2),
        ("private", SyntaxKind::KwPrivate, Version::V2),
        ("public", SyntaxKind::KwPublic, Version::V2),
        ("protected", SyntaxKind::KwProtected, Version::V2),
        ("constructor", SyntaxKind::KwConstructor, Version::V2),
        // v3: rest of the Java-style reserved-word list
        ("instanceof", SyntaxKind::KwInstanceof, Version::V3),
        ("is", SyntaxKind::KwIs, Version::V1),
        ("as", SyntaxKind::KwAs, Version::V3),
        ("xor", SyntaxKind::KwXor, Version::V1),
        ("switch", SyntaxKind::KwSwitch, Version::V3),
        ("case", SyntaxKind::KwCase, Version::V3),
        ("default", SyntaxKind::KwDefault, Version::V3),
        ("abstract", SyntaxKind::KwAbstract, Version::V3),
        ("await", SyntaxKind::KwAwait, Version::V3),
        ("import", SyntaxKind::KwImport, Version::V3),
        ("export", SyntaxKind::KwExport, Version::V3),
        ("goto", SyntaxKind::KwGoto, Version::V3),
        ("catch", SyntaxKind::KwCatch, Version::V3),
        ("finally", SyntaxKind::KwFinally, Version::V3),
        ("try", SyntaxKind::KwTry, Version::V3),
        ("throw", SyntaxKind::KwThrow, Version::V3),
        ("throws", SyntaxKind::KwThrows, Version::V3),
        ("typeof", SyntaxKind::KwTypeof, Version::V3),
        ("void", SyntaxKind::KwVoid, Version::V3),
        ("interface", SyntaxKind::KwInterface, Version::V3),
        ("let", SyntaxKind::KwLet, Version::V3),
        ("native", SyntaxKind::KwNative, Version::V3),
        ("package", SyntaxKind::KwPackage, Version::V3),
        ("byte", SyntaxKind::KwByte, Version::V3),
        ("char", SyntaxKind::KwChar, Version::V3),
        ("float", SyntaxKind::KwFloat, Version::V3),
        ("double", SyntaxKind::KwDouble, Version::V3),
        ("int", SyntaxKind::KwInt, Version::V3),
        ("long", SyntaxKind::KwLong, Version::V3),
        ("short", SyntaxKind::KwShort, Version::V3),
        ("transient", SyntaxKind::KwTransient, Version::V3),
        ("volatile", SyntaxKind::KwVolatile, Version::V3),
        ("synchronized", SyntaxKind::KwSynchronized, Version::V3),
        ("enum", SyntaxKind::KwEnum, Version::V3),
        ("eval", SyntaxKind::KwEval, Version::V3),
        ("final", SyntaxKind::KwFinal, Version::V3),
        ("with", SyntaxKind::KwWith, Version::V3),
        ("yield", SyntaxKind::KwYield, Version::V3),
        ("implements", SyntaxKind::KwImplements, Version::V3),
        ("const", SyntaxKind::KwConst, Version::V3),
        ("boolean", SyntaxKind::KwBoolean, Version::V3),
    ];

    let case_sensitive = version >= Version::V3;

    for &(name, kind, min) in TABLE {
        if version < min {
            continue;
        }
        // `class` and `function` are always case-sensitive even in
        // v1/v2 — `Class` and `Function` are reserved class/type
        // names that clash with the case-folded keyword lookup.
        let matched =
            if case_sensitive || matches!(kind, SyntaxKind::KwClass | SyntaxKind::KwFunction) {
                word == name
            } else {
                word.eq_ignore_ascii_case(name)
            };
        if matched {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_in_v1() {
        assert_eq!(keyword_lookup("VAR", Version::V1), Some(SyntaxKind::KwVar));
        assert_eq!(keyword_lookup("Var", Version::V2), Some(SyntaxKind::KwVar));
    }

    #[test]
    fn case_sensitive_in_v3_plus() {
        assert_eq!(keyword_lookup("VAR", Version::V3), None);
        assert_eq!(keyword_lookup("var", Version::V3), Some(SyntaxKind::KwVar));
    }

    #[test]
    fn class_always_case_sensitive() {
        assert_eq!(keyword_lookup("Class", Version::V1), None);
        assert_eq!(keyword_lookup("CLASS", Version::V2), None);
        assert_eq!(
            keyword_lookup("class", Version::V2),
            Some(SyntaxKind::KwClass)
        );
    }

    #[test]
    fn class_gated_to_v2() {
        // In v1, `class` is not a keyword at all — it's just an identifier.
        assert_eq!(keyword_lookup("class", Version::V1), None);
    }

    #[test]
    fn instanceof_gated_to_v3() {
        assert_eq!(keyword_lookup("instanceof", Version::V2), None);
        assert_eq!(
            keyword_lookup("instanceof", Version::V3),
            Some(SyntaxKind::KwInstanceof)
        );
    }

    #[test]
    fn trivia_classification() {
        assert!(SyntaxKind::Whitespace.is_trivia());
        assert!(SyntaxKind::LineComment.is_trivia());
        assert!(!SyntaxKind::Ident.is_trivia());
    }

    #[test]
    fn keyword_classification() {
        assert!(SyntaxKind::KwVar.is_keyword());
        assert!(!SyntaxKind::Ident.is_keyword());
        assert!(!SyntaxKind::Eof.is_keyword());
    }
}
