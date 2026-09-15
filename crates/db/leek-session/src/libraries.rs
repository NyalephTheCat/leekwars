//! Host-environment libraries: loading them, registering them with the
//! resolver, and turning their constants into a fold set.
//!
//! `--library leekwars` (and the manifest's `[project] libraries`) has to
//! reach the resolver *before* anything is compiled, or every call into
//! the game's API is an undefined function. Registration is
//! process-global, matching how the resolver supports importable builtin
//! libraries; the entry points here are the one place a driver does it,
//! so `leekc`, `miku` and the fight runners cannot load different sets.

use std::sync::{Arc, OnceLock};

use leek_config::{CompilationConfig, DynamicBuiltins, FoldSet, LibrarySet};

/// Register a host-environment library's functions with the resolver so the
/// whole pipeline (diagnostics, completion, type-checking) recognizes them
/// as defined functions instead of flagging them as undefined. Shared by
/// `leekc`, `miku`, and the LSP — load a catalog with
/// [`leek_environment::load_all`], then call this once.
///
/// Registration is process-global (the resolver's dynamic-builtin table),
/// matching how the resolver already supports importable builtin libraries.
/// `environment_contributions` is the same work as a value; this is that
/// plus the global apply.
pub fn register_environment(catalog: &dyn leek_environment::EnvironmentCatalog) {
    let mut builtins = DynamicBuiltins::default();
    environment_contributions(catalog, &mut builtins);
    register_builtins_globally(&builtins);
}

/// Add `catalog`'s functions and constants to `builtins`, writing nothing
/// process-global — the value half of [`register_environment`].
///
/// Host libraries aren't version-gated here, so every function is recorded
/// as available from v1. Constants (e.g. the fight constants `CELL_EMPTY`,
/// `WEAPON_PISTOL`) are recorded too, so they're recognized (no
/// "undefined") and offered in completion.
fn environment_contributions(
    catalog: &dyn leek_environment::EnvironmentCatalog,
    builtins: &mut DynamicBuiltins,
) {
    for (name, b) in catalog.entries() {
        builtins
            .functions
            .insert(name.to_string(), (b.min_arity, b.max_arity, 1));
        builtins.names.insert(name.to_string());
    }
    for (name, _ty) in catalog.constants() {
        builtins.constants.insert(name.to_string());
        builtins.names.insert(name.to_string());
    }
}

/// Everything loading a set of library specs produced.
///
/// The [`catalog`](Self::catalog) half is what a backend emits through; the
/// [`config`](Self::config) half is the same load as a value, so a caller
/// can thread it instead of reading back the process-globals
/// [`load_and_register_libraries`] writes.
#[derive(Debug)]
pub struct LoadedLibraries {
    /// The composed catalog. The built-in `leekwars` header contributes
    /// nothing to it — see [`load_and_register_libraries`].
    pub catalog: leek_environment::CompositeCatalog,
    /// The load as configuration: `libraries` names the headers to activate
    /// and `builtins` the functions and constants to register. `fold` and
    /// `seed_library` stay at their defaults, because loading a library
    /// neither folds constants nor seeds the type checker — `leekc
    /// --fold-constants` asks for folding separately, through
    /// [`leekwars_fold_set`].
    pub config: CompilationConfig,
}

/// Load library specs (built-in names like `"leekwars"`, or file paths)
/// into a [`LoadedLibraries`] value, touching no process-global.
///
/// Same per-spec branching as [`load_and_register_libraries`] — which is
/// this plus one global apply — except that the registrations are
/// accumulated into a [`CompilationConfig`] instead of being written to the
/// resolver's dynamic-builtin table and `leek_prelude`'s active-library
/// list. Nothing is written anywhere, so a failing spec leaves the process
/// exactly as it found it.
pub fn load_libraries<I, S>(specs: I) -> Result<LoadedLibraries, LibraryLoadError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut catalog = leek_environment::CompositeCatalog::new();
    let mut builtins = DynamicBuiltins::default();
    let mut libraries = LibrarySet::NONE;
    for spec in specs {
        let spec = spec.as_ref();
        if is_builtin_leekwars_spec(spec) {
            let leekwars = leekwars_config();
            merge_builtins(&mut builtins, &leekwars.builtins);
            libraries.insert(leekwars.libraries);
        } else {
            let lib = leek_environment::load(spec).map_err(|cause| LibraryLoadError {
                spec: spec.to_string(),
                cause,
            })?;
            environment_contributions(lib.as_ref(), &mut builtins);
            catalog.push(lib);
        }
    }
    Ok(LoadedLibraries {
        catalog,
        config: CompilationConfig {
            libraries,
            builtins: Arc::new(builtins),
            ..CompilationConfig::default()
        },
    })
}

/// Merge every registration in `from` into `into`; later values win, as
/// they do when the same name is registered twice with the resolver.
fn merge_builtins(into: &mut DynamicBuiltins, from: &DynamicBuiltins) {
    into.names.extend(from.names.iter().cloned());
    into.constants.extend(from.constants.iter().cloned());
    into.functions
        .extend(from.functions.iter().map(|(n, meta)| (n.clone(), *meta)));
    into.libraries
        .extend(from.libraries.iter().map(|(n, s)| (n.clone(), s.clone())));
}

/// Load library specs (built-in names like `"leekwars"`, or file paths) and
/// register them with the resolver in one step, returning the composed
/// catalog for a backend to emit through.
///
/// The built-in `leekwars` library is a *typed signature header*
/// (`leek_prelude::LEEKWARS_SRC`) carrying `@java-dispatch:` directives,
/// not a TSV catalog: its functions are registered by parsing the header,
/// its constants come from [`leek_environment`], and the header is
/// activated for HIR merge so the Java backend dispatches through the
/// directives (fully-qualified, no import needed). It contributes nothing
/// to the returned catalog; file-based `FileCatalog` libraries still use
/// the catalog's env-dispatch path.
///
/// [`load_libraries`] followed by one global apply. The specs are therefore
/// registered all-or-nothing: a spec that fails to load now leaves the
/// earlier ones unregistered too, where they used to be registered as they
/// were read. Every caller treats the error as fatal, so the difference is
/// not observable outside a test.
pub fn load_and_register_libraries<I, S>(
    specs: I,
) -> Result<leek_environment::CompositeCatalog, LibraryLoadError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let loaded = load_libraries(specs)?;
    apply_globally(&loaded.config);
    Ok(loaded.catalog)
}

/// Write a configuration into the process-globals it is meant to replace:
/// the resolver's dynamic-builtin table, `leek_prelude`'s active-library
/// list and fold-constant map, and `leek_types`' seeding flag.
///
/// Additive, because every one of those globals is: an empty field or a
/// `false` clears nothing, it simply contributes nothing. Applying two
/// configurations leaves the process holding their union — which is what
/// two `--library` flags, or two LSP workspaces, already do today.
fn apply_globally(config: &CompilationConfig) {
    register_builtins_globally(&config.builtins);
    // `activate_library` de-duplicates by pointer, and a `const` gives each
    // use site its own copy of the bytes, so each header is named here from
    // exactly one place: a second reference to the same `const` would be
    // pushed as a second library and merged into every file twice.
    if config.libraries.contains(LibrarySet::LEEKWARS) {
        leek_prelude::activate_library(leek_prelude::LEEKWARS_SRC);
    }
    if config.libraries.contains(LibrarySet::STDLIB) {
        leek_prelude::activate_library(leek_prelude::STDLIB_SRC);
    }
    if config.fold.contains(FoldSet::LEEKWARS) {
        leek_prelude::activate_fold_constants(
            leek_environment::leekwars_constant_values()
                .into_iter()
                .map(|(n, v)| (n.to_string(), v.to_string())),
        );
    }
    if config.seed_library {
        leek_types::set_seed_library(true);
    }
}

/// Replay a registry into the resolver's process-global one, name by name —
/// the only API it offers.
fn register_builtins_globally(builtins: &DynamicBuiltins) {
    for (name, &(min_args, max_args, min_version)) in &builtins.functions {
        leek_resolver::builtins::register_builtin_function(
            name.clone(),
            min_args,
            max_args,
            min_version,
        );
    }
    for name in &builtins.constants {
        leek_resolver::builtins::register_builtin_constant(name.clone());
    }
    for (name, symbols) in &builtins.libraries {
        leek_resolver::builtins::register_builtin_library(name.clone(), symbols.iter().cloned());
    }
    // Whatever is visible as a builtin without being a function or a
    // constant; registering either already makes its own name visible, so
    // the two loops above covered the rest.
    for name in builtins.names.iter().filter(|name| {
        !builtins.functions.contains_key(name.as_str())
            && !builtins.constants.contains(name.as_str())
    }) {
        leek_resolver::builtins::register_builtin_name(name.clone());
    }
}

/// The constant catalog `leekc --fold-constants` folds: the leek-wars fight
/// constants (`WEAPON_PISTOL` → `37`).
///
/// The value a caller puts in [`CompilationConfig::fold`]; the process-wide
/// form is [`activate_leekwars_constant_folding`].
#[must_use]
pub const fn leekwars_fold_set() -> FoldSet {
    FoldSet::LEEKWARS
}

/// Opt in to folding the leek-wars constants (`WEAPON_PISTOL` → `37`) during
/// HIR lowering — what `leekc --fold-constants` does. The official-parity
/// fight runners need this: the goldens are generated from AIs compiled with
/// folding on, and the native backend has no runtime lookup for environment
/// constants.
///
/// [`leekwars_fold_set`] plus the global apply.
pub fn activate_leekwars_constant_folding() {
    apply_globally(&CompilationConfig {
        fold: leekwars_fold_set(),
        ..CompilationConfig::default()
    });
}

/// Whether `spec` names the built-in leek-wars library rather than a
/// library-definition file. These specs go through the typed signature
/// header (see `leekwars_config`); [`leek_environment::load`] answers them
/// with a constants-only catalog that exposes no functions at all, so every
/// loader path has to branch on this *before* falling back to it.
fn is_builtin_leekwars_spec(spec: &str) -> bool {
    matches!(spec, "leekwars" | "fight" | "fight.generator")
}

/// The built-in leek-wars library as a configuration value: the header's
/// functions (names + arities), the fight constants, and the
/// [`LibrarySet::LEEKWARS`] bit that activates the header so HIR lowering
/// merges its signatures + `@java-dispatch:` directives.
///
/// Computed once per process. The header used to be re-parsed on every
/// call, and the callers are not rare — the LSP (on every configuration
/// reload), `miku fight`, `miku`'s `fight_emit`, `official-fight` and
/// `leek-dap` all ask for `leekwars` before they run.
fn leekwars_config() -> &'static CompilationConfig {
    static CONFIG: OnceLock<CompilationConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut builtins = DynamicBuiltins::default();
        for (name, min_args, max_args) in leekwars_header_arities() {
            builtins
                .functions
                .insert(name.clone(), (*min_args, *max_args, 1));
            builtins.names.insert(name.clone());
        }
        for (name, _ty) in leek_environment::leekwars_constants() {
            builtins.constants.insert(name.to_string());
            builtins.names.insert(name.to_string());
        }
        CompilationConfig {
            libraries: LibrarySet::LEEKWARS,
            builtins: Arc::new(builtins),
            ..CompilationConfig::default()
        }
    })
}

/// Register the leek-wars game library process-globally: `leekwars_config`
/// plus the global apply.
fn register_leekwars() {
    apply_globally(leekwars_config());
}

/// The leek-wars signature header as `(name, min_arity, max_arity)` rows —
/// the per-name parameter-count range across overloads — so the resolver
/// recognizes the functions (no "undefined function").
///
/// Parsed once per process; every later caller gets the same rows back.
fn leekwars_header_arities() -> &'static [(String, u8, u8)] {
    static ARITIES: OnceLock<Vec<(String, u8, u8)>> = OnceLock::new();
    ARITIES.get_or_init(parse_leekwars_header_arities)
}

/// Parse the header into arity rows — the uncached half of
/// [`leekwars_header_arities`], and the whole cost that `OnceLock` pays
/// exactly once.
fn parse_leekwars_header_arities() -> Vec<(String, u8, u8)> {
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::{ParseFeatures, parse_with_features};
    use leek_syntax::{SyntaxKind, SyntaxNode, Version};
    let parsed = parse_with_features(
        leek_prelude::LEEKWARS_SRC,
        leek_prelude::source_id(),
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            generics: true,
            ..Default::default()
        },
    );
    let Some(file) = SourceFile::cast(SyntaxNode::new_root(parsed.green)) else {
        return Vec::new();
    };
    let mut arities: std::collections::HashMap<String, (u8, u8)> = std::collections::HashMap::new();
    for child in file.syntax().children() {
        if child.kind() != SyntaxKind::FnDecl {
            continue;
        }
        let Some(name) = child
            .children_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
            .map(|t| t.text().to_string())
        else {
            continue;
        };
        let argc = child
            .children()
            .find(|n| n.kind() == SyntaxKind::ParamList)
            .map_or(0, |pl| {
                let n = pl
                    .children()
                    .filter(|n| n.kind() == SyntaxKind::Param)
                    .count();
                u8::try_from(n).unwrap_or(u8::MAX)
            });
        arities
            .entry(name)
            .and_modify(|(lo, hi)| {
                *lo = (*lo).min(argc);
                *hi = (*hi).max(argc);
            })
            .or_insert((argc, argc));
    }
    arities
        .into_iter()
        .map(|(n, (lo, hi))| (n, lo, hi))
        .collect()
}

/// What one library contributed when loaded, for verbose logging.
#[derive(Debug, Clone)]
pub struct LibraryStats {
    /// The spec as requested (`"leekwars"` or a file path).
    pub spec: String,
    /// Resolved import namespace(s) the catalog declares.
    pub imports: Vec<String>,
    /// Number of functions registered.
    pub functions: usize,
    /// Number of constants registered.
    pub constants: usize,
    /// A few function names (sorted) for a confirmation sample.
    pub sample_functions: Vec<String>,
    /// A few constant names (sorted) for a confirmation sample.
    pub sample_constants: Vec<String>,
}

/// One library spec that failed to load, and why.
///
/// Keeps the spec alongside the underlying
/// [`CatalogError`](leek_environment::CatalogError) so a caller reporting
/// several specs at once can say which one broke without parsing the message
/// back apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLoadError {
    /// The spec as the caller wrote it — a path, or a built-in name.
    pub spec: String,
    pub cause: leek_environment::CatalogError,
}

impl std::fmt::Display for LibraryLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.spec, self.cause)
    }
}

impl std::error::Error for LibraryLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Outcome of loading one library spec.
pub type LibraryLoadResult = Result<LibraryStats, LibraryLoadError>;

/// Describe what the built-in leek-wars library contributed, given the
/// arity rows `register_leekwars` registered.
fn leekwars_stats(spec: &str, arities: &[(String, u8, u8)]) -> LibraryStats {
    let mut fn_names: Vec<String> = arities.iter().map(|(n, _, _)| n.clone()).collect();
    let mut const_names: Vec<String> = leek_environment::leekwars_constants()
        .iter()
        .map(|(n, _)| (*n).to_string())
        .collect();
    fn_names.sort();
    const_names.sort();
    let functions = fn_names.len();
    let constants = const_names.len();
    fn_names.truncate(5);
    const_names.truncate(5);
    LibraryStats {
        spec: spec.to_string(),
        // The header dispatches through `@java-dispatch:` directives, so
        // there is no import namespace to report.
        imports: Vec::new(),
        functions,
        constants,
        sample_functions: fn_names,
        sample_constants: const_names,
    }
}

/// Load each spec individually, register it with the resolver, and return a
/// per-spec report (counts + a sorted sample of names, or a load error).
///
/// Unlike [`load_and_register_libraries`], this keeps going past a failing
/// spec (recording the error in that spec's slot) and reports each library's
/// individual contribution — for surfacing in the LSP / CLI logs so users can
/// confirm their library's functions *and constants* actually loaded.
pub fn load_register_and_report<I, S>(specs: I) -> Vec<LibraryLoadResult>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = Vec::new();
    for spec in specs {
        let spec = spec.as_ref();
        if is_builtin_leekwars_spec(spec) {
            // The header path, same as `load_and_register_libraries` —
            // literally the same now: both apply `leekwars_config()`, so
            // there is one description of what `leekwars` contributes and
            // the two paths cannot drift again. Going through
            // `leek_environment::load` here registered nothing (its leekwars
            // catalog has no functions) and left the prelude header
            // inactive, so the LSP — the only caller — logged "leekwars — 0
            // functions" and offered no signature, hover or completion for
            // `getCell` & co. while the CLIs had them all along.
            register_leekwars();
            out.push(Ok(leekwars_stats(spec, leekwars_header_arities())));
            continue;
        }
        match leek_environment::load(spec) {
            Ok(cat) => {
                register_environment(cat.as_ref());
                let mut fn_names: Vec<String> =
                    cat.entries().iter().map(|(n, _)| n.to_string()).collect();
                let mut const_names: Vec<String> =
                    cat.constants().iter().map(|(n, _)| n.to_string()).collect();
                fn_names.sort();
                const_names.sort();
                let functions = fn_names.len();
                let constants = const_names.len();
                fn_names.truncate(5);
                const_names.truncate(5);
                out.push(Ok(LibraryStats {
                    spec: spec.to_string(),
                    imports: cat.imports(),
                    functions,
                    constants,
                    sample_functions: fn_names,
                    sample_constants: const_names,
                }));
            }
            Err(cause) => out.push(Err(LibraryLoadError {
                spec: spec.to_string(),
                cause,
            })),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registration writes a process-global table shared by every test in
    /// this binary, so each assertion below is *monotone*: it checks that a
    /// name is registered with a particular arity, never that something is
    /// absent. Anything order-dependent would be flaky under `cargo test`'s
    /// thread-per-test model.
    fn arity_of(rows: &[(String, u8, u8)], name: &str) -> (u8, u8) {
        let (_, lo, hi) = rows
            .iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("`{name}` missing from the leekwars header rows"));
        (*lo, *hi)
    }

    #[test]
    fn leekwars_header_yields_the_documented_arity_ranges() {
        // Guards both the silent `Vec::new()` on a parse failure and the
        // per-name min/max fold across overloads. A header/parser regression
        // that disables all 305 functions shows up here rather than as
        // "undefined function" in an editor.
        // 305 signatures in the header fold into 195 distinct names.
        let rows = leekwars_header_arities();
        assert!(
            rows.len() >= 190,
            "only {} functions parsed from the header",
            rows.len()
        );
        // `getLife(entity)` / `getLife()`; `getCell(entity)` / `getCell()`.
        assert_eq!(arity_of(rows, "getLife"), (0, 1));
        assert_eq!(arity_of(rows, "getCell"), (0, 1));
        // Single-signature functions collapse to a point range.
        assert_eq!(arity_of(rows, "say"), (1, 1));
        assert_eq!(arity_of(rows, "useWeapon"), (1, 1));
        // `moveToward(entity, mp)` / `moveToward(entity)`.
        assert_eq!(arity_of(rows, "moveToward"), (1, 2));
        // `markText` ranges over 0..=3 parameters across its overloads.
        assert_eq!(arity_of(rows, "markText"), (0, 3));
        // Every row is a real range.
        for (name, lo, hi) in rows {
            assert!(lo <= hi, "{name}: {lo} > {hi}");
        }
    }

    #[test]
    fn header_names_are_unique_per_function() {
        let rows = leekwars_header_arities();
        let mut names: Vec<&str> = rows.iter().map(|(n, _, _)| n.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "overloads must fold into one row");
    }

    #[test]
    fn the_leekwars_header_is_parsed_once_per_process() {
        // The point of the `OnceLock`: every caller — the LSP on each
        // configuration reload, `miku fight`, `fight_emit`, `official-fight`
        // and `leek-dap` — used to re-parse all 305 signatures of the
        // header. Pointer identity is the observable form of "parsed once",
        // and it holds however many times the rows are asked for.
        assert!(std::ptr::eq(
            leekwars_header_arities().as_ptr(),
            leekwars_header_arities().as_ptr()
        ));
        assert!(std::ptr::eq(
            std::ptr::from_ref(leekwars_config()),
            std::ptr::from_ref(leekwars_config())
        ));
    }

    #[test]
    fn loading_leekwars_yields_a_config_rather_than_a_registration() {
        // The property this whole slice exists to create: the load is
        // available as a *value*, so a later caller can thread it instead of
        // reading back what `load_and_register_libraries` wrote.
        let loaded = load_libraries(["leekwars"]).expect("load");
        assert!(loaded.config.libraries.contains(LibrarySet::LEEKWARS));
        assert!(
            loaded.config.builtins.functions.len() >= 190,
            "{} functions in the config",
            loaded.config.builtins.functions.len()
        );
        assert_eq!(
            loaded.config.builtins.functions.get("getCell"),
            Some(&(0, 1, 1))
        );
        assert!(loaded.config.builtins.constants.contains("WEAPON_PISTOL"));
        // Every function and constant is visible as a builtin name.
        assert!(loaded.config.builtins.names.contains("useWeapon"));
        assert!(loaded.config.builtins.names.contains("WEAPON_PISTOL"));
        // Loading a library asks for neither folding nor seeding.
        assert_eq!(loaded.config.fold, FoldSet::NONE);
        assert!(!loaded.config.seed_library);
        // The header is not a TSV catalog, so it contributes no entries.
        assert!(leek_environment::EnvironmentCatalog::entries(&loaded.catalog).is_empty());
    }

    #[test]
    fn load_libraries_writes_nothing_to_the_resolver() {
        // The "without touching a global" half, asserted on a file catalog
        // rather than on `leekwars`: the dynamic-builtin table is
        // process-global and shared with every other test in this binary,
        // several of which register the leekwars names, so "`getCell` is not
        // registered" is not a claim this test could ever make. A function
        // named after this process is.
        let name = format!("loadLibrariesProbeFn{}", std::process::id());
        let constant = format!("LOAD_LIBRARIES_PROBE_CONST_{}", std::process::id());
        let dir = std::env::temp_dir().join(format!(
            "leek-session-recipes-probe-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("probe.lib");
        std::fs::write(
            &path,
            format!(
                "namespace = com.example.probe.*\n\
                 {name}\tProbeClass\tstatic\t2\t4\t1\n\
                 const {constant}\tinteger\n"
            ),
        )
        .expect("write library");
        let path = path.to_str().expect("utf-8 path");

        let loaded = load_libraries([path]).expect("load file catalog");
        assert_eq!(
            loaded.config.builtins.functions.get(&name),
            Some(&(2, 4, 1))
        );
        assert!(loaded.config.builtins.constants.contains(&constant));
        assert_eq!(leek_resolver::builtins::builtin_fn_meta(&name), None);
        assert!(!leek_resolver::builtins::is_builtin_constant(&constant));

        // …and the registering loader does write the very same rows.
        load_and_register_libraries([path]).expect("load file catalog");
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta(&name),
            Some((2, 4, 1))
        );
        assert!(leek_resolver::builtins::is_builtin_constant(&constant));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reporting_loader_registers_the_leekwars_header() {
        // Regression: `load_register_and_report` (the LSP's loader) went
        // straight to `leek_environment::load`, whose leekwars catalog
        // exposes no functions — so the editor reported "0 functions" and
        // flagged every `getCell()` as undefined while `leekc --library
        // leekwars` accepted it.
        let reports = load_register_and_report(["leekwars"]);
        assert_eq!(reports.len(), 1);
        let stats = reports[0].as_ref().expect("leekwars loads");
        assert_eq!(stats.spec, "leekwars");
        assert!(
            stats.functions >= 190,
            "reported {} functions",
            stats.functions
        );
        assert!(stats.constants > 0);
        assert_eq!(stats.sample_functions.len(), 5);
        assert!(stats.sample_functions.windows(2).all(|w| w[0] <= w[1]));

        // …and the registration actually reached the resolver.
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("getCell"),
            Some((0, 1, 1))
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("moveToward"),
            Some((1, 2, 1))
        );
        assert!(leek_resolver::builtins::is_builtin_constant(
            "WEAPON_PISTOL"
        ));
    }

    #[test]
    fn the_leekwars_aliases_take_the_same_header_path() {
        for spec in ["fight", "fight.generator"] {
            assert!(is_builtin_leekwars_spec(spec), "{spec}");
            let reports = load_register_and_report([spec]);
            let stats = reports[0]
                .as_ref()
                .unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert!(stats.functions >= 190, "{spec}: {}", stats.functions);
        }
        assert!(!is_builtin_leekwars_spec("leekwars.lib"));
        assert!(!is_builtin_leekwars_spec("/tmp/leekwars"));
    }

    #[test]
    fn leekwars_contributes_no_catalog_entries_but_still_registers() {
        // The documented contract: the header is not a TSV catalog, so the
        // composed catalog stays empty while the resolver learns the names.
        let catalog = load_and_register_libraries(["leekwars"]).expect("load");
        assert!(leek_environment::EnvironmentCatalog::entries(&catalog).is_empty());
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("useWeapon"),
            Some((1, 1, 1))
        );
    }

    #[test]
    fn a_missing_library_file_is_an_error_in_both_loaders() {
        let missing = "/definitely/not/a/library/file.lib";
        let err = load_and_register_libraries([missing]).expect_err("missing file");
        assert_eq!(err.spec, missing);
        assert!(
            matches!(err.cause, leek_environment::CatalogError::Io { .. }),
            "{err:?}"
        );

        // The reporting loader keeps going past a failure and records it in
        // that spec's slot — the documented difference between the two.
        let reports = load_register_and_report(["leekwars", missing]);
        assert_eq!(reports.len(), 2);
        assert!(reports[0].is_ok());
        let err = reports[1].as_ref().expect_err("missing file");
        assert_eq!(err.spec, missing);
    }

    #[test]
    fn file_catalogs_register_their_declared_arities_verbatim() {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-recipes-lib-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("demo.lib");
        std::fs::write(
            &path,
            "namespace = com.example.demo.*\n\
             # name\tclass\tkind\tmin\tmax\tops\n\
             demoZeroArg\tDemoClass\tstatic\t0\t0\t1\n\
             demoRange\tDemoClass\tstatic\t1\t3\t2\n\
             const DEMO_CONSTANT\tinteger\n",
        )
        .expect("write library");

        let catalog = load_and_register_libraries([path.to_str().expect("utf-8 path")])
            .expect("load file catalog");
        assert_eq!(
            leek_environment::EnvironmentCatalog::imports(&catalog),
            ["com.example.demo.*"]
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("demoZeroArg"),
            Some((0, 0, 1))
        );
        assert_eq!(
            leek_resolver::builtins::builtin_fn_meta("demoRange"),
            Some((1, 3, 1))
        );
        assert!(leek_resolver::builtins::is_builtin_constant(
            "DEMO_CONSTANT"
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_library_file_reports_the_offending_line() {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-recipes-bad-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("bad.lib");
        // A non-numeric arity field must fail loudly rather than register a
        // 0-arity entry that rejects every call.
        std::fs::write(&path, "brokenFn\tDemoClass\tstatic\tnope\n").expect("write");
        let err = load_and_register_libraries([path.to_str().expect("utf-8 path")])
            .expect_err("malformed");
        assert_eq!(
            err.cause,
            leek_environment::CatalogError::BadField {
                line: 1,
                field: 3,
                raw: "nope".to_string(),
            }
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
