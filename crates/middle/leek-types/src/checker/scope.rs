//! Lexical scopes for local type bindings.

use std::collections::{HashMap, HashSet};

use crate::ty::Type;

/// Which lexical region a scope stands for. The kind is what decides
/// whether [`lookup`](crate::checker::Checker::lookup) may keep
/// walking outwards when a name misses.
///
/// The taxonomy mirrors HIR lowering's `boundaries` marker, so the two
/// passes agree on what a name inside a closure refers to:
///
/// - [`Block`](ScopeKind::Block) and [`Lambda`](ScopeKind::Lambda) are
///   *transparent* — a lambda body reads the enclosing function's
///   locals as captures, exactly as HIR and the runtime treat it.
/// - [`Function`](ScopeKind::Function) is *opaque* — a named function,
///   method or constructor cannot reach another function's locals, nor
///   the main block's. Only program-wide `global`s cross it.
/// - [`File`](ScopeKind::File) is the outermost scope: the main
///   block's locals plus every `global` declared in the closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// The one outermost scope, holding main-block locals and globals.
    File,
    /// A named function, method or constructor body (opaque).
    Function,
    /// A lambda body (transparent — closures capture).
    Lambda,
    /// A block, loop header or narrowing scope (transparent).
    Block,
}

/// Variable-name → recorded type for one lexical region.
pub(crate) struct Scope {
    pub(crate) locals: HashMap<String, Type>,
    pub(crate) kind: ScopeKind,
    /// The subset of [`locals`](Scope::locals) declared with the
    /// `global` keyword — the only names a [`ScopeKind::Function`]
    /// lookup may reach on the file scope. Non-empty only there:
    /// [`declare_global`](crate::checker::Checker::declare_global)
    /// always records into the file scope, wherever the declaration
    /// textually sits.
    pub(crate) globals: HashSet<String>,
}

impl Scope {
    pub(crate) fn new(kind: ScopeKind) -> Self {
        Self {
            locals: HashMap::new(),
            kind,
            globals: HashSet::new(),
        }
    }
}
