//! The whole-program passes as tracked queries.
//!
//! [`include_graph`] says *which* files an entry reaches;
//! [`program_classes`] says what they all agree a class is. These three
//! queries are what a caller asks once it knows both: resolve, type
//! check and lower the closure **as one program**, the way
//! `docs/semantics.md` §3 says `include(…)` behaves — one shared
//! top-level scope, declarations visible program-wide, and main
//! statements spliced in execution order at their include sites.
//!
//! ### One green tree per file per program
//!
//! Every file's AST here comes from
//! [`parse_query`](leek_parser::pipeline::parse_query), keyed on the
//! same [`ProgramClasses`] set, so the three queries share one parse of
//! each file rather than one each. The green tree is cast to a red tree
//! *inside* the query body: a [`SyntaxNode`] is a per-thread cursor with
//! interior mutability and can never appear in a query's return value,
//! but nothing stops one existing while the body runs.
//!
//! ### Pure passes, untouched
//!
//! Each body assembles `FileUnit`s / `LowerUnit`s and calls the same
//! pure multi-file function the include-aware front end always called —
//! [`resolve_collecting_files`], [`check_collecting_files`],
//! [`lower_files`]. Not a line of the passes changes: what moved here is
//! the *memoization*, not the semantics.
//!
//! ### What is *not* in these values
//!
//! The include walk's own diagnostics (an unresolved name, a cycle) and
//! the per-site [`include_parse_failures`](crate::include::include_parse_failures)
//! reports are not folded in here. They belong to the graph, are
//! memoized beside it, and a caller assembling a diagnostic stream
//! concatenates them ahead of these. [`crate::diagnostics`] is that
//! caller, and states the whole order.

use std::path::{Path, PathBuf};

use leek_hir::lower::{LowerUnit, PRELUDE_UNIT_PATH, finish, lower_files, prelude_tree};
use leek_hir::pipeline::LowerHirResult;
use leek_parser::ast::{AstNode, SourceFile as Ast};
use leek_query::OptLevel;
use leek_resolver::FileUnit;
use leek_resolver::pipeline::ResolveArtifact;
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::{SyntaxNode, Version};
use leek_types::pipeline::TypeCheckArtifact;

use crate::include::{IncludeGraph, include_graph, program_classes};
use crate::{Db, ProgramClasses, SourceFile, WorkspaceFiles};

/// Resolve every file `entry` reaches as one program.
///
/// The pure [`resolve_collecting_files`](leek_resolver::resolve_collecting_files),
/// over parses this database already has. Re-runs when the closure's
/// shape, its class set, or any reached file's green tree changes — an
/// edit that leaves one leaf's tree equal re-resolves nothing.
#[salsa::tracked]
pub fn resolve_program(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> ResolveArtifact {
    let graph = include_graph(db, files, entry, entry_version);
    let parsed = parse_closure(db, files, entry, entry_version, &graph);
    let units = file_units(&parsed);
    // Pragmas contribute the experimental opt-ins only; the version and
    // strict mode are the settled ones off the entry's input. The
    // builtin registry `Options::default` snapshots is still a
    // process-global read from inside a tracked query — the same
    // untracked read `resolve_query` documents, and the same later slice
    // takes it off an input.
    let pragmas = leek_syntax::pipeline::pragma_query(db, entry).pragmas;
    let opts = leek_resolver::Options::from_settings(
        Some(&pragmas),
        FeatureFlags::from_bits(entry.flags_bits(db)),
        entry.strict(db),
    );
    let out = leek_resolver::resolve_collecting_files(&units, Some(&graph.resolved), opts);
    ResolveArtifact {
        diagnostics: out.diagnostics,
        table: out.table,
    }
}

/// Type-check every file `entry` reaches as one program.
///
/// Reads `strict`, `seed_library` and the experimental flags off the
/// entry's input rather than any process-global, so a memo cannot
/// outlive the settings it was computed under — the rule
/// `seed_library_is_an_input_tests` pins for the single-file query.
#[salsa::tracked]
pub fn typecheck_program(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> TypeCheckArtifact {
    let graph = include_graph(db, files, entry, entry_version);
    let parsed = parse_closure(db, files, entry, entry_version, &graph);
    let units = file_units(&parsed);
    let opts = leek_types::Options::from_settings(
        FeatureFlags::from_bits(entry.flags_bits(db)),
        entry.strict(db),
        entry.seed_library(db),
    );
    let out = leek_types::check_collecting_files(&units, Some(&graph.resolved), opts);
    TypeCheckArtifact {
        diagnostics: out.diagnostics,
        table: out.table,
        signatures: out.signatures,
    }
}

/// Lower every file `entry` reaches into one `HirFile`, at `opt`.
///
/// `opt` is part of the key on purpose. The single-file
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query) is keyed on
/// the file alone and therefore always lowers at
/// [`OptLevel::O0`], which leaves a codegen driver deep-cloning the
/// cached tree and optimizing the copy on **every** run (see the `O1`
/// branch in `leek_hir::pipeline`'s `run_lower`). Keyed here, the `O1`
/// tree is memoized like any other and the clone disappears.
#[salsa::tracked]
pub fn lower_program(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
    opt: OptLevel,
) -> LowerHirResult {
    // Entry boundary for the compilation configuration (#98, #226): still
    // a process-global, so still invisible to salsa, exactly as in
    // `lower_hir_query`. A later slice of epic #346 makes it an input.
    let libraries = leek_prelude::active_library_set();
    let fold = leek_hir::fold::fold_map(leek_prelude::active_fold_set());

    let graph = include_graph(db, files, entry, entry_version);
    let parsed = parse_closure(db, files, entry, entry_version, &graph);
    let flags = FeatureFlags::from_bits(entry.flags_bits(db));
    let Some((entry_file, includes)) = parsed.split_last() else {
        return LowerHirResult {
            hir: finish(leek_hir::HirFile::default(), &fold, opt),
            diagnostics: Vec::new(),
        };
    };

    // The active library headers merge in as a synthetic leading unit:
    // their bodiless signatures are pre-declared ahead of every user
    // file's, mirroring the single-file prelude path. No `include`
    // statement resolves to the synthetic path, so it contributes no
    // main block.
    let prelude = prelude_tree(libraries, flags.prelude, entry_version);
    let mut units: Vec<LowerUnit<'_>> = Vec::with_capacity(includes.len() + 1);
    if let Some((prelude_ast, prelude_source)) = &prelude {
        units.push(LowerUnit {
            ast: prelude_ast,
            source: *prelude_source,
            path: Path::new(PRELUDE_UNIT_PATH),
            version: entry_version,
        });
    }
    units.extend(includes.iter().map(ProgramFile::lower_unit));
    let (hir, diagnostics) = lower_files(
        entry_file.lower_unit(),
        &units,
        Some(&graph.resolved),
        flags,
    );
    LowerHirResult {
        hir: finish(hir, &fold, opt),
        diagnostics,
    }
}

/// One reached file's parse, in the owned form the pure passes borrow
/// their `FileUnit` / `LowerUnit` views out of.
struct ProgramFile {
    ast: Ast,
    source: SourceId,
    version: Version,
    path: PathBuf,
}

impl ProgramFile {
    fn lower_unit(&self) -> LowerUnit<'_> {
        LowerUnit {
            ast: &self.ast,
            source: self.source,
            path: &self.path,
            version: self.version,
        }
    }
}

/// Parse every file of the closure under the program's class set, in
/// the graph's dependency order — leaves first, the entry last, which
/// is the order all three pure passes require.
fn parse_closure(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
    graph: &IncludeGraph,
) -> Vec<ProgramFile> {
    let classes: ProgramClasses<'_> = program_classes(db, files, entry, entry_version);
    graph
        .files
        .iter()
        .map(|file| {
            let parse = leek_parser::pipeline::parse_query(db, file.file, classes);
            ProgramFile {
                ast: Ast::cast(SyntaxNode::new_root(parse.green))
                    .expect("grammar::source_file always opens a SourceFile root"),
                source: file.source,
                version: file.version,
                path: file.path.clone(),
            }
        })
        .collect()
}

/// The resolver/checker view of [`parse_closure`]'s output. Both passes
/// take the entry as the **last** element, which is the order the graph
/// already returns.
fn file_units(parsed: &[ProgramFile]) -> Vec<FileUnit<'_>> {
    parsed
        .iter()
        .map(|file| FileUnit {
            ast: &file.ast,
            source: file.source,
            version: file.version,
            path: &file.path,
        })
        .collect()
}

/// The whole program's MIR, lowered from [`lower_program`]'s merged HIR.
///
/// The include-aware counterpart of
/// [`lower_mir_query`](leek_mir::pipeline::lower_mir_query), which is keyed
/// on one file and so lowers the entry alone. Lowering a project through
/// the per-file query would silently drop every function an include
/// provides (#428), which is what this exists to prevent.
///
/// Keyed on `opt` for the reason [`lower_program`] is: an optimized program
/// is a different program, and keying it means a codegen driver reads its
/// own tree out of the cache instead of cloning the `O0` one and
/// optimizing the copy on every run.
#[salsa::tracked]
pub fn lower_program_mir(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
    opt: OptLevel,
) -> leek_mir::pipeline::LowerMirQueryResult {
    let hir = lower_program(db, files, entry, entry_version, opt);
    let (program, diagnostics) = leek_mir::lower::lower_and_optimize(hir.hir.as_ref(), opt);
    leek_mir::pipeline::LowerMirQueryResult {
        program: std::sync::Arc::new(program),
        diagnostics,
    }
}

/// The whole program's per-function complexity rows, over
/// [`lower_program`]'s merged HIR.
///
/// The include-aware counterpart of
/// [`complexity_query`](leek_complexity::pipeline::complexity_query). A
/// `miku analyze` over a project with includes wants a row for every
/// function the program defines, not only those the entry file spells out.
///
/// At [`OptLevel::O0`], matching the analysis drivers: folding constants
/// before measuring would report the cost of a tree the author did not
/// write.
#[salsa::tracked]
pub fn program_complexity(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> leek_complexity::pipeline::ComplexityReport {
    let hir = lower_program(db, files, entry, entry_version, OptLevel::O0);
    leek_complexity::pipeline::ComplexityReport(std::sync::Arc::new(leek_complexity::analyze_file(
        hir.hir.as_ref(),
    )))
}
