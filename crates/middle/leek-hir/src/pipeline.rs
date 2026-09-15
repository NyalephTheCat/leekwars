//! HIR lowering as a tracked query.
//!
//! The composition this query drives — parse the active headers, lower
//! the file against them, fold and optimize — lives in [`crate::lower`]
//! and [`crate::fold`] as public functions, so the whole-program lowering
//! in `leek-db` assembles the same stages over a closure.
//!
//! A note on what lowering does and does not share with the type checker:
//! lowering parses the **concatenation** of `PRELUDE_SRC` and the active
//! libraries under one cache key, while the checker parses `STDLIB_SRC` and
//! `LEEKWARS_SRC` as two separate keys (the `seed_header` calls in
//! `leek-types`' `checker/file.rs`). Both go through
//! [`leek_parser::parse_signature_header`], so they share its cache — but
//! never an entry in it. Each pass parses its own text once per language
//! version; neither re-parses per compile.

use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_query::OptLevel;
use leek_syntax::Version;

use crate::HirFile;
use crate::fold::fold_map;
use crate::lower::{finish, lower_one, prelude_tree};

/// Tracked return type for [`lower_hir_query`]: the HIR (in an
/// `Arc` for cheap cloning) plus the lowering pass's own diagnostics.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct LowerHirResult {
    pub hir: Arc<HirFile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for HIR lowering. Re-runs only when the
/// upstream [`parse_query`](leek_parser::pipeline::parse_query)'s
/// green tree changes.
///
/// Answers for **one file**, at [`OptLevel::O0`]. The include-aware
/// counterpart — which merges a closure's files into one tree and *is*
/// keyed on an opt level — is `leek_db::queries::lower_program`.
#[salsa::tracked]
pub fn lower_hir_query(
    db: &dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
) -> LowerHirResult {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_query::salsa::ProgramClasses;
    use leek_syntax::SyntaxNode;

    // Entry boundary for the compilation configuration (#98, #226): sampled
    // here rather than inside `prelude_tree` / `fold_map`, which are now pure
    // functions of these values. The read is still a process-global and so is
    // invisible to salsa — a later slice of epic #346 replaces it with a
    // tracked input on `file`, at which point this query re-runs when the
    // configuration changes.
    let libraries = leek_prelude::active_library_set();
    let fold = leek_prelude::active_fold_set();

    let parse = leek_parser::pipeline::parse_query(db, file, ProgramClasses::none(db));
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return LowerHirResult {
            hir: Arc::new(HirFile::default()),
            diagnostics: Vec::new(),
        };
    };
    let flags = leek_span::FeatureFlags::from_bits(file.flags_bits(db));
    let version_byte = file.version_byte(db);
    let prelude = prelude_tree(libraries, flags.prelude, Version::from_byte(version_byte));
    let (hir, diagnostics) = lower_one(
        &ast,
        file.source(db),
        version_byte,
        flags,
        prelude.as_ref().map(|(tree, source)| (tree, *source)),
    );
    LowerHirResult {
        hir: finish(hir, &fold_map(fold), OptLevel::O0),
        diagnostics,
    }
}
