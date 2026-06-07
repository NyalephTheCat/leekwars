//! Typed AST view over the CST.
//!
//! Each AST type is a newtype around [`SyntaxNode`]. They are cheap
//! to copy (`Clone`), they don't own anything, and they let callers
//! access children without filtering trivia by hand.
//!
//! The pattern mirrors rust-analyzer: define a `cast(SyntaxNode) ->
//! Option<Self>` constructor that succeeds only for the right kind,
//! plus accessor methods that return more AST types or `SyntaxToken`s.

use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

/// Trait every AST node implements. `cast` is the polymorphic
/// construction primitive used by enum AST types like [`Stmt`] and
/// [`Expr`].
pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

macro_rules! ast_node {
    ($name:ident = $kind:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(pub SyntaxNode);

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                (node.kind() == $kind).then_some(Self(node))
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(SourceFile = S::SourceFile);
ast_node!(Block = S::Block);
ast_node!(VarDeclStmt = S::VarDeclStmt);
ast_node!(ReturnStmt = S::ReturnStmt);
ast_node!(ExprStmt = S::ExprStmt);
ast_node!(IfStmt = S::IfStmt);
ast_node!(WhileStmt = S::WhileStmt);
ast_node!(BreakStmt = S::BreakStmt);
ast_node!(ContinueStmt = S::ContinueStmt);
ast_node!(LiteralExpr = S::LiteralExpr);
ast_node!(NameRef = S::NameRef);
ast_node!(BinaryExpr = S::BinaryExpr);
ast_node!(UnaryExpr = S::UnaryExpr);
ast_node!(ParenExpr = S::ParenExpr);
ast_node!(CallExpr = S::CallExpr);
ast_node!(ArgList = S::ArgList);
ast_node!(ArrayExpr = S::ArrayExpr);
ast_node!(IndexExpr = S::IndexExpr);
ast_node!(FieldExpr = S::FieldExpr);
ast_node!(MapExpr = S::MapExpr);
ast_node!(ObjectExpr = S::ObjectExpr);
ast_node!(SetExpr = S::SetExpr);
ast_node!(LambdaExpr = S::LambdaExpr);
ast_node!(NewExpr = S::NewExpr);
ast_node!(CastExpr = S::CastExpr);
ast_node!(TernaryExpr = S::TernaryExpr);
ast_node!(PostfixExpr = S::PostfixExpr);
ast_node!(IntervalExpr = S::IntervalExpr);
ast_node!(SliceExpr = S::SliceExpr);
ast_node!(Annotation = S::Annotation);
ast_node!(FnDecl = S::FnDecl);
ast_node!(ParamList = S::ParamList);
ast_node!(Param = S::Param);
ast_node!(IncludeStmt = S::IncludeStmt);
ast_node!(ImportStmt = S::ImportStmt);
ast_node!(ClassDecl = S::ClassDecl);
ast_node!(ClassBody = S::ClassBody);
ast_node!(ClassField = S::ClassField);
ast_node!(ClassMethod = S::ClassMethod);
ast_node!(ClassConstructor = S::ClassConstructor);
ast_node!(ForStmt = S::ForStmt);
ast_node!(ForeachStmt = S::ForeachStmt);
ast_node!(DoWhileStmt = S::DoWhileStmt);
ast_node!(SwitchStmt = S::SwitchStmt);
ast_node!(SwitchCase = S::SwitchCase);
ast_node!(TypeRef = S::TypeRef);

// ---- Enum wrappers ----

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Stmt {
    VarDecl(VarDeclStmt),
    Return(ReturnStmt),
    Expr(ExprStmt),
    If(IfStmt),
    While(WhileStmt),
    DoWhile(DoWhileStmt),
    For(ForStmt),
    Foreach(ForeachStmt),
    Switch(SwitchStmt),
    Break(BreakStmt),
    Continue(ContinueStmt),
    Block(Block),
    Include(IncludeStmt),
    Import(ImportStmt),
}

impl AstNode for Stmt {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            S::VarDeclStmt => VarDeclStmt::cast(node).map(Self::VarDecl),
            S::ReturnStmt => ReturnStmt::cast(node).map(Self::Return),
            S::ExprStmt => ExprStmt::cast(node).map(Self::Expr),
            S::IfStmt => IfStmt::cast(node).map(Self::If),
            S::WhileStmt => WhileStmt::cast(node).map(Self::While),
            S::DoWhileStmt => DoWhileStmt::cast(node).map(Self::DoWhile),
            S::ForStmt => ForStmt::cast(node).map(Self::For),
            S::ForeachStmt => ForeachStmt::cast(node).map(Self::Foreach),
            S::SwitchStmt => SwitchStmt::cast(node).map(Self::Switch),
            S::BreakStmt => BreakStmt::cast(node).map(Self::Break),
            S::ContinueStmt => ContinueStmt::cast(node).map(Self::Continue),
            S::Block => Block::cast(node).map(Self::Block),
            S::IncludeStmt => IncludeStmt::cast(node).map(Self::Include),
            S::ImportStmt => ImportStmt::cast(node).map(Self::Import),
            _ => None,
        }
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::VarDecl(n) => n.syntax(),
            Self::Return(n) => n.syntax(),
            Self::Expr(n) => n.syntax(),
            Self::If(n) => n.syntax(),
            Self::While(n) => n.syntax(),
            Self::DoWhile(n) => n.syntax(),
            Self::For(n) => n.syntax(),
            Self::Foreach(n) => n.syntax(),
            Self::Switch(n) => n.syntax(),
            Self::Break(n) => n.syntax(),
            Self::Continue(n) => n.syntax(),
            Self::Block(n) => n.syntax(),
            Self::Include(n) => n.syntax(),
            Self::Import(n) => n.syntax(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Literal(LiteralExpr),
    Name(NameRef),
    Binary(BinaryExpr),
    Unary(UnaryExpr),
    Paren(ParenExpr),
    Call(CallExpr),
    Array(ArrayExpr),
    Index(IndexExpr),
    Field(FieldExpr),
    Map(MapExpr),
    Object(ObjectExpr),
    Set(SetExpr),
    Lambda(LambdaExpr),
    New(NewExpr),
    Cast(CastExpr),
    Ternary(TernaryExpr),
    Postfix(PostfixExpr),
    Interval(IntervalExpr),
    Slice(SliceExpr),
}

impl AstNode for Expr {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            S::LiteralExpr => LiteralExpr::cast(node).map(Self::Literal),
            S::NameRef => NameRef::cast(node).map(Self::Name),
            S::BinaryExpr => BinaryExpr::cast(node).map(Self::Binary),
            S::UnaryExpr => UnaryExpr::cast(node).map(Self::Unary),
            S::ParenExpr => ParenExpr::cast(node).map(Self::Paren),
            S::CallExpr => CallExpr::cast(node).map(Self::Call),
            S::ArrayExpr => ArrayExpr::cast(node).map(Self::Array),
            S::IndexExpr => IndexExpr::cast(node).map(Self::Index),
            S::FieldExpr => FieldExpr::cast(node).map(Self::Field),
            S::MapExpr => MapExpr::cast(node).map(Self::Map),
            S::ObjectExpr => ObjectExpr::cast(node).map(Self::Object),
            S::SetExpr => SetExpr::cast(node).map(Self::Set),
            S::LambdaExpr => LambdaExpr::cast(node).map(Self::Lambda),
            S::NewExpr => NewExpr::cast(node).map(Self::New),
            S::CastExpr => CastExpr::cast(node).map(Self::Cast),
            S::TernaryExpr => TernaryExpr::cast(node).map(Self::Ternary),
            S::PostfixExpr => PostfixExpr::cast(node).map(Self::Postfix),
            S::IntervalExpr => IntervalExpr::cast(node).map(Self::Interval),
            S::SliceExpr => SliceExpr::cast(node).map(Self::Slice),
            _ => None,
        }
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Literal(n) => n.syntax(),
            Self::Name(n) => n.syntax(),
            Self::Binary(n) => n.syntax(),
            Self::Unary(n) => n.syntax(),
            Self::Paren(n) => n.syntax(),
            Self::Call(n) => n.syntax(),
            Self::Array(n) => n.syntax(),
            Self::Index(n) => n.syntax(),
            Self::Field(n) => n.syntax(),
            Self::Map(n) => n.syntax(),
            Self::Object(n) => n.syntax(),
            Self::Set(n) => n.syntax(),
            Self::Lambda(n) => n.syntax(),
            Self::New(n) => n.syntax(),
            Self::Cast(n) => n.syntax(),
            Self::Ternary(n) => n.syntax(),
            Self::Postfix(n) => n.syntax(),
            Self::Interval(n) => n.syntax(),
            Self::Slice(n) => n.syntax(),
        }
    }
}

// ---- Accessors ----

impl SourceFile {
    /// Iterator over the top-level statements in source order.
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        self.0.children().filter_map(Stmt::cast)
    }
}

impl VarDeclStmt {
    /// First declarator's name, if present. Multi-declarator support
    /// (the comma form) lands when we need it.
    pub fn name(&self) -> Option<SyntaxToken> {
        first_token(&self.0, S::Ident)
    }

    /// Initializer expression (the RHS of `=`), if present.
    pub fn init(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl LiteralExpr {
    /// The literal token itself. Inspect `.kind()` to determine
    /// whether it's an int, real, string, true, false, or null.
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
    }
}

impl NameRef {
    pub fn ident(&self) -> Option<SyntaxToken> {
        first_token(&self.0, S::Ident)
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    pub fn rhs(&self) -> Option<Expr> {
        let mut exprs = self.0.children().filter_map(Expr::cast);
        exprs.nth(1)
    }

    /// The operator token (e.g. `Plus`, `EqEq`). It is the only
    /// non-trivia token child of a `BinaryExpr` — operands are wrapped
    /// in `LiteralExpr`/`BinaryExpr`/etc. nodes, never bare tokens.
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
    }
}

impl UnaryExpr {
    pub fn op(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
    }

    pub fn operand(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
}

impl CallExpr {
    /// The expression being called (everything before the `ArgList`).
    pub fn callee(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    pub fn arg_list(&self) -> Option<ArgList> {
        self.0.children().find_map(ArgList::cast)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        self.0.children().filter_map(Expr::cast)
    }
}

impl ArrayExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> + '_ {
        self.0.children().filter_map(Expr::cast)
    }
}

impl IndexExpr {
    /// The expression being indexed.
    pub fn base(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
    /// The index expression.
    pub fn index(&self) -> Option<Expr> {
        self.0.children().filter_map(Expr::cast).nth(1)
    }
}

impl FieldExpr {
    /// The base expression (`a` in `a.b`).
    pub fn base(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
    /// The field name (`b` in `a.b`). Accepts the `class`
    /// keyword too, since `x.class` is the reflective access form
    /// that returns the value's runtime class.
    pub fn field(&self) -> Option<SyntaxToken> {
        // `KwSuper` is permitted here so `A.super` (the reflective
        // form that returns the parent class) parses as a field
        // access rather than a syntax error.
        self.0
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| matches!(t.kind(), S::Ident | S::KwClass | S::KwSuper))
    }
}

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        self.0.children().filter_map(Stmt::cast)
    }
}

impl IfStmt {
    /// The condition expression.
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }

    /// Then-branch statement.
    pub fn then_branch(&self) -> Option<Stmt> {
        self.0.children().find_map(Stmt::cast)
    }

    /// Else-branch, if present. We walk for an `else` keyword token
    /// and return the first statement after it.
    pub fn else_branch(&self) -> Option<Stmt> {
        let mut seen_else = false;
        for child in self.0.children_with_tokens() {
            match child {
                leek_syntax::SyntaxElement::Token(t) if t.kind() == S::KwElse => {
                    seen_else = true;
                }
                leek_syntax::SyntaxElement::Node(n) if seen_else => {
                    if let Some(s) = Stmt::cast(n) {
                        return Some(s);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        self.0.children().find_map(Expr::cast)
    }
    pub fn body(&self) -> Option<Stmt> {
        self.0.children().find_map(Stmt::cast)
    }
}

// ---- Helpers ----

fn first_token(node: &SyntaxNode, kind: S) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == kind)
}
