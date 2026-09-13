//! Memoized parses of signature-only library headers (the stdlib / leek-wars
//! / implicit-prelude `.leek` headers).
//!
//! Both the type checker (signature seeding) and HIR lowering (prelude merge)
//! parse these headers. They share this one cache so they see the **same**
//! tree for a given program version: the parse is keyed on the language
//! version the program is compiled at, never a hard-coded one, so a v1–v3
//! program's checker and lowerer can't disagree about the header AST.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use leek_span::SourceId;
use leek_syntax::Version;
use rowan::GreenNode;

use crate::{ParseFeatures, parse_with_features};

/// `version → header text → green tree`. Nested so a lookup borrows the
/// header text instead of allocating a key. Header texts are few (a handful
/// of static headers plus the merged active-library text), so the cache stays
/// small. Poison-safe so one panicking thread can't wedge it.
static HEADER_PARSE_CACHE: LazyLock<Mutex<HashMap<Version, HashMap<String, GreenNode>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Parse a signature header at `version` (bodiless function signatures and
/// generics enabled), memoized on `(version, src)`. Returns a cheap
/// `Arc`-backed clone of the cached green tree; parse diagnostics are
/// dropped (headers aren't user source).
#[must_use]
pub fn parse_signature_header(src: &str, version: Version) -> GreenNode {
    if let Some(green) = HEADER_PARSE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&version)
        .and_then(|by_text| by_text.get(src))
    {
        return green.clone();
    }
    // Green trees carry no source id, so any id works for the throwaway
    // diagnostics.
    let source = SourceId::new(1).expect("non-zero SourceId");
    let parsed = parse_with_features(
        src,
        source,
        version,
        ParseFeatures {
            function_signatures: true,
            generics: true,
            ..Default::default()
        },
    );
    HEADER_PARSE_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(version)
        .or_default()
        .entry(src.to_owned())
        .or_insert(parsed.green)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_syntax::{SyntaxKind, SyntaxNode};

    fn has_class_decl(green: GreenNode) -> bool {
        SyntaxNode::new_root(green)
            .descendants()
            .any(|n| n.kind() == SyntaxKind::ClassDecl)
    }

    #[test]
    fn cache_is_keyed_by_version() {
        // `class` is only a keyword from v2 on, so the same header text
        // parses to different trees at v1 and v4. A cache keyed on the text
        // alone (or a hard-coded V4 parse) would hand v1 the v4 tree.
        let src = "class Header {}\n";
        let v1 = parse_signature_header(src, Version::V1);
        let v4 = parse_signature_header(src, Version::V4);
        assert!(!has_class_decl(v1.clone()), "v1 must not see a class decl");
        assert!(has_class_decl(v4.clone()), "v4 must see a class decl");
        // Repeated lookups hit the cache and return the same tree.
        assert_eq!(parse_signature_header(src, Version::V1), v1);
        assert_eq!(parse_signature_header(src, Version::V4), v4);
    }
}
