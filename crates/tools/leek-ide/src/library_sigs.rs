//! Name → signature index for the embedded `.leek` library headers
//! (`stdlib.leek` builtins and `leekwars.leek` game functions).
//!
//! Builtin and leek-wars calls resolve to a `Builtin` symbol with no
//! user-source declaration node, so [`crate::signature::signature_for`]
//! has nothing to render and hover would only show `builtin <name>`.
//! This module parses the typed signature headers once and exposes each
//! function's rendered signature(s) + leading doc-comment so hover can
//! show the real shape — `function getLife(integer? entity) -> integer?`.
//!
//! Overloads (`abs(real)` / `abs(integer)`, `getLife()` / `getLife(…)`)
//! are kept as separate entries under the same name.

use std::collections::HashMap;
use std::sync::LazyLock;

use leek_parser::ast::{AstNode, FnDecl, SourceFile};
use leek_parser::{ParseFeatures, parse_with_features};
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

use crate::signature::signature_for;

/// One library function signature: the rendered one-line shape plus the
/// Doxygen/`//` doc-comment that preceded it in the header (if any).
#[derive(Debug, Clone)]
pub struct LibSig {
    pub signature: String,
    pub doc: Option<String>,
}

/// All library signatures grouped by function name. Lazily built from the
/// embedded headers on first lookup.
static INDEX: LazyLock<HashMap<String, Vec<LibSig>>> = LazyLock::new(build_index);

/// The library signatures for `name`, in header order (overloads
/// included), or `None` when the name isn't a known library function.
pub fn library_signatures(name: &str) -> Option<&'static [LibSig]> {
    INDEX.get(name).map(Vec::as_slice)
}

fn build_index() -> HashMap<String, Vec<LibSig>> {
    let mut map: HashMap<String, Vec<LibSig>> = HashMap::new();
    // Standard library (builtins) first, then the leek-wars game
    // functions. Both are typed signature headers with bodiless function
    // declarations, so they need the `function_signatures` parse feature.
    for src in [leek_prelude::STDLIB_SRC, leek_prelude::LEEKWARS_SRC] {
        collect_header(src, &mut map);
    }
    map
}

fn collect_header(src: &str, map: &mut HashMap<String, Vec<LibSig>>) {
    let parsed = parse_with_features(
        src,
        leek_prelude::source_id(),
        Version::LATEST,
        ParseFeatures {
            function_signatures: true,
            generics: true,
        },
    );
    let Some(file) = SourceFile::cast(SyntaxNode::new_root(parsed.green)) else {
        return;
    };
    for child in file.syntax().children() {
        let Some(fn_decl) = FnDecl::cast(child) else {
            continue;
        };
        let Some(name) = fn_name(&fn_decl) else {
            continue;
        };
        let Some(signature) = signature_for(fn_decl.syntax()) else {
            continue;
        };
        let offset = u32::from(fn_decl.syntax().text_range().start());
        let doc = crate::doc::doc_comment_before(src, offset).filter(|d| !d.trim().is_empty());
        map.entry(name).or_default().push(LibSig { signature, doc });
    }
}

/// A function declaration's name — the first `Ident` token child (the
/// name follows the `function` keyword and precedes any `<T>` type
/// parameter list).
fn fn_name(decl: &FnDecl) -> Option<String> {
    decl.syntax()
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_builtin_has_signature() {
        let sigs = library_signatures("count").expect("count is a builtin");
        assert!(!sigs.is_empty());
        assert!(
            sigs[0].signature.starts_with("function count("),
            "got: {}",
            sigs[0].signature
        );
    }

    #[test]
    fn overloaded_builtin_keeps_all_overloads() {
        let sigs = library_signatures("abs").expect("abs is a builtin");
        assert!(
            sigs.len() >= 2,
            "abs should have real+integer overloads, got {}",
            sigs.len()
        );
    }

    #[test]
    fn leekwars_function_is_indexed() {
        let sigs = library_signatures("getLife").expect("getLife is a leekwars fn");
        assert!(!sigs.is_empty());
        assert!(
            sigs.iter().any(|s| s.signature.contains("getLife")),
            "got: {sigs:?}"
        );
    }

    #[test]
    fn unknown_name_is_none() {
        assert!(library_signatures("definitely_not_a_builtin_xyz").is_none());
    }
}
