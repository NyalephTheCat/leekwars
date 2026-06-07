use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use leek_syntax::Version;
use rowan::GreenNode;

use super::prelude::*;

/// Memoized parses of the static stdlib / leekwars signature headers, keyed
/// by `(header tag, version)`. The headers never change, but the LSP builds a
/// fresh `Checker` per keystroke (when prelude seeding is enabled), so without
/// this each keystroke re-parsed both headers. The cached value is a cheap
/// Arc-backed `GreenNode` clone. Poison-safe so one panicking thread can't
/// wedge the cache for the rest of the process.
static PRELUDE_PARSE_CACHE: LazyLock<Mutex<HashMap<(u8, Version), GreenNode>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Parse a static prelude header once per `(tag, version)` and return a clone
/// of the cached green tree.
fn cached_prelude_parse(tag: u8, version: Version, src: &str) -> GreenNode {
    use leek_parser::{parse_with_features, ParseFeatures};
    let key = (tag, version);
    if let Some(g) = PRELUDE_PARSE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return g.clone();
    }
    let parsed = parse_with_features(
        src,
        leek_prelude::source_id(),
        version,
        ParseFeatures {
            function_signatures: true,
            generics: true,
        },
    );
    PRELUDE_PARSE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_insert(parsed.green)
        .clone()
}

impl Checker {
    /// Seed the function-signature maps from the embedded *typed*
    /// standard-library header (named params, declared returns, and the
    /// generic pass: `push<T>(Array<T>, T)`, `first<T>(Array<T>) -> T`).
    ///
    /// Only the signature maps are populated — no diagnostics are
    /// emitted and nothing enters the per-expression type table, since
    /// the library isn't part of the user's source. User declarations
    /// run afterwards and overwrite any same-named entry, so a user
    /// redefinition always wins.
    ///
    /// Gated by the caller on [`Options::experimental_prelude`] (the threaded
    /// prelude feature flag) or [`Options::seed_library`]; off by default so the
    /// corpus baseline and normal inference are unchanged.
    pub(crate) fn seed_library_signatures(&mut self) {
        // Standard library first, then the leek-wars game functions
        // (`getCell`, `getLife`, …) so in-game scripts infer their
        // declared returns too. Same-named entries: last wins, but the
        // two sets are effectively disjoint.
        self.seed_header(0, leek_prelude::STDLIB_SRC);
        self.seed_header(1, leek_prelude::LEEKWARS_SRC);
    }

    /// Parse one typed signature header and collect its top-level
    /// function signatures into the maps. No diagnostics, no type-table
    /// entries — the header isn't user source.
    fn seed_header(&mut self, tag: u8, src: &str) {
        use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
        use leek_syntax::SyntaxNode;
        let green = cached_prelude_parse(tag, self.version, src);
        let Some(lib) = AstSourceFile::cast(SyntaxNode::new_root(green)) else {
            return;
        };
        for child in lib.syntax().children() {
            if let Some(fn_decl) = FnDecl::cast(child) {
                self.collect_fn_signature(&fn_decl);
            }
        }
    }

    pub(crate) fn check_file(&mut self, file: &SourceFile) {
        // Pre-pass: collect every top-level function's param/return
        // types so call-site checks for Function<X => Y>-typed
        // parameters (IMPOSSIBLE_CAST detection) can resolve them.
        // Also record class → parent links so `super` types correctly.
        for child in file.syntax().children() {
            if let Some(fn_decl) = FnDecl::cast(child.clone()) {
                self.collect_fn_signature(&fn_decl);
            } else if let Some(cls) = ClassDecl::cast(child) {
                let (name, parent) = class_name_and_parent(&cls);
                let Some(name) = name else { continue };
                if let Some(parent) = parent {
                    self.class_parents.insert(name.clone(), parent);
                }
                self.collect_class_members(&cls, &name);
                self.collect_generic_class(&cls, &name);
            }
        }
        for child in file.syntax().children() {
            if let Some(fn_decl) = FnDecl::cast(child.clone()) {
                self.check_fn_body(&fn_decl);
            } else if let Some(cls) = ClassDecl::cast(child.clone()) {
                self.check_class(&cls);
            } else if let Some(stmt) = Stmt::cast(child) {
                self.check_stmt(&stmt);
            }
        }
    }

    pub(crate) fn collect_fn_signature(&mut self, decl: &FnDecl) {
        let Some(name) = decl
            .syntax()
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
        else {
            return;
        };
        let mut param_types = Vec::new();
        if let Some(params) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        {
            for p in params.children() {
                if p.kind() != SyntaxKind::Param {
                    continue;
                }
                let ty = p
                    .children()
                    .find(|n| n.kind() == SyntaxKind::TypeRef)
                    .map_or(Type::Any, |n| type_from_node(&n));
                param_types.push(ty);
            }
        }
        let ret = fn_return_type(decl.syntax()).unwrap_or_else(|| {
            // No `=> T` annotation. Inferring "does this body
            // return anything?" precisely needs a flow walk; for
            // the IMPOSSIBLE_CAST case we only need to know that a
            // body *with no return statement* yields null. Anything
            // else falls back to Any so we don't false-fire.
            if has_return_stmt(decl.syntax()) {
                Type::Any
            } else {
                Type::Null
            }
        });
        // Experimental: a generic function (`f<T>(…) -> T`) also records
        // a GenericSig so call sites resolve `T` against concrete args.
        let typarams = crate::ty::collect_type_params(decl.syntax());
        if typarams.is_empty() {
            // A non-generic (re)definition shadows any seeded generic
            // library signature of the same name, so the precise
            // user/redefined return type wins at call sites.
            self.user_fn_generic.remove(name.text());
        } else {
            use crate::generic::{GType, GenericSig};
            let mut gparams = Vec::new();
            if let Some(params) = decl
                .syntax()
                .children()
                .find(|n| n.kind() == SyntaxKind::ParamList)
            {
                for p in params.children().filter(|n| n.kind() == SyntaxKind::Param) {
                    let gt = p
                        .children()
                        .find(|n| n.kind() == SyntaxKind::TypeRef)
                        .map_or(GType::Concrete(Type::Any), |n| crate::ty::gtype_from_node(&n, &typarams));
                    gparams.push(gt);
                }
            }
            let gret = crate::ty::fn_return_type_node(decl.syntax()).map_or_else(|| GType::Concrete(ret.clone()), |n| crate::ty::gtype_from_node(&n, &typarams));
            self.user_fn_generic.insert(
                name.text().to_string(),
                GenericSig {
                    params: gparams,
                    ret: gret,
                },
            );
        }
        self.user_fn_param_types
            .insert(name.text().to_string(), param_types);
        self.user_fn_return_type
            .insert(name.text().to_string(), ret);
    }

    pub(crate) fn check_fn_body(&mut self, decl: &FnDecl) {
        self.push_function();
        self.declare_params_as_any(decl.syntax());
        let ret_ty = fn_return_type(decl.syntax());
        self.return_types.push(ret_ty);
        if let Some(body) = decl.syntax().children().find_map(Block::cast) {
            self.check_block(&body);
        }
        self.return_types.pop();
        self.pop_scope();
    }

    /// Record `class`'s declared field types and method return types
    /// for later member-access / call inference.
    pub(crate) fn collect_class_members(&mut self, decl: &ClassDecl, class: &str) {
        let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        else {
            return;
        };
        let mut fields: std::collections::HashMap<String, Type> = std::collections::HashMap::new();
        let mut methods: std::collections::HashMap<String, Type> = std::collections::HashMap::new();
        for member in body.children() {
            match member.kind() {
                SyntaxKind::ClassField => {
                    let name = member_decl_ident(&member);
                    let ty = member
                        .children()
                        .find(|n| n.kind() == SyntaxKind::TypeRef)
                        .map_or(Type::Any, |n| type_from_node(&n));
                    if let Some(name) = name {
                        fields.insert(name, ty);
                    }
                }
                SyntaxKind::ClassMethod => {
                    let name = member_decl_ident(&member);
                    // Methods use a leading return-type prefix
                    // (`string describe()`), with a trailing `-> T` as
                    // a fallback.
                    let ret = member
                        .children()
                        .find(|n| n.kind() == SyntaxKind::TypeRef)
                        .map(|n| type_from_node(&n))
                        .or_else(|| fn_return_type(&member));
                    if let Some(name) = name {
                        methods.insert(name, ret.unwrap_or(Type::Any));
                    }
                }
                _ => {}
            }
        }
        // LSP-only (`seed_library`): a field with no type annotation is
        // recorded as `Any`. Recover a useful type from the constructor's
        // `this.field = <typed-param | literal>` assignments so hover and
        // member access on untyped fields resolve. Gated so the
        // corpus/driver baseline is unchanged.
        if self.opts.seed_library {
            self.infer_fields_from_ctor(&body, &mut fields);
        }
        self.class_field_types.insert(class.to_string(), fields);
        self.class_method_returns.insert(class.to_string(), methods);
    }

    /// Upgrade still-`Any` field types by reading the constructor's
    /// `this.field = rhs` assignments: a right-hand side that names a
    /// typed constructor parameter (or is a literal) gives the field its
    /// type. Never overrides an explicit annotation. Best-effort: only
    /// direct `this.field = name/literal` forms are recognized.
    fn infer_fields_from_ctor(
        &self,
        body: &SyntaxNode,
        fields: &mut std::collections::HashMap<String, Type>,
    ) {
        let Some(ctor) = body
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassConstructor)
        else {
            return;
        };
        // Constructor parameter name → declared type.
        let mut params: std::collections::HashMap<String, Type> = std::collections::HashMap::new();
        if let Some(pl) = ctor
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        {
            for p in pl.children().filter(|n| n.kind() == SyntaxKind::Param) {
                let ty = p
                    .children()
                    .find(|n| n.kind() == SyntaxKind::TypeRef)
                    .map_or(Type::Any, |n| type_from_node(&n));
                if let Some(name) = member_decl_ident(&p) {
                    params.insert(name, ty);
                }
            }
        }
        for node in ctor.descendants() {
            let Some(bin) = BinaryExpr::cast(node) else {
                continue;
            };
            if bin.op().map(|t| t.kind()) != Some(SyntaxKind::Eq) {
                continue;
            }
            let Some(Expr::Field(lhs)) = bin.lhs() else {
                continue;
            };
            if !field_base_is_this(&lhs) {
                continue;
            }
            let Some(field) = lhs.field().map(|t| t.text().to_string()) else {
                continue;
            };
            // Only fill in fields we don't already have a real type for.
            if !matches!(fields.get(&field), Some(Type::Any)) {
                continue;
            }
            let ty = match bin.rhs() {
                Some(Expr::Name(nr)) => nr
                    .ident()
                    .and_then(|id| params.get(id.text()).cloned()),
                Some(Expr::Literal(lit)) => literal_type(&lit),
                _ => None,
            };
            if let Some(ty) = ty
                && !matches!(ty, Type::Any)
            {
                fields.insert(field, ty);
            }
        }
    }

    /// Collect generic metadata for `class C<T, …>` — field/method/ctor
    /// type patterns over the class's (and each method's own) type
    /// variables. No-op for a non-generic class (no `TypeParamList`).
    pub(crate) fn collect_generic_class(&mut self, decl: &ClassDecl, class: &str) {
        use crate::generic::{GType, GenericSig};
        let class_params = crate::ty::collect_type_params(decl.syntax());
        let (parent, parent_args) = extends_parent_args(decl.syntax(), &class_params);
        // Record an entry when the class is itself generic OR it passes
        // type arguments to its parent (`IntBox extends Box<integer>`) —
        // the latter still needs an entry so member resolution can walk
        // into the generic parent and re-map its type arguments. A plain
        // `class Child extends Parent` (no `<…>`) has no parent args and
        // isn't generic, so it gets no entry (corpus path unchanged) and
        // is order-independent.
        if class_params.is_empty() && parent_args.is_empty() {
            return;
        }
        let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        else {
            return;
        };
        // Build a GType pattern from a member's TypeRef over `vars`.
        let pat = |node: &leek_syntax::SyntaxNode,
                   vars: &std::collections::HashSet<String>|
         -> GType {
            node.children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
                .map_or(GType::Concrete(Type::Any), |n| crate::ty::gtype_from_node(&n, vars))
        };
        let mut info = super::GenericClassInfo {
            type_params: crate::ty::type_param_list_names(decl.syntax()),
            parent,
            parent_args,
            ..Default::default()
        };
        for member in body.children() {
            match member.kind() {
                SyntaxKind::ClassField => {
                    if let Some(name) = member_decl_ident(&member) {
                        info.fields.insert(name, pat(&member, &class_params));
                    }
                }
                SyntaxKind::ClassMethod => {
                    let Some(name) = member_decl_ident(&member) else {
                        continue;
                    };
                    // The method resolves over the class's type vars plus
                    // its own (`T get<U>(U key)`).
                    let mut vars = class_params.clone();
                    vars.extend(crate::ty::collect_type_params(&member));
                    let mut params = Vec::new();
                    if let Some(pl) = member
                        .children()
                        .find(|n| n.kind() == SyntaxKind::ParamList)
                    {
                        for p in pl.children().filter(|n| n.kind() == SyntaxKind::Param) {
                            params.push(pat(&p, &vars));
                        }
                    }
                    // Return: leading prefix TypeRef, else trailing `-> T`.
                    let ret = member
                        .children()
                        .find(|n| n.kind() == SyntaxKind::TypeRef)
                        .or_else(|| crate::ty::fn_return_type_node(&member))
                        .map_or(GType::Concrete(Type::Any), |n| crate::ty::gtype_from_node(&n, &vars));
                    info.methods.insert(name, GenericSig { params, ret });
                }
                SyntaxKind::ClassConstructor => {
                    if let Some(pl) = member
                        .children()
                        .find(|n| n.kind() == SyntaxKind::ParamList)
                    {
                        info.ctor_params = pl
                            .children()
                            .filter(|n| n.kind() == SyntaxKind::Param)
                            .map(|p| pat(&p, &class_params))
                            .collect();
                    }
                }
                _ => {}
            }
        }
        self.generic_classes.insert(class.to_string(), info);
    }

    pub(crate) fn check_class(&mut self, decl: &ClassDecl) {
        let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        else {
            return;
        };
        // Make `this` resolve to this class while we walk its members.
        let class_name = class_name_and_parent(decl).0;
        if let Some(name) = class_name.clone() {
            self.class_stack.push(name);
        }
        for member in body.children() {
            if member.kind() != SyntaxKind::ClassMethod
                && member.kind() != SyntaxKind::ClassConstructor
            {
                continue;
            }
            self.push_function();
            self.declare_params_as_any(&member);
            let ret_ty = fn_return_type(&member);
            self.return_types.push(ret_ty);
            if let Some(body) = member.children().find_map(Block::cast) {
                self.check_block(&body);
            }
            self.return_types.pop();
            self.pop_scope();
        }
        if class_name.is_some() {
            self.class_stack.pop();
        }
    }

    /// Declare every `Param` under the given function/method node with
    /// its declared type (`Type::Any` when unannotated). Honoring the
    /// annotation feeds member-access inference and `!= null` narrowing.
    pub(crate) fn declare_params_as_any(&mut self, fn_node: &SyntaxNode) {
        let Some(params) = fn_node
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
        else {
            return;
        };
        for p in params.children() {
            if p.kind() != SyntaxKind::Param {
                continue;
            }
            let ty = p
                .children()
                .find(|n| n.kind() == SyntaxKind::TypeRef)
                .map_or(Type::Any, |n| type_from_node(&n));
            if let Some(ident) = p
                .children_with_tokens()
                .filter_map(rowan::NodeOrToken::into_token)
                .find(|t| t.kind() == SyntaxKind::Ident)
            {
                self.declare(ident.text(), ty);
            }
        }
    }

    pub(crate) fn check_block(&mut self, block: &Block) {
        self.push_scope();
        for s in block.stmts() {
            self.check_stmt(&s);
        }
        self.pop_scope();
    }
}

/// The declared name of a class member — the first `Ident` token child
/// (any leading field/return type sits inside a `TypeRef` node, so it
/// isn't a direct token and won't be mistaken for the name).
fn member_decl_ident(member: &SyntaxNode) -> Option<String> {
    member
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// True when a field-access receiver is the `this` keyword.
fn field_base_is_this(f: &leek_parser::ast::FieldExpr) -> bool {
    f.base()
        .and_then(|b| b.syntax().first_token())
        .is_some_and(|t| t.kind() == SyntaxKind::KwThis)
}

/// The type of a literal expression — mirrors the checker's literal
/// inference, used to type a field from `this.field = <literal>`.
fn literal_type(lit: &leek_parser::ast::LiteralExpr) -> Option<Type> {
    let tok = lit.token()?;
    Some(match tok.kind() {
        SyntaxKind::IntLiteral => Type::Integer,
        SyntaxKind::RealLiteral => Type::Real,
        SyntaxKind::StringLiteral => Type::String,
        SyntaxKind::KwTrue | SyntaxKind::KwFalse => Type::Boolean,
        _ => return None,
    })
}

/// Extract `(class name, parent name)` from a `class Name extends
/// Parent` declaration. The name is the first `Ident`; the parent is
/// the `Ident` that follows the `extends` keyword (if any).
fn class_name_and_parent(decl: &ClassDecl) -> (Option<String>, Option<String>) {
    let name = decl
        .syntax()
        .children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string());
    let mut saw_extends = false;
    let mut parent = None;
    for el in decl.syntax().children_with_tokens() {
        if let NodeOrToken::Token(t) = el {
            if t.kind() == SyntaxKind::KwExtends {
                saw_extends = true;
            } else if saw_extends && t.kind() == SyntaxKind::Ident {
                parent = Some(t.text().to_string());
                break;
            }
        }
    }
    (name, parent)
}

/// The `extends Parent<args…>` parent name and its type arguments as
/// [`GType`](crate::generic::GType) patterns over the *child's* type
/// variables (`class_params`). The args live in the `TypeParamList` that
/// follows the `extends` Ident (the experimental generic syntax records
/// `extends Container<T>`'s `<T>` as a flat token list); each top-level
/// identifier becomes a `Var` when it names a child type parameter, else
/// a concrete class/primitive leaf. `None` parent ⇒ no args.
fn extends_parent_args(
    decl: &SyntaxNode,
    class_params: &std::collections::HashSet<String>,
) -> (Option<String>, Vec<crate::generic::GType>) {
    use crate::generic::GType;
    let mut saw_extends = false;
    let mut parent: Option<String> = None;
    for el in decl.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) => {
                if t.kind() == SyntaxKind::KwExtends {
                    saw_extends = true;
                } else if saw_extends && parent.is_none() && t.kind() == SyntaxKind::Ident {
                    parent = Some(t.text().to_string());
                }
            }
            NodeOrToken::Node(n) => {
                // The first TypeParamList after the extends-Ident holds the
                // parent's type arguments.
                if saw_extends && parent.is_some() && n.kind() == SyntaxKind::TypeParamList {
                    let args = n
                        .children_with_tokens()
                        .filter_map(rowan::NodeOrToken::into_token)
                        .filter(|t| t.kind() == SyntaxKind::Ident)
                        .map(|t| {
                            let name = t.text();
                            if class_params.contains(name) {
                                GType::Var(name.to_string())
                            } else {
                                GType::Concrete(crate::ty::type_from_name(name))
                            }
                        })
                        .collect();
                    return (parent, args);
                }
            }
        }
    }
    (parent, Vec::new())
}
