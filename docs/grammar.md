# The LeekScript Language — Grammar Specification

**Status:** Reference specification
**Scope:** Lexical and syntactic grammar of LeekScript as implemented by this
workspace's frontend (`crates/frontend/leek-lexer`, `leek-parser`,
`leek-syntax`).
**Default language version:** `4` (`Version::LATEST`).

---

## 1. Introduction

LeekScript is the dynamically-typed scripting language behind
[LeekWars](https://leekwars.com). This document specifies its concrete grammar:
the lexical structure (how source text is split into tokens) and the syntactic
structure (how tokens form declarations, statements, and expressions).

The grammar is derived directly from the parser implementation in this
repository, not from an external language standard. Where the implementation
deviates from a naïve reading of the syntax — operator fusion, lookahead-based
disambiguation, version gates — those behaviours are documented explicitly so
the grammar reflects what the parser *actually* accepts.

### 1.1 Grammars and notational conventions

The notation is modelled on the **ECMA-262 (ECMAScript) grammar conventions**
([tc39.es/ecma262, §Notational Conventions](https://tc39.es/ecma262/#sec-notational-conventions)),
with two deliberate deviations noted at the end of this section.

**Two grammars.** Two cooperating grammars define the language, distinguished by
their defining symbol:

- The **lexical grammar** (§3) maps source characters to tokens. Its productions
  use the defining symbol **`::`**.
- The **syntactic grammar** (§4–§8) maps the token stream to a syntax tree. Its
  productions use the defining symbol **`:`**.

**Productions.** A production lists, on the lines following its left-hand side,
one *alternative* per line. For example

```ebnf
Stmt :
    Block
    IfStmt
```

defines `Stmt` as matching either a `Block` or an `IfStmt`.

**Terminals and nonterminals.**

- **Nonterminals** are written in `MixedCase` (`Stmt`, `Expr`, `IntLiteral`).
- **Terminals** — the literal tokens of §3 — are written in single quotes
  (`'while'`, `'->'`, `'('`). *(ECMA-262 sets terminals in a distinct typeface;
  because this document renders grammar in monospaced blocks, terminals are
  quoted instead.)*
- Abbreviations in angle brackets denote individual characters that are otherwise
  hard to show: `<SP>` space, `<TAB>` tab, `<LF>` line feed, `<CR>` carriage
  return, `<NBSP>` no-break space (U+00A0).

**`one of`.** When every alternative is a single terminal, the production is
written `Name :: one of` followed by the terminals laid out on the next line(s).
Items in a `one of` list are bare (unquoted), since the entire list is terminals:

```ebnf
BinDigit :: one of
    0 1
```

**`but not`.** `A but not B` matches any expansion of `A` that is not an
expansion of `B` (used to carve exceptions out of broad character classes).

**`[empty]`.** Denotes an alternative that matches no tokens.

**Lookahead restrictions.** `[lookahead ∉ { … }]` and `[lookahead = … ]` place a
condition on the next token(s); the surrounding alternative applies only when the
condition holds. These capture the parser's disambiguation decisions. Multi-token
or contextual restrictions that do not fit the bracket form are given as
`(* … *)` comments.

**Grouping.** `( … )` groups symbols. Inline alternation within a group uses `|`
(e.g. `( '->' | '=>' )`).

**Deviations from ECMA-262 (at the author's request).** Optional and repeated
symbols use postfix operators rather than ECMA-262's `opt` subscript and
recursive list productions:

| Notation | Meaning                              | ECMA-262 equivalent           |
| -------- | ------------------------------------ | ----------------------------- |
| `x?`     | optional — zero or one `x`           | `x` with `opt` subscript      |
| `x*`     | zero or more `x`                     | a recursive *List* production |
| `x+`     | one or more `x`                      | a recursive *List* production |

The postfix operators bind tighter than concatenation, which binds tighter than
alternation; use `( … )` to override.

### 1.2 Processing model

A LeekScript source file is processed in stages:

```
source text  ──▶  Lexer  ──▶  token stream  ──▶  Parser  ──▶  syntax tree (CST/AST)
```

1. **Lexing** (`leek-lexer`) converts UTF-8 source text into a flat stream of
   tokens, including *trivia* tokens (whitespace and comments). Lexing is
   parametrised by the language **version** (§2), which determines the keyword
   set and a handful of lexical rules.
2. **Parsing** (`leek-parser`) consumes the non-trivia tokens and produces a
   syntax tree. The parser is error-recovering: malformed input is wrapped in
   error nodes rather than aborting the parse.

The lexer and parser are both *total*: any input produces a token stream and a
tree; diagnostics are reported as a side channel.

---

## 2. Language versions

LeekScript has four language versions. The version in force changes the keyword
set and some lexical/syntactic rules.

```ebnf
Version :: one of
    1 2 3 4
```

| Version | Summary                                                            |
| ------- | ------------------------------------------------------------------ |
| `1`     | Base language: control flow, untyped/typed variables, functions.   |
| `2`     | Object orientation: `class`, `extends`, `this`, `super`, `new`, …  |
| `3`     | Java-style reserved-word set; fully case-sensitive keywords.       |
| `4`     | Current/default version. No new keywords over v3.                  |

The version is selected by a **pragma** comment (§3.8). When no version pragma is
present, the default is **v4** (`Version::LATEST`, matching the Java reference's
`WordCompiler.LATEST_VERSION`).

**Version-dependent lexical rules:**

- **Keyword case sensitivity.** In **v1–v2**, keyword matching is
  *case-insensitive*, with the sole exceptions of `class` and `function`, which
  are always case-sensitive (so `Class` and `Function` remain usable as
  identifiers). In **v3–v4**, all keyword matching is *case-sensitive*.
- **Keyword availability.** Each version adds keywords (§3.1). A word that is a
  keyword in a later version is an ordinary identifier in earlier versions.
- **Block-comment quirk (v1).** In v1 only, `/*/` is a complete block comment
  (the `/` immediately after `/*` terminates it). See §3.6.

Feature gates (`generics`, `types`, `interfaces`, `enums`,
`function_signatures`) are an orthogonal, opt-in mechanism that unlocks
experimental syntax independently of the version (§7).

---

## 3. Lexical grammar

The lexer reads UTF-8 text and is multi-byte safe: it never splits a UTF-8
sequence, and any unrecognised character is consumed as a single `Error` token
spanning the whole code point. `SourceCharacter` denotes any single Unicode code
point of the input.

A token stream is a sequence of tokens terminated by `EOF`:

```ebnf
TokenStream ::
    Token* EOF

Token ::
    Trivia
    Literal
    Ident
    Keyword
    Punctuator

Trivia ::
    Whitespace
    LineComment
    BlockComment

Literal ::
    IntLiteral
    RealLiteral
    StringLiteral
    Lemniscate
    Pi
```

Trivia tokens are retained in the concrete syntax tree but skipped by the
parser's grammar productions.

### 3.1 Keywords

Keywords are introduced cumulatively by version.

```ebnf
KeywordV1 :: one of
    var      global   return   function if       else
    while    for      do       in       break    continue
    null     true     false    and      or       not
    include  is       as       xor

KeywordV2 :: one of
    class    extends  this     super    new
    static   private  public   protected constructor

KeywordV3 :: one of
    switch   case     default  instanceof abstract await
    import   export   goto     catch      finally  try
    throw    throws   typeof   void       interface let
    native   package  byte     char       float    double
    int      long     short    transient  volatile synchronized
    enum     eval     final    with       yield    implements
    const    boolean
```

**Version 4** adds no new keywords.

Not every reserved word has a corresponding grammar construct. Keywords that are
**implemented** (the parser builds a node for them) are:

```ebnf
ImplementedKeyword :: one of
    var global return function if else while for do in break continue
    null true false and or not include xor class extends this super new
    static private public protected constructor instanceof is as
    switch case default import
```

The remaining v3 words (`abstract`, `await`, `export`, `goto`, `catch`,
`finally`, `try`, `throw`, `throws`, `typeof`, `void`, `interface`, `let`,
`native`, `package`, the Java numeric type names, `transient`, `volatile`,
`synchronized`, `enum`, `eval`, `final`, `with`, `yield`, `implements`, `const`,
`boolean`) are **reserved-only**: they tokenize as keywords but, outside of
feature-gated constructs (§7) or type position (§7), have no associated syntax.

**Keyword operators.** Three keywords lex to the same token as a symbolic
operator and behave identically in the grammar:

| Keyword | Equivalent symbol |
| ------- | ----------------- |
| `and`   | `&&`              |
| `or`    | `\|\|`            |
| `not`   | `!`               |

### 3.2 Identifiers

```ebnf
Ident ::
    IdentStart IdentCont*

IdentStart ::
    AsciiLetter
    '_'
    Latin1Letter

IdentCont ::
    IdentStart
    DecDigit
```

`AsciiLetter` is any character in `a`–`z` or `A`–`Z`. `Latin1Letter` covers the
accented Latin-1 letter ranges accepted by the lexer:

```
U+00C0…U+00D6   (À … Ö)
U+00D8…U+00DD   (Ø … Ý)
U+00E0…U+00F6   (à … ö)
U+00F8…U+00FD   (ø … ý)
U+0152…U+0153   (Œ, œ)
U+00FF          (ÿ)
```

Two Unicode characters are **not** identifier characters; each lexes as its own
single-character literal token (§3.4): `U+221E` `∞` (`Lemniscate`) and `U+03C0`
`π` (`Pi`).

### 3.3 Number literals

```ebnf
IntLiteral ::
    DecInt
    HexInt
    BinInt

RealLiteral ::
    DecReal
    HexReal

DecInt ::
    DecDigits 'L'?

HexInt ::
    HexPrefix HexDigits 'L'?

BinInt ::
    BinPrefix BinDigits 'L'?

DecReal ::
    DecDigits '.' DecDigits? DecExponent?
    DecDigits DecExponent

HexReal ::
    HexPrefix HexDigits ( '.' HexDigits? )? HexExponent

DecExponent ::
    ( 'e' | 'E' ) Sign? DecDigits

HexExponent ::
    ( 'p' | 'P' ) Sign? DecDigits

DecDigits ::
    DecDigit ( '_'? DecDigit )*

HexDigits ::
    HexDigit ( '_'? HexDigit )*

BinDigits ::
    BinDigit ( '_'? BinDigit )*

HexPrefix :: one of
    0x 0X

BinPrefix :: one of
    0b 0B

Sign :: one of
    + -

DecDigit :: one of
    0 1 2 3 4 5 6 7 8 9

HexDigit :: one of
    0 1 2 3 4 5 6 7 8 9 a b c d e f A B C D E F

BinDigit :: one of
    0 1
```

Notes and constraints (enforced by the lexer, with diagnostics):

- **Bases.** Decimal, hexadecimal (`0x`/`0X`), and binary (`0b`/`0B`). There is
  **no octal** prefix.
- **Digit separators.** A single underscore `_` may appear *between* digits, and
  immediately after a base prefix (`0x_ff`). Two consecutive underscores
  (`1__000`) raise `MULTIPLE_NUMERIC_SEPARATORS`. A prefix with no following
  digit raises `INVALID_NUMBER`.
- **Big-integer suffix.** A trailing uppercase `L` (only) marks a big integer:
  `2L`, `1_000L`. Lowercase `5l` and doubled `5LL` are invalid.
- **Reals.** A fractional point requires at least one digit before it; the digits
  after it are optional (`0.` is the real `0.0`). Decimal exponents use `e`/`E`.
  Hex-float literals use a mandatory binary exponent `p`/`P`
  (e.g. `0x1.p53`, `0xa.bcdp-42`). Binary (`0b`) literals support neither
  fraction nor exponent.
- **Range vs. trailing dot.** `1..10` lexes as `IntLiteral '..' IntLiteral`
  (an interval), never as `1.0 . 10`. `0.foo` lexes as `IntLiteral '.' Ident`
  (member access), not as a real.

### 3.4 Special numeric tokens

```ebnf
Lemniscate ::
    '∞'        (* U+221E — positive infinity *)

Pi ::
    'π'        (* U+03C0 — the constant pi *)
```

These are atomic single-character literal tokens (see §3.2).

### 3.5 String literals

```ebnf
StringLiteral ::
    '"' DoubleStringChar* '"'
    "'" SingleStringChar* "'"

DoubleStringChar ::
    SourceCharacter but not one of '"' or '\' or LineTerminator
    '\' SourceCharacter

SingleStringChar ::
    SourceCharacter but not one of "'" or '\' or LineTerminator
    '\' SourceCharacter
```

- Strings are delimited by **double** (`"…"`) or **single** (`'…'`) quotes.
- A backslash escapes the following character — including the delimiter
  (`"say \"hi\""`, `'it\'s'`). The **lexer does not interpret** escape sequences;
  the backslash and escaped character are preserved verbatim in the token text
  (interpretation happens later).
- Strings are **single-line**. An unterminated string (a `LineTerminator` or
  `EOF` before the closing quote) raises `STRING_NOT_CLOSED`; the token is still
  emitted.

### 3.6 Comments

```ebnf
LineComment ::
    '//' LineCommentChar*

LineCommentChar ::
    SourceCharacter but not LineTerminator

BlockComment ::
    '/*' BlockCommentChar* '*/'

BlockCommentChar ::
    SourceCharacter but not the sequence '*/'
```

- **Line comments** run to (but do not include) the end of line.
- **Block comments** are **not nestable**: the first `*/` closes the comment
  regardless of any intervening `/*`. An unterminated block comment extends to
  `EOF`.
- **v1 quirk:** in v1 only, `/*/` is a complete block comment.
- There is no distinct doc-comment token; documentation comments are ordinary
  line/block comments.

### 3.7 Whitespace and line terminators

```ebnf
Whitespace ::
    WhitespaceChar+

WhitespaceChar :: one of
    <SP> <TAB> <LF> <CR> <NBSP>

LineTerminator :: one of
    <LF> <CR>
```

Whitespace (including line terminators) is trivia. **There is no automatic
semicolon insertion**: a line terminator is whitespace, never a statement
terminator. A run of whitespace — possibly containing line terminators — forms a
single trivia token.

### 3.8 Pragmas

Pragmas are special `//`-comments that configure the compiler. They may appear
anywhere in the file.

```ebnf
Pragma ::
    '//' '@' PragmaBody

PragmaBody ::
    VersionPragma
    StrictPragma
    ExperimentalPragma
    UnknownPragma

VersionPragma ::
    'version' ':' Version

StrictPragma ::
    'strict'

ExperimentalPragma ::
    'experimental' ':' FeatureName
```

- `// @version:N` selects the language version (§2). Whitespace is permitted
  before `//`, after `//`, and around the `:`.
- `// @strict` enables strict mode (no value).
- `// @experimental:feature` opts into an experimental feature; multiple are
  allowed.
- Diagnostics: `PRAGMA_DUPLICATE` (repeated `@version`/`@strict`),
  `PRAGMA_BAD_VERSION`, `PRAGMA_INVALID_VALUE` (`@strict` given a value, or
  `@experimental` given none), and `PRAGMA_UNKNOWN` (warning).

### 3.9 Punctuators and operators

Multi-character operators are matched greedily (longest match): `===` is one
token, not `==` followed by `=`.

```ebnf
Punctuator :: one of
    ( ) [ ] { } , ; : . ..
    = == === != !==
    < <= > >=
    + - * / \ % **
    ++ --
    ~ & | ^
    << >> >>>
    && ||
    ! ? ??
    -> =>
    @

AssignmentOperator :: one of
    = += -= *= /= \= %= **=
    &= |= ^=
    <<= >>= >>>=
    ??=
```

`\` is integer (floored) division; `>>>` is the unsigned right shift; `@` serves
both as the by-reference marker and the annotation marker.

For reference, the named token kinds are: `LParen ( )`, `RParen )`, `LBracket [`,
`RBracket ]`, `LBrace {`, `RBrace }`, `Comma ,`, `Semicolon ;`, `Colon :`,
`Dot .`, `DotDot ..`, `Tilde ~`, `At @`, `Eq =`, `EqEq ==`, `EqEqEq ===`,
`NotEq !=`, `NotEqEq !==`, `Lt <`, `Le <=`, `Gt >`, `Ge >=`, `Plus +`,
`PlusPlus ++`, `PlusEq +=`, `Minus -`, `MinusMinus --`, `MinusEq -=`, `Star *`,
`StarStar **`, `StarEq *=`, `StarStarEq **=`, `Slash /`, `SlashEq /=`,
`Backslash \`, `BackslashEq \=`, `Percent %`, `PercentEq %=`, `Caret ^`,
`CaretEq ^=`, `Amp &`, `AmpAmp &&`, `AmpEq &=`, `Pipe |`, `PipePipe ||`,
`PipeEq |=`, `Bang !`, `Question ?`, `QuestionQuestion ??`,
`QuestionQuestionEq ??=`, `Arrow ->`, `FatArrow =>`, `ShiftLeft <<`,
`ShiftLeftEq <<=`, `ShiftRight >>`, `ShiftRightEq >>=`, `UShiftRight >>>`,
`UShiftRightEq >>>=`.

---

## 4. Syntactic grammar — top level

A source file is a sequence of top-level items. There is no enforced ordering
between declarations and statements; both may appear at the top level and in any
order.

```ebnf
SourceFile :
    TopLevelItem*

TopLevelItem :
    Annotation* FnDecl
    Annotation* ClassDecl
    Annotation* IncludeStmt
    Annotation* ImportStmt
    Annotation* Stmt

Annotation :
    '@' AnnotationName Arguments?

AnnotationName :
    Ident
    Keyword

Arguments :
    '(' ArgList? ')'
```

An annotation binds to the declaration that follows it (the annotation is folded
into that declaration's node). The parser distinguishes an *annotation*
`@name { … }` from an *expression statement* `@name(...);` by lookahead: a `;`
following the optional arguments marks an expression statement.

Statements are terminated by an **optional** `;` (§3.7 — there is no automatic
semicolon insertion, but most statements tolerate a missing or extra terminator).

---

## 5. Declarations

### 5.1 Variable declarations

```ebnf
VarDeclStmt :
    'var'    VarDeclarator    ( ',' VarDeclarator )*    ';'?
    Type     VarDeclarator    ( ',' VarDeclarator )*    ';'?
    'global' GlobalDeclarator ( ',' GlobalDeclarator )* ';'?

VarDeclarator :
    Ident ( '=' Expr )?

GlobalDeclarator :
    Type? Ident ( '=' Expr )?
```

The `Type`-prefixed alternative applies only under a lookahead restriction:
`[lookahead = Type Ident ( '=' | ';' | ',' | '}' | EOF )]`. An
expression-continuation token (`(`, `[`, `.`, `++`, `--`, a binary operator, …)
after the identifier disqualifies it, and the construct is parsed as an
expression statement instead.

- **Untyped** (`var x;`, `var a = 1, b = 2, c;`) and **typed**
  (`integer x = 5;`, `Array<integer> arr;`) forms coexist; multiple declarators
  share one keyword/type prefix.
- **Globals** (`global x = 10;`, `global integer x = 5;`) may mix typed and
  untyped declarators in one statement (`global a, integer b = 0;`).

### 5.2 Function declarations

```ebnf
FnDecl :
    'function' Ident? TypeParams? ParamList ReturnType? FnBody

FnBody :
    Block
    ';'                                   (* feature: function_signatures *)

ParamList :
    '(' ( Param ( ',' Param )* )? ')'

Param :
    '@'? ( Type '@'? )? Ident ( '=' Expr )?

ReturnType :
    ( '->' | '=>' ) Type

TypeParams :
    '<' Ident ( ',' Ident )* '>'          (* feature: generics *)
```

- The return type is introduced by `->` *or* `=>` (interchangeable) and is
  optional.
- **By-reference parameters** are marked with `@`. The canonical (upstream) form
  places `@` after the type (`integer @cell`); a leading `@` (`@integer cell`,
  `@cell`) is also accepted for compatibility. (By-reference parameters are a
  legacy v1 form, deprecated from v2 on.)
- Parameters may have **default values** (`function f(integer x = 5)`).
- A **bodiless** signature `function f() -> T;` is accepted only under the
  `function_signatures` feature gate; otherwise a `Block` is required.

### 5.3 Class declarations

```ebnf
ClassDecl :
    'class' Ident TypeParams? ClassExtends? ClassImplements? ClassBody

ClassExtends :
    'extends' Ident TypeParams?

ClassImplements :
    'implements' Ident ( ',' Ident )*     (* feature: interfaces *)

ClassBody :
    '{' ClassMember* '}'

ClassMember :
    Annotation* ClassConstructor
    Annotation* ClassMethod
    Annotation* ClassField

ClassField :
    Modifier* Type Ident ( '=' Expr )? ';'?

ClassMethod :
    Modifier* Type? Ident TypeParams? ParamList ReturnType? Block?

ClassConstructor :
    Modifier* 'constructor' ParamList Block?

Modifier : one of
    public private protected static final
```

- Single inheritance only (`extends` takes one class).
- A member is disambiguated as a **method** when its name is followed by `(`
  (or, under the `generics` feature, `<`); otherwise `Type Ident` followed by
  `=` or `;` is a **field**.
- Constructors are always named by the keyword `constructor`.
- Visibility defaults to public when no modifier is given. Modifiers may appear
  in any order. (`final` is a keyword only in v3+; in v1/v2 it is an ordinary
  identifier.)

---

## 6. Statements

```ebnf
Stmt :
    Block
    VarDeclStmt
    IfStmt
    WhileStmt
    DoWhileStmt
    ForStmt
    ForeachStmt
    SwitchStmt
    BreakStmt
    ContinueStmt
    ReturnStmt
    IncludeStmt
    ImportStmt
    ExprStmt
    EmptyStmt
```

### 6.1 Blocks and trivial statements

```ebnf
Block :
    '{' Stmt* '}'

ExprStmt :
    Expr ';'?

EmptyStmt :
    ';'
```

A bare `;` is a tolerated empty statement.

### 6.2 Conditionals

```ebnf
IfStmt :
    'if' '(' Expr ')' Stmt ( 'else' Stmt )?

SwitchStmt :
    'switch' '(' Expr ')' '{' SwitchCase* '}'

SwitchCase :
    'case' Expr ':' Stmt*
    'default' ':' Stmt*
```

- The dangling `else` binds to the nearest unmatched `if`.
- In a `switch`, a `case` label is followed by an arbitrary expression (not only
  a constant). Statements under a label run until the next `case`/`default` or
  the closing `}`; fall-through is implicit.

### 6.3 Loops

```ebnf
WhileStmt :
    'while' '(' Expr ')' Stmt

DoWhileStmt :
    'do' Stmt 'while' '(' Expr ')' ';'?

ForStmt :
    'for' '(' ForInit? ';' Expr? ';' Expr? ')' Stmt

ForInit :
    VarDeclStmt
    Expr

ForeachStmt :
    'for' '(' ForBinding ( ':' ForBinding )? 'in' Expr ')' Stmt

ForBinding :
    'var'? '@'? Type? Ident
```

- `ForStmt` is the C-style loop; all three clauses are optional
  (`for (;;)` is an infinite loop). The init clause may be a typed/untyped
  variable declaration or an expression.
- `ForeachStmt` has a single-binding value form (`for (x in arr)`) and a
  key/value form (`for (k : v in map)`). Each binding independently allows an
  optional `var`, an optional by-reference `@`, and an optional type.
- The parser distinguishes `ForeachStmt` from `ForStmt` by scanning for an `in`
  keyword at the head's top nesting level before any `;` — i.e.
  `[lookahead = … 'in' … before ';']`.

### 6.4 Jumps

```ebnf
BreakStmt :
    'break' ';'?

ContinueStmt :
    'continue' ';'?

ReturnStmt :
    'return' '?'? Expr? ';'?
```

Labeled break/continue is **not** supported. The optional `?` after `return` is
the **soft-return** marker (an extension whose meaning is resolved at a later
compilation stage).

### 6.5 Module statements

```ebnf
IncludeStmt :
    'include' '(' StringLiteral ')' ';'?

ImportStmt :
    'import' '(' StringLiteral ')' ';'?
    'import' StringLiteral ';'?
    'import' DottedPath ';'?

DottedPath :
    Ident ( '.' Ident )*
```

`include` takes a string-literal module name. `import` accepts a string literal
(optionally parenthesised) or a dotted identifier path.

---

## 7. Types

Types appear in variable/field/parameter declarations, return-type position,
`as` casts, and `instanceof`.

```ebnf
Type :
    TypeUnion ( ( '->' | '=>' ) Type )?          (* trailing arrow: function type *)

TypeUnion :
    TypeAtom ( '|' TypeAtom )* '?'*

TypeAtom :
    TypeName TypeArgs?
    'Array' TypeArgs? ( '[' TypeUnion ( ',' TypeUnion )* ']' )?
    ( '->' | '=>' ) Type

TypeArgs :
    '<' Type ( ',' Type )* GenericClose

TypeName :
    PrimitiveType
    Ident

PrimitiveType : one of
    integer real big_integer string boolean
    void any null Array Map
    Set Interval Object Function Number Boolean
```

- **Nullable.** A trailing `?` marks a nullable type; multiple `?` are tolerated
  (`integer?`, `Array<string>?`).
- **Unions.** `A | B | C` denotes a union of alternatives.
- **Generics.** `Array<integer>`, `Map<string, integer>`,
  `Array<Array<integer>>`. Because the lexer fuses consecutive `>` into `>>` /
  `>>>` (to avoid clashing with shift operators), `GenericClose` denotes the
  closing `>` that the parser splits back out of a fused `>>` or `>>>` token at
  the end of nested generic argument lists.
- **Tuple-shaped arrays** (`types` feature): `Array[T, U]` gives per-position
  element types, using `[ … ]` to distinguish from generic arguments.
- **Function types.** `integer => string`; a leading arrow `=> T` denotes a
  nullary function returning `T`. Function types appear chiefly in parameter
  annotations and lambda contexts.
- In v1/v2 most primitive names are lexed as ordinary identifiers; the type
  parser recognises them by spelling. Any identifier (commonly Capitalised) may
  name a user-defined class type.

---

## 8. Expressions

Expressions are parsed by a **Pratt (precedence-climbing) parser**. Each operator
has a *binding power*; the parser consumes an operator only when its left binding
power is at least the caller's minimum. The conventions in the implementation
are:

- A binary operator carries a pair `(l_bp, r_bp)`.
- `l_bp = r_bp − 1` ⟹ **left-associative**.
- `l_bp = r_bp + 1` ⟹ **right-associative**.

### 8.1 Precedence table

From **lowest** to **highest** binding. Each row binds tighter than the rows
above it.

| Tier | Operators | `(l_bp, r_bp)` | Assoc. |
| ---- | --------- | -------------- | ------ |
| Assignment | `=` `+=` `-=` `*=` `/=` `\=` `%=` `**=` `&=` `\|=` `^=` `<<=` `>>=` `>>>=` `??=` | `(5, 4)` | right |
| Ternary | `?` `:` | `(7, 6)` | right |
| Logical OR / coalesce | `\|\|` `or` `??` | `(10, 11)` | left |
| Logical AND / XOR | `&&` `and` `xor` | `(20, 21)` | left |
| Bitwise OR | `\|` | `(22, 23)` | left |
| Bitwise XOR | `^` | `(25, 26)` | left |
| Bitwise AND | `&` | `(28, 29)` | left |
| Equality | `==` `!=` `===` `!==` `is` | `(30, 31)` | left |
| Relational / membership | `<` `<=` `>` `>=` `in` `not in` `instanceof` | `(40, 41)` | left |
| Shift | `<<` `>>` `>>>` | `(45, 46)` | left |
| Additive | `+` `-` | `(50, 51)` | left |
| Multiplicative | `*` `/` `%` `\` | `(60, 61)` | left |
| Exponentiation | `**` | `(81, 80)` | right |
| Prefix unary | `-` `+` `!` `~` `not` `++` `--` `@` | `85` | right |
| Postfix unary | `++` `--` `!` (non-null), `as Type` | `90` | — |
| Call / access | `(args)` `[index]` `[slice]` `.field` `?.field` | `100` | — |

Notes:

- **Right-associative exponent** binds tighter than prefix unary, so `-12 ** 2`
  is `(-12) ** 2 = 144`, not `-(12 ** 2)`.
- The `?` of a ternary (`Question`) and the coalescing `??` (`QuestionQuestion`)
  are distinct tokens, so no grammar-level lookahead is needed to separate them.
- For `instanceof` and `as`, the right operand is a **Type** (§7), not an
  expression. `not in` is recognised as the two-token sequence `not` then `in`.

### 8.2 Expression structure

The production below collapses the precedence tiers of §8.1 into a single
`BinaryExpr` for brevity; in the implementation each tier is a distinct level of
the precedence-climbing loop with the associativity given in the table.

```ebnf
Expr :
    AssignmentExpr

AssignmentExpr :
    ConditionalExpr ( AssignmentOperator AssignmentExpr )?

ConditionalExpr :
    BinaryExpr ( '?' Expr ':' AssignmentExpr )?

BinaryExpr :
    UnaryExpr ( BinaryOperator UnaryExpr )*

UnaryExpr :
    PrefixOperator UnaryExpr
    PostfixExpr

PrefixOperator : one of
    - + ! ~ not ++ --  @

PostfixExpr :
    PrimaryExpr PostfixOp*

PostfixOp :
    '(' ArgList? ')'                              (* call *)
    '[' Expr ']'                                  (* index *)
    '[' Expr? ':' Expr? ( ':' Expr? )? ']'        (* slice, v4+ *)
    '.'  ( Ident | Keyword )                      (* member *)
    '?.' ( Ident | Keyword )                      (* optional member *)
    '++'
    '--'
    '!'                                           (* non-null assertion *)
    'as' Type                                     (* cast *)

ArgList :
    Expr ( ',' Expr )*
```

Member/field names after `.` and `?.` may be any identifier *or* keyword
(e.g. `obj.class`, `obj.if`); stricter checking happens in the resolver.

Notable lookahead restrictions:

- The `'[' Expr ']'` index applies only `[lookahead: a matching ']' exists at the
  same bracket depth]`, so it does not swallow an interval close (`[`).
- The slice form `'[' … ':' … ']'` is v4+; in v1–v3 a `:` inside `[ … ]` is
  rejected.
- `?.` applies only `[lookahead = '?.' Ident-shaped]`, so `a ? .5 : b` remains a
  ternary.

### 8.3 Primary expressions

```ebnf
PrimaryExpr :
    Literal
    NameRef
    '(' Expr ')'
    NewExpr
    Lambda
    CollectionLiteral
    IntervalExpr

NameRef :
    Ident
    'this'
    'super'
    'class'

NewExpr :
    'new' Ident ( '(' ArgList? ')' )?
```

(`Literal` is defined in §3.) `new MyClass(a, b)` invokes a constructor; the
argument list is optional (`new MyClass` constructs with no arguments).

### 8.4 Lambdas and anonymous functions

LeekScript accepts several lambda shapes; `->` and `=>` are interchangeable
arrows in all of them.

```ebnf
Lambda :
    Arrow LambdaBody                              (* zero parameters *)
    Ident Arrow LambdaBody                        (* one bare parameter *)
    Type Ident Arrow LambdaBody                   (* one typed parameter *)
    Ident ( ',' Ident )+ Arrow LambdaBody         (* multiple bare parameters *)
    '(' LambdaParams? ')' Arrow LambdaBody        (* parenthesised parameters *)
    '(' LambdaParams? Arrow LambdaBody ')'        (* inner-arrow form *)
    AnonFn

AnonFn :
    'function' Ident? ParamList ReturnType? Block

Arrow : one of
    -> =>

LambdaBody :
    Type? Block
    Type? Expr

LambdaParams :
    LambdaParam ( ',' LambdaParam )*

LambdaParam :
    Type? Ident
```

- The multiple-bare-parameter form (`x, y -> …`) is only recognised at
  statement/initialiser position, where the commas are unambiguous.
- A lambda body may be a block `{ … }` or a single expression, optionally
  preceded by a return type.

### 8.5 Collection literals

LeekScript shares the `[ … ]` and `{ … }` (and legacy `< … >`) delimiters across
arrays, maps, objects, and sets; the parser discriminates by the first separator
it sees.

```ebnf
CollectionLiteral :
    ArrayLiteral
    MapLiteral
    ObjectLiteral
    SetLiteral
    MapAngle
    SetAngle

ArrayLiteral :
    '[' ']'
    '[' Expr ( ',' Expr | Expr )* ']'

MapLiteral :
    '[' ':' ']'
    '[' Expr ':' Expr ( ',' Expr ':' Expr )* ']'

ObjectLiteral :
    '{' '}'
    '{' Expr ':' Expr ( ','? Expr ':' Expr )* '}'

SetLiteral :
    '{' SetElem ( ',' SetElem )* ','? '}'

MapAngle :
    '<' ':' '>'
    '<' Expr ':' Expr ( ',' Expr ':' Expr )* '>'

SetAngle :
    '<' '>'
    '<' SetElem ( ',' SetElem )* ','? '>'

SetElem :
    Expr '..' Expr                 (* a..b expands to an inclusive range *)
    Expr
```

Discrimination rules:

- `[` opens an **array**, a **map** (`[k: v, …]`, with the special empty form
  `[:]`), or an **interval** (§8.6), decided by whether a top-level `:` or `..`
  appears before the matching `]`.
- `{` opens an **object** (`{k: v, …}`) when the first separator is `:`, else a
  **set** (`{a, b, c}`). Commas between object entries are optional.
- `< … >` is the **legacy** map/set syntax (`<k: v>` map, `<a, b>` set, with
  empty forms `<:>` and `<>`). While parsing an angle collection, `>` is treated
  as the closer rather than the greater-than operator.
- In v1, array elements may be separated by spaces instead of commas.
- A set element may be a range `a..b`, which expands to the inclusive integer set
  (`{1..3}` ≡ `{1, 2, 3}`).

### 8.6 Interval literals

Intervals (v4+) are bracket-delimited ranges. The bracket *direction* encodes
inclusivity at each end: `[` is inclusive, `]` is exclusive.

```ebnf
IntervalExpr :
    OpenBracket Expr? '..' Expr? ( ':' Expr? )? CloseBracket

OpenBracket : one of
    [ ]            (* '[' inclusive start, ']' exclusive start *)

CloseBracket : one of
    ] [            (* ']' inclusive end,   '[' exclusive end   *)
```

A leading `[` is parsed as an interval (rather than an array/subscript) under the
restriction `[lookahead: a top-level '..' appears before the matching close
bracket]`. The optional `:` step gives a stride.

Examples:

```
[1..5]        inclusive 1 … 5
[1..5[        1 inclusive, 5 exclusive
]1..5]        1 exclusive, 5 inclusive
[..5]         open start, inclusive end
[1..]         inclusive start, open end
[1..10:2]     inclusive 1 … 10, step 2
[..]          unbounded
```

---

## 9. Worked examples

### 9.1 Recursion and operators

```leek
// @version: 4
function factorial(n) {
    if (n <= 1) {
        return 1;
    }
    return n * factorial(n - 1);
}
factorial(5);   // 120
```

### 9.2 Typed declarations, foreach, and collections

```leek
Array<integer> xs = [3, 1, 2];
Map<string, integer> scores = ["a": 1, "b": 2];

var total = 0;
for (x in xs) {
    total += x;
}
for (k : v in scores) {
    debug(k + " = " + v);
}
```

### 9.3 Classes

```leek
// @version: 2
class Animal {
    protected string name;
    constructor(string n) { this.name = n; }
    public string speak() -> string { return "..."; }
}

class Dog extends Animal {
    constructor(string n) { super(n); }
    public string speak() -> string { return this.name + " says woof"; }
}

var d = new Dog("Rex");
d.speak();
```

### 9.4 Lambdas, intervals, and operators

```leek
var sq = x -> x ** 2;             // single-parameter lambda
var add = (a, b) => a + b;        // parenthesised lambda
var r = [1..10:2];                // interval 1,3,5,7,9
var f = sq(3) + add(4, 5);        // 9 + 9 = 18
var label = f > 10 ? "big" : "small";
var name = maybeNull ?? "default";  // null-coalescing
```

---

## 10. Implementation cross-reference

This specification is derived from the following modules. For any question the
prose does not settle, the implementation is authoritative.

| Area | Source |
| ---- | ------ |
| Token kinds, versions, pragmas | `crates/frontend/leek-syntax/src/{kind,version,pragma,token,language}.rs` |
| Lexer (numbers, strings, idents, comments, operators) | `crates/frontend/leek-lexer/src/*.rs` |
| Expression grammar & precedence | `crates/frontend/leek-parser/src/grammar/expr/*.rs` |
| Statements & declarations | `crates/frontend/leek-parser/src/grammar/{stmt,decls,mod}.rs` |
| Type grammar | `crates/frontend/leek-parser/src/grammar/types.rs` |
| AST node kinds | `crates/frontend/leek-parser/src/ast.rs` |
| Parser driver / binding powers | `crates/frontend/leek-parser/src/parser.rs` |
