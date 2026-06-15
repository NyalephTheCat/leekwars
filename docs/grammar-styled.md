<!--
  ECMA-262-styled rendering of the LeekScript grammar.
  Productions use raw HTML inside <pre> blocks so they render with the
  ECMA-262 look: italic nonterminals, bold terminals, and an "opt" subscript
  for optional symbols. The canonical, monospace/EBNF version of this
  specification is docs/grammar.md; the two are kept in sync.
-->

# The LeekScript Language — Grammar Specification *(ECMA-styled rendering)*

**Status:** Reference specification
**Scope:** Lexical and syntactic grammar of LeekScript as implemented by this
workspace's frontend (`crates/frontend/leek-lexer`, `leek-parser`,
`leek-syntax`).
**Default language version:** `4` (`Version::LATEST`).
**Companion:** [`grammar.md`](grammar.md) — the same grammar in plain
monospace EBNF. This document renders it in the **ECMA-262 styled-prose form**.

---

## 1. Introduction

LeekScript is the dynamically-typed scripting language behind
[LeekWars](https://leekwars.com). This document specifies its concrete grammar:
the lexical structure (how source text is split into tokens) and the syntactic
structure (how tokens form declarations, statements, and expressions). It is
derived directly from the parser implementation in this repository.

### 1.1 Grammars and notational conventions

The presentation follows the **ECMA-262 (ECMAScript) grammar conventions**
([tc39.es/ecma262, §Notational Conventions](https://tc39.es/ecma262/#sec-notational-conventions)):

- **Nonterminals** are set in *italics*: <i>Statement</i>.
- **Terminals** — the literal tokens of §3 — are set in **bold**: <b>while</b>,
  <b>-&gt;</b>.
- A **production** lists each *alternative* on its own indented line beneath the
  left-hand side. The **lexical grammar** (§3) uses the defining symbol `::`; the
  **syntactic grammar** (§4–§8) uses `:`.
- An **<sub>opt</sub>** subscript marks an optional symbol: <i>Expr</i><sub>opt</sub>
  matches an <i>Expr</i> or nothing.
- **one of** introduces a list whose every alternative is a single terminal.
- **<i>A</i> but not <i>B</i>** matches any expansion of <i>A</i> that is not an
  expansion of <i>B</i>.
- **[empty]** denotes an alternative that matches no tokens.
- **[lookahead …]** is a restriction on the following token(s); the surrounding
  alternative applies only when it holds.
- Character abbreviations: `<SP>` space, `<TAB>` tab, `<LF>` line feed,
  `<CR>` carriage return, `<NBSP>` no-break space (U+00A0).

**Repetition.** ECMA-262 expresses repetition with recursive *List* productions.
For compactness this document keeps two postfix operators (a deliberate
deviation): a star `*` after a symbol means *zero or more*, and a plus `+` means
*one or more*. (Optionality still uses the <sub>opt</sub> subscript.)

For example:

<pre>
<i>Stmt</i> :
    <i>Block</i>
    <i>IfStmt</i>

<i>IfStmt</i> :
    <b>if</b> <b>(</b> <i>Expr</i> <b>)</b> <i>Stmt</i> <i>ElseClause</i><sub>opt</sub>

<i>BinDigit</i> :: <b>one of</b>
    <b>0</b>  <b>1</b>
</pre>

### 1.2 Processing model

```
source text  ──▶  Lexer  ──▶  token stream  ──▶  Parser  ──▶  syntax tree
```

**Lexing** (`leek-lexer`) turns UTF-8 text into a flat token stream, including
*trivia* (whitespace, comments); it is parametrised by the language **version**
(§2). **Parsing** (`leek-parser`) consumes the non-trivia tokens and builds an
error-recovering syntax tree. Both stages are total; diagnostics are a side
channel.

---

## 2. Language versions

<pre>
<i>Version</i> :: <b>one of</b>
    <b>1</b>  <b>2</b>  <b>3</b>  <b>4</b>
</pre>

| Version | Summary                                                            |
| ------- | ------------------------------------------------------------------ |
| `1`     | Base language: control flow, untyped/typed variables, functions.   |
| `2`     | Object orientation: `class`, `extends`, `this`, `super`, `new`, …  |
| `3`     | Java-style reserved-word set; fully case-sensitive keywords.       |
| `4`     | Current/default version. No new keywords over v3.                  |

The version is chosen by a **pragma** (§3.8); absent one, the default is **v4**
(`Version::LATEST`). Version-dependent lexical rules:

- **Case sensitivity.** In **v1–v2** keyword matching is case-insensitive, except
  `class` and `function` (always case-sensitive). In **v3–v4** it is fully
  case-sensitive.
- **Keyword availability** grows with the version (§3.1); a later version's
  keyword is an ordinary identifier in earlier ones.
- **v1 block-comment quirk:** `/*/` is a complete block comment (§3.6).

Feature gates (`generics`, `types`, `interfaces`, `enums`,
`function_signatures`) are an orthogonal opt-in mechanism (§7).

---

## 3. Lexical grammar

`SourceCharacter` denotes any single Unicode code point of the input. The lexer
is multi-byte safe; an unrecognised character becomes a single <i>Error</i>
token.

<pre>
<i>TokenStream</i> ::
    <i>Token</i>* <b>EOF</b>

<i>Token</i> ::
    <i>Trivia</i>
    <i>Literal</i>
    <i>Ident</i>
    <i>Keyword</i>
    <i>Punctuator</i>

<i>Trivia</i> ::
    <i>Whitespace</i>
    <i>LineComment</i>
    <i>BlockComment</i>

<i>Literal</i> ::
    <i>IntLiteral</i>
    <i>RealLiteral</i>
    <i>StringLiteral</i>
    <i>Lemniscate</i>
    <i>Pi</i>
</pre>

Trivia tokens are kept in the concrete tree but skipped by the syntactic grammar.

### 3.1 Keywords

Keywords are introduced cumulatively by version.

<pre>
<i>KeywordV1</i> :: <b>one of</b>
    <b>var</b>      <b>global</b>   <b>return</b>   <b>function</b> <b>if</b>       <b>else</b>
    <b>while</b>    <b>for</b>      <b>do</b>       <b>in</b>       <b>break</b>    <b>continue</b>
    <b>null</b>     <b>true</b>     <b>false</b>    <b>and</b>      <b>or</b>       <b>not</b>
    <b>include</b>  <b>is</b>       <b>as</b>       <b>xor</b>

<i>KeywordV2</i> :: <b>one of</b>
    <b>class</b>    <b>extends</b>  <b>this</b>     <b>super</b>    <b>new</b>
    <b>static</b>   <b>private</b>  <b>public</b>   <b>protected</b> <b>constructor</b>

<i>KeywordV3</i> :: <b>one of</b>
    <b>switch</b>   <b>case</b>     <b>default</b>  <b>instanceof</b> <b>abstract</b> <b>await</b>
    <b>import</b>   <b>export</b>   <b>goto</b>     <b>catch</b>      <b>finally</b>  <b>try</b>
    <b>throw</b>    <b>throws</b>   <b>typeof</b>   <b>void</b>       <b>interface</b> <b>let</b>
    <b>native</b>   <b>package</b>  <b>byte</b>     <b>char</b>       <b>float</b>    <b>double</b>
    <b>int</b>      <b>long</b>     <b>short</b>    <b>transient</b>  <b>volatile</b> <b>synchronized</b>
    <b>enum</b>     <b>eval</b>     <b>final</b>    <b>with</b>       <b>yield</b>    <b>implements</b>
    <b>const</b>    <b>boolean</b>
</pre>

**Version 4** adds no new keywords. Keywords the parser actually builds a node
for (the rest are reserved-only):

<pre>
<i>ImplementedKeyword</i> :: <b>one of</b>
    <b>var</b> <b>global</b> <b>return</b> <b>function</b> <b>if</b> <b>else</b> <b>while</b> <b>for</b> <b>do</b> <b>in</b> <b>break</b> <b>continue</b>
    <b>null</b> <b>true</b> <b>false</b> <b>and</b> <b>or</b> <b>not</b> <b>include</b> <b>xor</b> <b>class</b> <b>extends</b> <b>this</b> <b>super</b> <b>new</b>
    <b>static</b> <b>private</b> <b>public</b> <b>protected</b> <b>constructor</b> <b>instanceof</b> <b>is</b> <b>as</b>
    <b>switch</b> <b>case</b> <b>default</b> <b>import</b>
</pre>

**Keyword operators.** Three keywords lex to the same token as a symbolic
operator: <b>and</b> ≡ <b>&amp;&amp;</b>, <b>or</b> ≡ <b>||</b>, <b>not</b> ≡ <b>!</b>.

### 3.2 Identifiers

<pre>
<i>Ident</i> ::
    <i>IdentStart</i> <i>IdentCont</i>*

<i>IdentStart</i> ::
    <i>AsciiLetter</i>
    <b>_</b>
    <i>Latin1Letter</i>

<i>IdentCont</i> ::
    <i>IdentStart</i>
    <i>DecDigit</i>
</pre>

<i>AsciiLetter</i> is any character `a`–`z` or `A`–`Z`. <i>Latin1Letter</i>
covers the accented Latin-1 ranges accepted by the lexer:

```
U+00C0…U+00D6  (À…Ö)   U+00D8…U+00DD  (Ø…Ý)   U+00E0…U+00F6  (à…ö)
U+00F8…U+00FD  (ø…ý)   U+0152…U+0153  (Œ, œ)  U+00FF  (ÿ)
```

`U+221E` `∞` (<i>Lemniscate</i>) and `U+03C0` `π` (<i>Pi</i>) are **not**
identifier characters — each is its own literal token (§3.4).

### 3.3 Number literals

<pre>
<i>IntLiteral</i> ::
    <i>DecInt</i>
    <i>HexInt</i>
    <i>BinInt</i>

<i>RealLiteral</i> ::
    <i>DecReal</i>
    <i>HexReal</i>

<i>DecInt</i> ::
    <i>DecDigits</i> <b>L</b><sub>opt</sub>

<i>HexInt</i> ::
    <i>HexPrefix</i> <i>HexDigits</i> <b>L</b><sub>opt</sub>

<i>BinInt</i> ::
    <i>BinPrefix</i> <i>BinDigits</i> <b>L</b><sub>opt</sub>

<i>DecReal</i> ::
    <i>DecDigits</i> <b>.</b> <i>DecDigits</i><sub>opt</sub> <i>DecExponent</i><sub>opt</sub>
    <i>DecDigits</i> <i>DecExponent</i>

<i>HexReal</i> ::
    <i>HexPrefix</i> <i>HexDigits</i> ( <b>.</b> <i>HexDigits</i><sub>opt</sub> )<sub>opt</sub> <i>HexExponent</i>

<i>DecExponent</i> ::
    ( <b>e</b> | <b>E</b> ) <i>Sign</i><sub>opt</sub> <i>DecDigits</i>

<i>HexExponent</i> ::
    ( <b>p</b> | <b>P</b> ) <i>Sign</i><sub>opt</sub> <i>DecDigits</i>

<i>DecDigits</i> ::
    <i>DecDigit</i> ( <b>_</b><sub>opt</sub> <i>DecDigit</i> )*

<i>HexDigits</i> ::
    <i>HexDigit</i> ( <b>_</b><sub>opt</sub> <i>HexDigit</i> )*

<i>BinDigits</i> ::
    <i>BinDigit</i> ( <b>_</b><sub>opt</sub> <i>BinDigit</i> )*

<i>HexPrefix</i> :: <b>one of</b>
    <b>0x</b>  <b>0X</b>

<i>BinPrefix</i> :: <b>one of</b>
    <b>0b</b>  <b>0B</b>

<i>Sign</i> :: <b>one of</b>
    <b>+</b>  <b>-</b>

<i>DecDigit</i> :: <b>one of</b>
    <b>0</b> <b>1</b> <b>2</b> <b>3</b> <b>4</b> <b>5</b> <b>6</b> <b>7</b> <b>8</b> <b>9</b>

<i>HexDigit</i> :: <b>one of</b>
    <b>0</b> <b>1</b> <b>2</b> <b>3</b> <b>4</b> <b>5</b> <b>6</b> <b>7</b> <b>8</b> <b>9</b> <b>a</b> <b>b</b> <b>c</b> <b>d</b> <b>e</b> <b>f</b> <b>A</b> <b>B</b> <b>C</b> <b>D</b> <b>E</b> <b>F</b>

<i>BinDigit</i> :: <b>one of</b>
    <b>0</b>  <b>1</b>
</pre>

Constraints (enforced with diagnostics): no octal prefix; a single `_` may
separate digits or follow a prefix (`0x_ff`), but doubled `_` raises
`MULTIPLE_NUMERIC_SEPARATORS` and a digit-less prefix raises `INVALID_NUMBER`;
the big-integer suffix is uppercase `L` only; a fractional point needs a leading
digit (`0.` is `0.0`); hex floats require a binary `p`/`P` exponent; binary
literals admit neither fraction nor exponent. `1..10` lexes as
<i>IntLiteral</i> <b>..</b> <i>IntLiteral</i>, and `0.foo` as
<i>IntLiteral</i> <b>.</b> <i>Ident</i>.

### 3.4 Special numeric tokens

<pre>
<i>Lemniscate</i> ::
    <b>∞</b>        (* U+221E — positive infinity *)

<i>Pi</i> ::
    <b>π</b>        (* U+03C0 — the constant pi *)
</pre>

### 3.5 String literals

<pre>
<i>StringLiteral</i> ::
    <b>"</b> <i>DoubleStringChar</i>* <b>"</b>
    <b>'</b> <i>SingleStringChar</i>* <b>'</b>

<i>DoubleStringChar</i> ::
    <i>SourceCharacter</i> <b>but not one of</b> <b>"</b> <b>\</b> <i>LineTerminator</i>
    <b>\</b> <i>SourceCharacter</i>

<i>SingleStringChar</i> ::
    <i>SourceCharacter</i> <b>but not one of</b> <b>'</b> <b>\</b> <i>LineTerminator</i>
    <b>\</b> <i>SourceCharacter</i>
</pre>

Strings are single-line and delimited by `"…"` or `'…'`. A backslash escapes the
next character (including the delimiter); the lexer keeps the backslash and
escaped character **verbatim** — escape sequences are interpreted later. An
unterminated string raises `STRING_NOT_CLOSED` (the token is still emitted).

### 3.6 Comments

<pre>
<i>LineComment</i> ::
    <b>//</b> <i>LineCommentChar</i>*

<i>LineCommentChar</i> ::
    <i>SourceCharacter</i> <b>but not</b> <i>LineTerminator</i>

<i>BlockComment</i> ::
    <b>/*</b> <i>BlockCommentChar</i>* <b>*/</b>

<i>BlockCommentChar</i> ::
    <i>SourceCharacter</i> <b>but not</b> the sequence <b>*/</b>
</pre>

Block comments do **not** nest (first `*/` closes); an unterminated one extends
to `EOF`. In v1 only, `/*/` is a complete block comment. There is no distinct
doc-comment token.

### 3.7 Whitespace and line terminators

<pre>
<i>Whitespace</i> ::
    <i>WhitespaceChar</i>+

<i>WhitespaceChar</i> :: <b>one of</b>
    &lt;SP&gt;  &lt;TAB&gt;  &lt;LF&gt;  &lt;CR&gt;  &lt;NBSP&gt;

<i>LineTerminator</i> :: <b>one of</b>
    &lt;LF&gt;  &lt;CR&gt;
</pre>

Whitespace (line terminators included) is trivia. There is **no automatic
semicolon insertion** — a line terminator never terminates a statement.

### 3.8 Pragmas

<pre>
<i>Pragma</i> ::
    <b>//</b> <b>@</b> <i>PragmaBody</i>

<i>PragmaBody</i> ::
    <i>VersionPragma</i>
    <i>StrictPragma</i>
    <i>ExperimentalPragma</i>
    <i>UnknownPragma</i>

<i>VersionPragma</i> ::
    <b>version</b> <b>:</b> <i>Version</i>

<i>StrictPragma</i> ::
    <b>strict</b>

<i>ExperimentalPragma</i> ::
    <b>experimental</b> <b>:</b> <i>FeatureName</i>
</pre>

`// @version:N` selects the version; `// @strict` enables strict mode (no value);
`// @experimental:feature` opts into a feature (repeatable). Diagnostics:
`PRAGMA_DUPLICATE`, `PRAGMA_BAD_VERSION`, `PRAGMA_INVALID_VALUE`,
`PRAGMA_UNKNOWN`.

### 3.9 Punctuators and operators

Multi-character operators match greedily (`===` is one token).

<pre>
<i>Punctuator</i> :: <b>one of</b>
    <b>(</b> <b>)</b> <b>[</b> <b>]</b> <b>{</b> <b>}</b> <b>,</b> <b>;</b> <b>:</b> <b>.</b> <b>..</b>
    <b>=</b> <b>==</b> <b>===</b> <b>!=</b> <b>!==</b>
    <b>&lt;</b> <b>&lt;=</b> <b>&gt;</b> <b>&gt;=</b>
    <b>+</b> <b>-</b> <b>*</b> <b>/</b> <b>\</b> <b>%</b> <b>**</b>
    <b>++</b> <b>--</b>
    <b>~</b> <b>&amp;</b> <b>|</b> <b>^</b>
    <b>&lt;&lt;</b> <b>&gt;&gt;</b> <b>&gt;&gt;&gt;</b>
    <b>&amp;&amp;</b> <b>||</b>
    <b>!</b> <b>?</b> <b>??</b>
    <b>-&gt;</b> <b>=&gt;</b>
    <b>@</b>

<i>AssignmentOperator</i> :: <b>one of</b>
    <b>=</b> <b>+=</b> <b>-=</b> <b>*=</b> <b>/=</b> <b>\=</b> <b>%=</b> <b>**=</b>
    <b>&amp;=</b> <b>|=</b> <b>^=</b>
    <b>&lt;&lt;=</b> <b>&gt;&gt;=</b> <b>&gt;&gt;&gt;=</b>
    <b>??=</b>
</pre>

`\` is integer (floored) division; `>>>` is the unsigned right shift; `@` is both
the by-reference marker and the annotation marker.

---

## 4. Syntactic grammar — top level

<pre>
<i>SourceFile</i> :
    <i>TopLevelItem</i>*

<i>TopLevelItem</i> :
    <i>Annotation</i>* <i>FnDecl</i>
    <i>Annotation</i>* <i>ClassDecl</i>
    <i>Annotation</i>* <i>IncludeStmt</i>
    <i>Annotation</i>* <i>ImportStmt</i>
    <i>Annotation</i>* <i>Stmt</i>

<i>Annotation</i> :
    <b>@</b> <i>AnnotationName</i> <i>Arguments</i><sub>opt</sub>

<i>AnnotationName</i> :
    <i>Ident</i>
    <i>Keyword</i>

<i>Arguments</i> :
    <b>(</b> <i>ArgList</i><sub>opt</sub> <b>)</b>
</pre>

An annotation binds to the declaration that follows it. An *annotation*
`@name { … }` is told apart from an *expression statement* `@name(...);` by a
trailing `;`. Statements take an optional `;` (no automatic insertion; a missing
or extra terminator is tolerated).

---

## 5. Declarations

### 5.1 Variable declarations

<pre>
<i>VarDeclStmt</i> :
    <b>var</b>    <i>VarDeclarator</i>    ( <b>,</b> <i>VarDeclarator</i> )*    <b>;</b><sub>opt</sub>
    <i>Type</i>   <i>VarDeclarator</i>    ( <b>,</b> <i>VarDeclarator</i> )*    <b>;</b><sub>opt</sub>
    <b>global</b> <i>GlobalDeclarator</i> ( <b>,</b> <i>GlobalDeclarator</i> )* <b>;</b><sub>opt</sub>

<i>VarDeclarator</i> :
    <i>Ident</i> ( <b>=</b> <i>Expr</i> )<sub>opt</sub>

<i>GlobalDeclarator</i> :
    <i>Type</i><sub>opt</sub> <i>Ident</i> ( <b>=</b> <i>Expr</i> )<sub>opt</sub>
</pre>

The <i>Type</i>-prefixed alternative applies only when
[lookahead = <i>Type</i> <i>Ident</i> ( <b>=</b> | <b>;</b> | <b>,</b> | <b>}</b> |
<b>EOF</b> )]; an expression-continuation token after the identifier disqualifies
it (parsed as an expression statement instead). Untyped (`var x;`) and typed
(`integer x = 5;`) forms coexist; globals may mix typed and untyped declarators.

### 5.2 Function declarations

<pre>
<i>FnDecl</i> :
    <b>function</b> <i>Ident</i><sub>opt</sub> <i>TypeParams</i><sub>opt</sub> <i>ParamList</i> <i>ReturnType</i><sub>opt</sub> <i>FnBody</i>

<i>FnBody</i> :
    <i>Block</i>
    <b>;</b>                                  (* feature: function_signatures *)

<i>ParamList</i> :
    <b>(</b> ( <i>Param</i> ( <b>,</b> <i>Param</i> )* )<sub>opt</sub> <b>)</b>

<i>Param</i> :
    <b>@</b><sub>opt</sub> ( <i>Type</i> <b>@</b><sub>opt</sub> )<sub>opt</sub> <i>Ident</i> ( <b>=</b> <i>Expr</i> )<sub>opt</sub>

<i>ReturnType</i> :
    ( <b>-&gt;</b> | <b>=&gt;</b> ) <i>Type</i>

<i>TypeParams</i> :
    <b>&lt;</b> <i>Ident</i> ( <b>,</b> <i>Ident</i> )* <b>&gt;</b>          (* feature: generics *)
</pre>

`->` and `=>` are interchangeable. **By-reference** parameters use `@` (canonical
`integer @cell`; leading `@` also accepted). Parameters may have defaults. A
bodiless signature (`function f() -> T;`) needs the `function_signatures`
feature.

### 5.3 Class declarations

<pre>
<i>ClassDecl</i> :
    <b>class</b> <i>Ident</i> <i>TypeParams</i><sub>opt</sub> <i>ClassExtends</i><sub>opt</sub> <i>ClassImplements</i><sub>opt</sub> <i>ClassBody</i>

<i>ClassExtends</i> :
    <b>extends</b> <i>Ident</i> <i>TypeParams</i><sub>opt</sub>

<i>ClassImplements</i> :
    <b>implements</b> <i>Ident</i> ( <b>,</b> <i>Ident</i> )*       (* feature: interfaces *)

<i>ClassBody</i> :
    <b>{</b> <i>ClassMember</i>* <b>}</b>

<i>ClassMember</i> :
    <i>Annotation</i>* <i>ClassConstructor</i>
    <i>Annotation</i>* <i>ClassMethod</i>
    <i>Annotation</i>* <i>ClassField</i>

<i>ClassField</i> :
    <i>Modifier</i>* <i>Type</i> <i>Ident</i> ( <b>=</b> <i>Expr</i> )<sub>opt</sub> <b>;</b><sub>opt</sub>

<i>ClassMethod</i> :
    <i>Modifier</i>* <i>Type</i><sub>opt</sub> <i>Ident</i> <i>TypeParams</i><sub>opt</sub> <i>ParamList</i> <i>ReturnType</i><sub>opt</sub> <i>Block</i><sub>opt</sub>

<i>ClassConstructor</i> :
    <i>Modifier</i>* <b>constructor</b> <i>ParamList</i> <i>Block</i><sub>opt</sub>

<i>Modifier</i> : <b>one of</b>
    <b>public</b>  <b>private</b>  <b>protected</b>  <b>static</b>  <b>final</b>
</pre>

Single inheritance only. A member is a **method** when its name is followed by
`(` (or `<` under `generics`), else a **field**. Visibility defaults to public;
modifiers may appear in any order. (`final` is a keyword only in v3+.)

---

## 6. Statements

<pre>
<i>Stmt</i> :
    <i>Block</i>
    <i>VarDeclStmt</i>
    <i>IfStmt</i>
    <i>WhileStmt</i>
    <i>DoWhileStmt</i>
    <i>ForStmt</i>
    <i>ForeachStmt</i>
    <i>SwitchStmt</i>
    <i>BreakStmt</i>
    <i>ContinueStmt</i>
    <i>ReturnStmt</i>
    <i>IncludeStmt</i>
    <i>ImportStmt</i>
    <i>ExprStmt</i>
    <i>EmptyStmt</i>
</pre>

### 6.1 Blocks and trivial statements

<pre>
<i>Block</i> :
    <b>{</b> <i>Stmt</i>* <b>}</b>

<i>ExprStmt</i> :
    <i>Expr</i> <b>;</b><sub>opt</sub>

<i>EmptyStmt</i> :
    <b>;</b>
</pre>

### 6.2 Conditionals

<pre>
<i>IfStmt</i> :
    <b>if</b> <b>(</b> <i>Expr</i> <b>)</b> <i>Stmt</i> ( <b>else</b> <i>Stmt</i> )<sub>opt</sub>

<i>SwitchStmt</i> :
    <b>switch</b> <b>(</b> <i>Expr</i> <b>)</b> <b>{</b> <i>SwitchCase</i>* <b>}</b>

<i>SwitchCase</i> :
    <b>case</b> <i>Expr</i> <b>:</b> <i>Stmt</i>*
    <b>default</b> <b>:</b> <i>Stmt</i>*
</pre>

The dangling `else` binds to the nearest unmatched `if`. A `case` label takes an
arbitrary expression; fall-through is implicit.

### 6.3 Loops

<pre>
<i>WhileStmt</i> :
    <b>while</b> <b>(</b> <i>Expr</i> <b>)</b> <i>Stmt</i>

<i>DoWhileStmt</i> :
    <b>do</b> <i>Stmt</i> <b>while</b> <b>(</b> <i>Expr</i> <b>)</b> <b>;</b><sub>opt</sub>

<i>ForStmt</i> :
    <b>for</b> <b>(</b> <i>ForInit</i><sub>opt</sub> <b>;</b> <i>Expr</i><sub>opt</sub> <b>;</b> <i>Expr</i><sub>opt</sub> <b>)</b> <i>Stmt</i>

<i>ForInit</i> :
    <i>VarDeclStmt</i>
    <i>Expr</i>

<i>ForeachStmt</i> :
    <b>for</b> <b>(</b> <i>ForBinding</i> ( <b>:</b> <i>ForBinding</i> )<sub>opt</sub> <b>in</b> <i>Expr</i> <b>)</b> <i>Stmt</i>

<i>ForBinding</i> :
    <b>var</b><sub>opt</sub> <b>@</b><sub>opt</sub> <i>Type</i><sub>opt</sub> <i>Ident</i>
</pre>

`ForStmt` clauses are all optional (`for (;;)` loops forever). `ForeachStmt` has
a value form (`for (x in arr)`) and a key/value form (`for (k : v in map)`); each
binding independently allows `var`, `@`, and a type. The parser tells foreach
from the C-style loop by [lookahead = … <b>in</b> … before <b>;</b>].

### 6.4 Jumps

<pre>
<i>BreakStmt</i> :
    <b>break</b> <b>;</b><sub>opt</sub>

<i>ContinueStmt</i> :
    <b>continue</b> <b>;</b><sub>opt</sub>

<i>ReturnStmt</i> :
    <b>return</b> <b>?</b><sub>opt</sub> <i>Expr</i><sub>opt</sub> <b>;</b><sub>opt</sub>
</pre>

No labeled break/continue. The optional `?` after `return` is the **soft-return**
marker.

### 6.5 Module statements

<pre>
<i>IncludeStmt</i> :
    <b>include</b> <b>(</b> <i>StringLiteral</i> <b>)</b> <b>;</b><sub>opt</sub>

<i>ImportStmt</i> :
    <b>import</b> <b>(</b> <i>StringLiteral</i> <b>)</b> <b>;</b><sub>opt</sub>
    <b>import</b> <i>StringLiteral</i> <b>;</b><sub>opt</sub>
    <b>import</b> <i>DottedPath</i> <b>;</b><sub>opt</sub>

<i>DottedPath</i> :
    <i>Ident</i> ( <b>.</b> <i>Ident</i> )*
</pre>

---

## 7. Types

<pre>
<i>Type</i> :
    <i>TypeUnion</i> ( ( <b>-&gt;</b> | <b>=&gt;</b> ) <i>Type</i> )<sub>opt</sub>     (* trailing arrow: function type *)

<i>TypeUnion</i> :
    <i>TypeAtom</i> ( <b>|</b> <i>TypeAtom</i> )* <b>?</b>*

<i>TypeAtom</i> :
    <i>TypeName</i> <i>TypeArgs</i><sub>opt</sub>
    <b>Array</b> <i>TypeArgs</i><sub>opt</sub> ( <b>[</b> <i>TypeUnion</i> ( <b>,</b> <i>TypeUnion</i> )* <b>]</b> )<sub>opt</sub>
    ( <b>-&gt;</b> | <b>=&gt;</b> ) <i>Type</i>

<i>TypeArgs</i> :
    <b>&lt;</b> <i>Type</i> ( <b>,</b> <i>Type</i> )* <i>GenericClose</i>

<i>TypeName</i> :
    <i>PrimitiveType</i>
    <i>Ident</i>

<i>PrimitiveType</i> : <b>one of</b>
    <b>integer</b> <b>real</b> <b>big_integer</b> <b>string</b> <b>boolean</b>
    <b>void</b> <b>any</b> <b>null</b> <b>Array</b> <b>Map</b>
    <b>Set</b> <b>Interval</b> <b>Object</b> <b>Function</b> <b>Number</b> <b>Boolean</b>
</pre>

A trailing `?` marks **nullable** (multiple tolerated); `|` forms **unions**.
Because the lexer fuses consecutive `>` into `>>`/`>>>`, <i>GenericClose</i> is
the closing `>` the parser splits back out of a fused token. Under the `types`
feature, `Array[T, U]` gives per-position element types. `integer => string` is a
**function type**.

---

## 8. Expressions

Expressions are parsed by a **Pratt (precedence-climbing) parser**: each operator
carries a binding-power pair `(l_bp, r_bp)`, with `l_bp = r_bp − 1` for
left-associative and `l_bp = r_bp + 1` for right-associative operators.

### 8.1 Precedence table

Lowest to highest; each row binds tighter than those above it.

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

`**` (right-assoc.) binds tighter than prefix unary, so `-12 ** 2 = 144`. The
ternary `?` (`Question`) and coalescing `??` (`QuestionQuestion`) are distinct
tokens. For `instanceof` and `as` the right operand is a <i>Type</i>; `not in` is
the two-token sequence `not` then `in`.

### 8.2 Expression structure

The single <i>BinaryExpr</i> below collapses the precedence tiers of §8.1; in the
implementation each tier is its own precedence level with the stated
associativity.

<pre>
<i>Expr</i> :
    <i>AssignmentExpr</i>

<i>AssignmentExpr</i> :
    <i>ConditionalExpr</i> ( <i>AssignmentOperator</i> <i>AssignmentExpr</i> )<sub>opt</sub>

<i>ConditionalExpr</i> :
    <i>BinaryExpr</i> ( <b>?</b> <i>Expr</i> <b>:</b> <i>AssignmentExpr</i> )<sub>opt</sub>

<i>BinaryExpr</i> :
    <i>UnaryExpr</i> ( <i>BinaryOperator</i> <i>UnaryExpr</i> )*

<i>UnaryExpr</i> :
    <i>PrefixOperator</i> <i>UnaryExpr</i>
    <i>PostfixExpr</i>

<i>PrefixOperator</i> : <b>one of</b>
    <b>-</b> <b>+</b> <b>!</b> <b>~</b> <b>not</b> <b>++</b> <b>--</b> <b>@</b>

<i>PostfixExpr</i> :
    <i>PrimaryExpr</i> <i>PostfixOp</i>*

<i>PostfixOp</i> :
    <b>(</b> <i>ArgList</i><sub>opt</sub> <b>)</b>                              (* call *)
    <b>[</b> <i>Expr</i> <b>]</b>                                  (* index *)
    <b>[</b> <i>Expr</i><sub>opt</sub> <b>:</b> <i>Expr</i><sub>opt</sub> ( <b>:</b> <i>Expr</i><sub>opt</sub> )<sub>opt</sub> <b>]</b>  (* slice, v4+ *)
    <b>.</b>  ( <i>Ident</i> | <i>Keyword</i> )                      (* member *)
    <b>?.</b> ( <i>Ident</i> | <i>Keyword</i> )                      (* optional member *)
    <b>++</b>
    <b>--</b>
    <b>!</b>                                           (* non-null assertion *)
    <b>as</b> <i>Type</i>                                     (* cast *)

<i>ArgList</i> :
    <i>Expr</i> ( <b>,</b> <i>Expr</i> )*
</pre>

Member names after `.`/`?.` may be any identifier or keyword. The index form
applies only [lookahead: a matching `]` exists at the same depth]; the slice form
is v4+; `?.` applies only [lookahead = `?.` <i>Ident</i>-shaped], so `a ? .5 : b`
stays a ternary.

### 8.3 Primary expressions

<pre>
<i>PrimaryExpr</i> :
    <i>Literal</i>
    <i>NameRef</i>
    <b>(</b> <i>Expr</i> <b>)</b>
    <i>NewExpr</i>
    <i>Lambda</i>
    <i>CollectionLiteral</i>
    <i>IntervalExpr</i>

<i>NameRef</i> :
    <i>Ident</i>
    <b>this</b>
    <b>super</b>
    <b>class</b>

<i>NewExpr</i> :
    <b>new</b> <i>Ident</i> ( <b>(</b> <i>ArgList</i><sub>opt</sub> <b>)</b> )<sub>opt</sub>
</pre>

`new MyClass(a, b)` invokes a constructor; the argument list is optional.

### 8.4 Lambdas and anonymous functions

<pre>
<i>Lambda</i> :
    <i>Arrow</i> <i>LambdaBody</i>                              (* zero parameters *)
    <i>Ident</i> <i>Arrow</i> <i>LambdaBody</i>                        (* one bare parameter *)
    <i>Type</i> <i>Ident</i> <i>Arrow</i> <i>LambdaBody</i>                   (* one typed parameter *)
    <i>Ident</i> ( <b>,</b> <i>Ident</i> )+ <i>Arrow</i> <i>LambdaBody</i>         (* multiple bare parameters *)
    <b>(</b> <i>LambdaParams</i><sub>opt</sub> <b>)</b> <i>Arrow</i> <i>LambdaBody</i>        (* parenthesised parameters *)
    <b>(</b> <i>LambdaParams</i><sub>opt</sub> <i>Arrow</i> <i>LambdaBody</i> <b>)</b>        (* inner-arrow form *)
    <i>AnonFn</i>

<i>AnonFn</i> :
    <b>function</b> <i>Ident</i><sub>opt</sub> <i>ParamList</i> <i>ReturnType</i><sub>opt</sub> <i>Block</i>

<i>Arrow</i> : <b>one of</b>
    <b>-&gt;</b>  <b>=&gt;</b>

<i>LambdaBody</i> :
    <i>Type</i><sub>opt</sub> <i>Block</i>
    <i>Type</i><sub>opt</sub> <i>Expr</i>

<i>LambdaParams</i> :
    <i>LambdaParam</i> ( <b>,</b> <i>LambdaParam</i> )*

<i>LambdaParam</i> :
    <i>Type</i><sub>opt</sub> <i>Ident</i>
</pre>

`->` and `=>` are interchangeable. The multiple-bare-parameter form is only
recognised at statement/initialiser position. A body is a block or a single
expression, optionally preceded by a return type.

### 8.5 Collection literals

The `[ … ]`, `{ … }`, and legacy `< … >` delimiters are shared; the parser
discriminates by the first separator.

<pre>
<i>CollectionLiteral</i> :
    <i>ArrayLiteral</i>
    <i>MapLiteral</i>
    <i>ObjectLiteral</i>
    <i>SetLiteral</i>
    <i>MapAngle</i>
    <i>SetAngle</i>

<i>ArrayLiteral</i> :
    <b>[</b> <b>]</b>
    <b>[</b> <i>Expr</i> ( <b>,</b> <i>Expr</i> | <i>Expr</i> )* <b>]</b>

<i>MapLiteral</i> :
    <b>[</b> <b>:</b> <b>]</b>
    <b>[</b> <i>Expr</i> <b>:</b> <i>Expr</i> ( <b>,</b> <i>Expr</i> <b>:</b> <i>Expr</i> )* <b>]</b>

<i>ObjectLiteral</i> :
    <b>{</b> <b>}</b>
    <b>{</b> <i>Expr</i> <b>:</b> <i>Expr</i> ( <b>,</b><sub>opt</sub> <i>Expr</i> <b>:</b> <i>Expr</i> )* <b>}</b>

<i>SetLiteral</i> :
    <b>{</b> <i>SetElem</i> ( <b>,</b> <i>SetElem</i> )* <b>,</b><sub>opt</sub> <b>}</b>

<i>MapAngle</i> :
    <b>&lt;</b> <b>:</b> <b>&gt;</b>
    <b>&lt;</b> <i>Expr</i> <b>:</b> <i>Expr</i> ( <b>,</b> <i>Expr</i> <b>:</b> <i>Expr</i> )* <b>&gt;</b>

<i>SetAngle</i> :
    <b>&lt;</b> <b>&gt;</b>
    <b>&lt;</b> <i>SetElem</i> ( <b>,</b> <i>SetElem</i> )* <b>,</b><sub>opt</sub> <b>&gt;</b>

<i>SetElem</i> :
    <i>Expr</i> <b>..</b> <i>Expr</i>                 (* a..b expands to an inclusive range *)
    <i>Expr</i>
</pre>

`[` opens an array, a map (`[k: v]`, empty `[:]`), or an interval (§8.6),
depending on whether a top-level `:` or `..` precedes the matching `]`. `{` opens
an object when the first separator is `:`, else a set; object commas are
optional. `< … >` is the legacy map/set syntax (`<:>`, `<>` empty), and while
parsing it, `>` is the closer rather than greater-than. In v1, array elements may
be space-separated. A set element `a..b` expands to the inclusive integer set.

### 8.6 Interval literals

Bracket direction encodes inclusivity: `[` inclusive, `]` exclusive.

<pre>
<i>IntervalExpr</i> :
    <i>OpenBracket</i> <i>Expr</i><sub>opt</sub> <b>..</b> <i>Expr</i><sub>opt</sub> ( <b>:</b> <i>Expr</i><sub>opt</sub> )<sub>opt</sub> <i>CloseBracket</i>

<i>OpenBracket</i> : <b>one of</b>
    <b>[</b>  <b>]</b>            (* '[' inclusive start, ']' exclusive start *)

<i>CloseBracket</i> : <b>one of</b>
    <b>]</b>  <b>[</b>            (* ']' inclusive end,   '[' exclusive end *)
</pre>

A leading `[` is an interval only when [lookahead: a top-level `..` precedes the
matching close bracket]. The optional `:` step gives a stride.

```
[1..5]    [1..5[    ]1..5]    [..5]    [1..]    [1..10:2]    [..]
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

For any question the prose does not settle, the implementation is authoritative.

| Area | Source |
| ---- | ------ |
| Token kinds, versions, pragmas | `crates/frontend/leek-syntax/src/{kind,version,pragma,token,language}.rs` |
| Lexer (numbers, strings, idents, comments, operators) | `crates/frontend/leek-lexer/src/*.rs` |
| Expression grammar & precedence | `crates/frontend/leek-parser/src/grammar/expr/*.rs` |
| Statements & declarations | `crates/frontend/leek-parser/src/grammar/{stmt,decls,mod}.rs` |
| Type grammar | `crates/frontend/leek-parser/src/grammar/types.rs` |
| AST node kinds | `crates/frontend/leek-parser/src/ast.rs` |
| Parser driver / binding powers | `crates/frontend/leek-parser/src/parser.rs` |
