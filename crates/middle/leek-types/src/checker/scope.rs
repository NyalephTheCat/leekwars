//! Lexical scopes for local type bindings.

use std::collections::HashMap;

use crate::ty::Type;

/// Variable-name → recorded type, with a flag marking
/// function/lambda boundaries for closure-capture exclusion.
pub(crate) struct Scope {
    pub(crate) locals: HashMap<String, Type>,
    pub(crate) is_function_boundary: bool,
}

impl Scope {
    pub(crate) fn empty() -> Self {
        Self {
            locals: HashMap::new(),
            is_function_boundary: false,
        }
    }
    pub(crate) fn function() -> Self {
        Self {
            locals: HashMap::new(),
            is_function_boundary: true,
        }
    }
}
