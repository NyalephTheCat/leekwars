//! Class-declaration walk and inheritance-chain helpers.
//!
//! `declare_class` is the resolver's first-pass entry point for a
//! class — it registers the class name, records inheritance, and
//! sweeps members to populate the various per-class metadata maps
//! the later checks (privacy, finality, arity) consult.

use std::collections::{HashMap, HashSet};

use leek_parser::ast::{AstNode, ClassDecl, ClassField, ClassMethod};
use leek_syntax::{SyntaxKind, Version};

use crate::Resolver;
use crate::codes;
use crate::scope::SymbolKind;
use crate::util::{
    INTRINSIC_FINAL_CLASS_FIELDS, field_name, first_ident_after, fn_arity, method_name,
    ranges_overlap,
};

impl Resolver {
    pub(crate) fn declare_class(&mut self, decl: &ClassDecl) {
        let Some(name) = first_ident_after(decl.syntax(), SyntaxKind::KwClass) else {
            return;
        };
        let nm = name.text().to_string();
        // Built-in class names became reserved in v4 — declaring
        // `class Map` / `class Array` / etc. is a hard error there.
        // v2-v3 still allowed user-defined classes with these names.
        if self.version >= Version::V4
            && matches!(
                nm.as_str(),
                "Array"
                    | "Map"
                    | "Set"
                    | "Object"
                    | "Class"
                    | "Function"
                    | "String"
                    | "Number"
                    | "Integer"
                    | "Real"
                    | "Boolean"
                    | "Null"
                    | "JSON"
                    | "Value",
            )
        {
            self.err(
                codes::VARIABLE_NAME_UNAVAILABLE,
                self.span_of(&name),
                format!("`{nm}` is a reserved class name"),
            );
        }
        let (_, redecl) = self.declare(&name, SymbolKind::Class);
        if redecl {
            self.err(
                codes::REDECLARED_SYMBOL,
                self.span_of(&name),
                format!("`{nm}` is already declared"),
            );
        }
        // Track inheritance — `class B extends A {…}` means B carries
        // A's (and A's parents') members. We don't unify them yet, so
        // record a "has unknown parent" flag and skip strict member
        // checks on B.
        if let Some(parent_tok) = first_ident_after(decl.syntax(), SyntaxKind::KwExtends) {
            self.class_has_unknown_parent.insert(nm.clone());
            self.class_parent
                .insert(nm.clone(), parent_tok.text().to_string());
        }
        let collected = self.collect_class_members(decl, &nm);
        self.commit_class_members(&nm, collected);
    }

    /// Sweep the class body once, gathering all per-modifier name
    /// sets. Mutating side-effects (registering ctor visibility,
    /// emitting DUPLICATED_METHOD, calling `check_param_defaults`)
    /// happen during the walk; the returned struct collects what
    /// gets committed to `Resolver` state afterwards.
    fn collect_class_members(&mut self, decl: &ClassDecl, class_name: &str) -> CollectedMembers {
        let mut c = CollectedMembers::default();
        let Some(body) = decl
            .syntax()
            .children()
            .find(|n| n.kind() == SyntaxKind::ClassBody)
        else {
            return c;
        };
        for member in body.children() {
            let modifiers = collect_modifiers(&member);
            let is_private = modifiers.contains(&"private");
            let is_protected = modifiers.contains(&"protected");
            let is_final = modifiers.contains(&"final");
            let is_static = modifiers.contains(&"static");
            match member.kind() {
                SyntaxKind::ClassField => {
                    if let Some(field) = ClassField::cast(member.clone())
                        && let Some(ident) = field_name(&field)
                    {
                        let n = ident.text().to_string();
                        c.all_fields.insert(n.clone());
                        if is_static {
                            c.static_members.insert(n.clone());
                            if is_final {
                                c.static_finals.insert(n.clone());
                            }
                        } else if is_final {
                            c.finals.insert(n.clone());
                        }
                        if is_private {
                            c.private_fields.insert(n.clone());
                        } else if is_protected {
                            c.protected_fields.insert(n);
                        }
                    }
                }
                SyntaxKind::ClassConstructor => {
                    if is_private {
                        self.class_private_constructor.insert(class_name.into());
                    } else if is_protected {
                        self.class_protected_constructor.insert(class_name.into());
                    }
                    // v4: overlapping-arity constructors are a hard
                    // error (matches the DUPLICATED_METHOD rule for
                    // regular methods).
                    let (lo, hi) = fn_arity(&member);
                    if self.version == Version::V4
                        && c.ctor_arities
                            .iter()
                            .any(|(elo, ehi)| ranges_overlap(*elo, *ehi, lo, hi))
                    {
                        self.err(
                            codes::DUPLICATED_CONSTRUCTOR,
                            self.node_span(&member),
                            "constructor with overlapping arity already defined".to_string(),
                        );
                    }
                    c.ctor_arities.push((lo, hi));
                }
                SyntaxKind::ClassMethod => {
                    if let Some(m) = ClassMethod::cast(member.clone())
                        && let Some(ident) = method_name(&m)
                    {
                        let n = ident.text().to_string();
                        // Methods count as "members" for the `this.x`
                        // existence check — `this.m()` should not
                        // error just because `m` is a method rather
                        // than a field.
                        c.all_fields.insert(n.clone());
                        c.all_methods.insert(n.clone());
                        if is_static {
                            c.static_members.insert(n.clone());
                        }
                        if is_private {
                            if is_static {
                                c.private_static_methods.insert(n.clone());
                            } else {
                                c.private_methods.insert(n.clone());
                            }
                        } else if is_protected {
                            if is_static {
                                c.protected_static_methods.insert(n.clone());
                            } else {
                                c.protected_methods.insert(n.clone());
                            }
                        }
                        self.check_param_defaults(m.syntax());
                        // At v4, two methods with the same name and
                        // overlapping arity ranges is a hard error.
                        // Earlier versions accept the overload-by-
                        // arity pattern silently.
                        let (lo, hi) = fn_arity(m.syntax());
                        if self.version == Version::V4
                            && let Some(existing) = c.method_overloads.get(&n)
                            && existing
                                .iter()
                                .any(|(elo, ehi)| ranges_overlap(*elo, *ehi, lo, hi))
                        {
                            self.err(
                                codes::DUPLICATED_METHOD,
                                self.span_of(&ident),
                                format!(
                                    "method `{n}` is already defined with an overlapping arity",
                                ),
                            );
                        }
                        c.method_overloads
                            .entry(n.clone())
                            .or_default()
                            .push((lo, hi));
                        // Methods can be overloaded by arity in
                        // Leekscript — merge each overload into a
                        // `(min, max)` envelope so call-site checks
                        // accept any of them.
                        c.method_arities
                            .entry(n)
                            .and_modify(|(cur_lo, cur_hi)| {
                                *cur_lo = (*cur_lo).min(lo);
                                *cur_hi = (*cur_hi).max(hi);
                            })
                            .or_insert((lo, hi));
                    }
                }
                _ => {}
            }
        }
        c
    }

    fn commit_class_members(&mut self, nm: &str, c: CollectedMembers) {
        if !c.finals.is_empty() {
            self.class_final_fields.insert(nm.into(), c.finals);
        }
        if !c.static_finals.is_empty() {
            self.class_static_final_fields
                .insert(nm.into(), c.static_finals);
        }
        // Always record the field set — even an empty class needs
        // the entry so `instance.unknown_field` errors instead of
        // silently passing through the "I don't know" path.
        self.class_fields_all.insert(nm.into(), c.all_fields);
        if !c.static_members.is_empty() {
            self.class_static_members
                .insert(nm.into(), c.static_members);
        }
        if !c.private_fields.is_empty() {
            self.class_private_fields
                .insert(nm.into(), c.private_fields);
        }
        if !c.protected_fields.is_empty() {
            self.class_protected_fields
                .insert(nm.into(), c.protected_fields);
        }
        if !c.private_methods.is_empty() {
            self.class_private_methods
                .insert(nm.into(), c.private_methods);
        }
        if !c.protected_methods.is_empty() {
            self.class_protected_methods
                .insert(nm.into(), c.protected_methods);
        }
        if !c.private_static_methods.is_empty() {
            self.class_private_static_methods
                .insert(nm.into(), c.private_static_methods);
        }
        if !c.protected_static_methods.is_empty() {
            self.class_protected_static_methods
                .insert(nm.into(), c.protected_static_methods);
        }
        if !c.method_arities.is_empty() {
            self.class_method_arities
                .insert(nm.into(), c.method_arities);
        }
        // Note: we deliberately don't track `all_methods` separately —
        // calling an unknown method on a class instance fails at
        // runtime in Leekscript and the upstream emits a different
        // diagnostic that's tricky to discriminate from PRIVATE_METHOD.
        let _ = c.all_methods;
    }

    // ---- Inheritance-chain walkers ----

    /// Whether `field` on `ClassName` is immutable — either a user
    /// `static final` declaration or one of the intrinsic class
    /// metadata fields (`name`, `fields`, …) that are always final.
    pub(crate) fn is_final_class_member(&self, class_name: &str, field: &str) -> bool {
        if INTRINSIC_FINAL_CLASS_FIELDS.contains(&field) {
            return true;
        }
        self.class_static_final_fields
            .get(class_name)
            .is_some_and(|s| s.contains(field))
    }

    /// Walk the class's inheritance chain and return the first
    /// ancestor (including `start`) that declares `field` as
    /// private. Returns the ancestor's name, or `None` if no
    /// ancestor restricts the field.
    pub(crate) fn lookup_private_owner(&self, start: &str, field: &str) -> Option<String> {
        self.walk_class_chain(start, |c| {
            self.class_private_fields
                .get(c)
                .is_some_and(|s| s.contains(field))
        })
    }

    pub(crate) fn lookup_protected_owner(&self, start: &str, field: &str) -> Option<String> {
        self.walk_class_chain(start, |c| {
            self.class_protected_fields
                .get(c)
                .is_some_and(|s| s.contains(field))
        })
    }

    pub(crate) fn lookup_private_method_owner(&self, start: &str, method: &str) -> Option<String> {
        self.walk_class_chain(start, |c| {
            self.class_private_methods
                .get(c)
                .is_some_and(|s| s.contains(method))
        })
    }

    pub(crate) fn lookup_protected_method_owner(
        &self,
        start: &str,
        method: &str,
    ) -> Option<String> {
        self.walk_class_chain(start, |c| {
            self.class_protected_methods
                .get(c)
                .is_some_and(|s| s.contains(method))
        })
    }

    pub(crate) fn walk_class_chain<P>(&self, start: &str, mut pred: P) -> Option<String>
    where
        P: FnMut(&str) -> bool,
    {
        let mut visited = HashSet::new();
        let mut cur = start.to_string();
        loop {
            if !visited.insert(cur.clone()) {
                return None;
            }
            if pred(&cur) {
                return Some(cur);
            }
            match self.class_parent.get(&cur) {
                Some(p) => cur.clone_from(p),
                None => return None,
            }
        }
    }
}

/// Per-class member counts gathered during the first-pass walk, used
/// as a payload between [`Resolver::collect_class_members`] and
/// [`Resolver::commit_class_members`].
#[derive(Default)]
struct CollectedMembers {
    finals: HashSet<String>,
    static_finals: HashSet<String>,
    all_fields: HashSet<String>,
    static_members: HashSet<String>,
    all_methods: HashSet<String>,
    private_fields: HashSet<String>,
    protected_fields: HashSet<String>,
    private_methods: HashSet<String>,
    protected_methods: HashSet<String>,
    private_static_methods: HashSet<String>,
    protected_static_methods: HashSet<String>,
    method_arities: HashMap<String, (u8, u8)>,
    method_overloads: HashMap<String, Vec<(u8, u8)>>,
    ctor_arities: Vec<(u8, u8)>,
}

/// Collect the modifier keywords on a class member. The parser
/// represents these as keyword tokens at v3+ but as `Ident` tokens
/// at v1/v2 (where `private`, `public`, etc. weren't reserved), so
/// we accept both shapes.
fn collect_modifiers(member: &leek_syntax::SyntaxNode) -> Vec<&'static str> {
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
