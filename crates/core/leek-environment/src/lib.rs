//! Pluggable host-environment builtin catalogs.
//!
//! The LeekScript *language* builtins (math, arrays, strings, …) live in
//! [`leek-builtins`]. A running host — the Leek Wars fight engine, or a
//! custom one — injects an additional set of *environment* functions on top
//! (combat API: `getCell`, `moveToward`, `useWeapon`, …). In the official
//! generator this is `LeekFunctions.setExtraFunctions(FightFunctions
//! .getFunctions(), "com.leekwars.generator.classes.*")`.
//!
//! An [`EnvironmentCatalog`] describes one such set so a backend can emit
//! calls to it. The official generator's catalog is [`LeekWarsCatalog`]; a
//! future custom fight generator supplies its own `impl`.
//!
//! Emit shape (matching the official v4 static path,
//! `LeekFunctionCall.writeJavaCode`): a call `f(args)` to a known
//! environment function becomes `<dispatch_class>.<name>(<ai>, args)`,
//! relying on an `import <import_namespace>;` in the generated file. The
//! Java method name is the LeekScript name verbatim, and the dispatch class
//! is the registered class + `Class` (e.g. `Entity` → `EntityClass`).

use std::collections::HashMap;
use std::sync::OnceLock;

/// One environment function's dispatch metadata.
#[derive(Debug, Clone)]
pub struct EnvBuiltin {
    /// Java class the call dispatches to (e.g. `"EntityClass"`). Resolved
    /// against the catalog's [`EnvironmentCatalog::import_namespace`].
    pub dispatch_class: String,
    /// `true` for a static-function call `Class.name(ai, args)`; `false`
    /// for a receiver call `((T) arg0).name(ai, rest)`.
    pub is_static: bool,
    /// Smallest / largest user-visible argument count across overloads
    /// (excludes the implicit AI receiver).
    pub min_arity: u8,
    pub max_arity: u8,
    /// Operations charged per call (the engine's op budget).
    pub op_cost: u32,
}

impl EnvBuiltin {
    /// Whether `argc` user arguments match one of this function's overloads.
    pub fn accepts_arity(&self, argc: usize) -> bool {
        (self.min_arity as usize..=self.max_arity as usize).contains(&argc)
    }
}

/// A loadable *library* of host-environment functions a backend can emit
/// calls to — the official fight functions, or any user-supplied library.
///
/// `Debug + Send + Sync` so a catalog can live behind an `Arc` in a
/// backend's (clonable, debuggable) options struct.
pub trait EnvironmentCatalog: std::fmt::Debug + Send + Sync {
    /// Java `import …;` targets for this library's dispatch classes —
    /// e.g. `["com.leekwars.generator.classes.*"]`. A backend adds these
    /// imports to any generated file that references the library. Multiple
    /// when composed from several libraries.
    fn imports(&self) -> Vec<String>;

    /// Look up a function by its LeekScript name.
    fn lookup(&self, name: &str) -> Option<&EnvBuiltin>;

    /// Every `(name, builtin)` in this library — used to register the names
    /// with the resolver (so they aren't flagged as undefined functions)
    /// and for completion.
    fn entries(&self) -> Vec<(&str, &EnvBuiltin)>;

    /// Named constants this library defines, as `(name, type-kind)` where
    /// the kind is one of `integer`/`real`/`boolean`/`string`/`array`/
    /// `map`/`null`/`any` (e.g. the fight constants `CELL_EMPTY`,
    /// `WEAPON_PISTOL`). Registered with the resolver so they aren't flagged
    /// as undefined and surface in completion. Empty by default.
    fn constants(&self) -> Vec<(&str, &str)> {
        Vec::new()
    }

    /// Whether `name` is a function in this library.
    fn is_known(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }
}

/// The official leek-wars-generator fight functions, extracted from
/// `FightFunctions.java` into `game_builtins.tsv`
/// (`tools/game-builtin-extract.sh`).
#[derive(Debug, Default, Clone, Copy)]
pub struct LeekWarsCatalog;

/// One fight constant: `(name, type-kind, value)`. `value` is the
/// resolved literal (`"37"`, `"0.05"`) — empty when not foldable (e.g.
/// an enum `.ordinal()`).
type ConstRow = (String, String, String);

static LEEKWARS_CONSTS: OnceLock<Vec<ConstRow>> = OnceLock::new();

fn leekwars_consts_table() -> &'static Vec<ConstRow> {
    LEEKWARS_CONSTS.get_or_init(|| {
        include_str!("../game_constants.tsv")
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                // `name <TAB> kind [<TAB> value]`.
                let mut f = line.split('\t');
                let name = f.next()?.trim().to_string();
                let kind = f.next().unwrap_or("any").trim().to_string();
                let value = f.next().unwrap_or("").trim().to_string();
                Some((name, kind, value))
            })
            .collect()
    })
}

/// The leek-wars fight constants (`CELL_EMPTY`, `WEAPON_PISTOL`, …) as
/// `(name, type-kind)`. The functions themselves now live in the typed
/// signature header (`leek_prelude::LEEKWARS_SRC`) with `@java-dispatch:`
/// directives; only the constant *names* still come from a table, so a
/// driver can register them with the resolver.
pub fn leekwars_constants() -> Vec<(&'static str, &'static str)> {
    leekwars_consts_table()
        .iter()
        .map(|(n, t, _)| (n.as_str(), t.as_str()))
        .collect()
}

/// Foldable fight constants as `(name, value)` — the resolved literal
/// for each constant whose value is known (`("WEAPON_PISTOL", "37")`,
/// `("EROSION_DAMAGE", "0.05")`). Constants without a resolvable value
/// are omitted. A driver turns these into an IR constant-folding map so
/// every backend emits the literal instead of an undefined identifier.
pub fn leekwars_constant_values() -> Vec<(&'static str, &'static str)> {
    leekwars_consts_table()
        .iter()
        .filter(|(_, _, v)| !v.is_empty())
        .map(|(n, _, v)| (n.as_str(), v.as_str()))
        .collect()
}

// The leek-wars *functions* are no longer a TSV catalog — they're the
// typed signature header. `LeekWarsCatalog` is retained as a constants
// provider (and a stable `load("leekwars")` target) but exposes no
// functions; dispatch happens via the header's directives.
impl EnvironmentCatalog for LeekWarsCatalog {
    fn imports(&self) -> Vec<String> {
        Vec::new()
    }

    fn lookup(&self, _name: &str) -> Option<&EnvBuiltin> {
        None
    }

    fn entries(&self) -> Vec<(&str, &EnvBuiltin)> {
        Vec::new()
    }

    fn constants(&self) -> Vec<(&str, &str)> {
        leekwars_constants()
    }
}

/// A library loaded from a text file. Format (tab-separated, `#` comments):
///
/// ```text
/// namespace = com.example.classes.*
/// # name      dispatch_class   static|receiver   min_arity  max_arity  ops
/// getCell     EntityClass      static            0          1          5
/// ```
///
/// `dispatch_class` is the *fully resolved* Java class the call dispatches
/// to. There may be several `namespace = …` lines (each becomes an import).
#[derive(Debug, Default, Clone)]
pub struct FileCatalog {
    namespaces: Vec<String>,
    table: HashMap<String, EnvBuiltin>,
    consts: Vec<(String, String)>,
}

impl FileCatalog {
    /// Parse a library definition from text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut cat = FileCatalog::default();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(ns) = line.strip_prefix("namespace") {
                let ns = ns.trim_start_matches(|c: char| c == '=' || c.is_whitespace());
                if !ns.is_empty() {
                    cat.namespaces.push(ns.to_string());
                }
                continue;
            }
            // `const NAME type` declares a named constant.
            if let Some(rest) = line.strip_prefix("const ") {
                let mut it = rest.split_whitespace();
                if let Some(name) = it.next() {
                    let ty = it.next().unwrap_or("any").to_string();
                    cat.consts.push((name.to_string(), ty));
                }
                continue;
            }
            let f: Vec<&str> = line.split('\t').map(str::trim).filter(|s| !s.is_empty()).collect();
            if f.len() < 2 {
                return Err(format!("line {}: expected `name<TAB>class[...]`", lineno + 1));
            }
            let is_static = f.get(2).is_none_or(|s| *s != "receiver");
            // A *present but non-numeric* arity/cost field means a malformed
            // catalog, not "default to 0" — fail loud rather than silently
            // registering a 0-arity entry that rejects every call to it.
            let field_u32 = |idx: usize| -> Result<Option<u32>, String> {
                match f.get(idx) {
                    Some(s) => s.parse::<u32>().map(Some).map_err(|_| {
                        format!("line {}: field {idx} {s:?} is not a number", lineno + 1)
                    }),
                    None => Ok(None),
                }
            };
            let to_u8 = |v: u32, what: &str| {
                u8::try_from(v).map_err(|_| format!("line {}: {what} out of range", lineno + 1))
            };
            let min_arity = to_u8(field_u32(3)?.unwrap_or(0), "min_arity")?;
            let max_arity = match field_u32(4)? {
                Some(v) => to_u8(v, "max_arity")?,
                None => min_arity,
            };
            let op_cost = field_u32(5)?.unwrap_or(0);
            cat.table.insert(
                f[0].to_string(),
                EnvBuiltin {
                    dispatch_class: f[1].to_string(),
                    is_static,
                    min_arity,
                    max_arity,
                    op_cost,
                },
            );
        }
        Ok(cat)
    }

    /// Load a library from a file path.
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::parse(&text)
    }
}

impl EnvironmentCatalog for FileCatalog {
    fn imports(&self) -> Vec<String> {
        self.namespaces.clone()
    }
    fn lookup(&self, name: &str) -> Option<&EnvBuiltin> {
        self.table.get(name)
    }
    fn entries(&self) -> Vec<(&str, &EnvBuiltin)> {
        self.table.iter().map(|(k, v)| (k.as_str(), v)).collect()
    }
    fn constants(&self) -> Vec<(&str, &str)> {
        self.consts.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect()
    }
}

/// Several libraries loaded together. Lookups try each in order; imports
/// and entries are the union (so a program can use functions from multiple
/// libraries at once).
#[derive(Debug, Default)]
pub struct CompositeCatalog {
    libraries: Vec<Box<dyn EnvironmentCatalog>>,
}

impl CompositeCatalog {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, lib: Box<dyn EnvironmentCatalog>) {
        self.libraries.push(lib);
    }
    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }
}

impl EnvironmentCatalog for CompositeCatalog {
    fn imports(&self) -> Vec<String> {
        let mut out = Vec::new();
        for lib in &self.libraries {
            for imp in lib.imports() {
                if !out.contains(&imp) {
                    out.push(imp);
                }
            }
        }
        out
    }
    fn lookup(&self, name: &str) -> Option<&EnvBuiltin> {
        self.libraries.iter().find_map(|lib| lib.lookup(name))
    }
    fn entries(&self) -> Vec<(&str, &EnvBuiltin)> {
        self.libraries.iter().flat_map(|lib| lib.entries()).collect()
    }
    fn constants(&self) -> Vec<(&str, &str)> {
        self.libraries.iter().flat_map(|lib| lib.constants()).collect()
    }
}

/// Load a library by *spec*: the built-in name `"leekwars"` (the official
/// fight functions), or a path to a [`FileCatalog`] definition file.
pub fn load(spec: &str) -> Result<Box<dyn EnvironmentCatalog>, String> {
    match spec {
        "leekwars" | "fight" | "fight.generator" => Ok(Box::new(LeekWarsCatalog)),
        path => FileCatalog::from_path(std::path::Path::new(path))
            .map(|c| Box::new(c) as Box<dyn EnvironmentCatalog>),
    }
}

/// Load and compose several library specs into one catalog.
pub fn load_all<I, S>(specs: I) -> Result<CompositeCatalog, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut composite = CompositeCatalog::new();
    for spec in specs {
        composite.push(load(spec.as_ref())?);
    }
    Ok(composite)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leekwars_catalog_exposes_no_functions() {
        // Functions moved to the typed signature header; the catalog now
        // only carries constants.
        let cat = LeekWarsCatalog;
        assert!(cat.lookup("getCell").is_none());
        assert!(cat.entries().is_empty());
    }

    #[test]
    fn leekwars_catalog_has_constants() {
        let cat = LeekWarsCatalog;
        let consts = cat.constants();
        assert!(consts.len() >= 300, "got {}", consts.len());
        assert!(consts.iter().any(|(n, t)| *n == "CELL_EMPTY" && *t == "integer"));
        assert!(consts.iter().any(|(n, _)| *n == "WEAPON_PISTOL"));
        // The free accessor used by leek-recipes matches.
        assert_eq!(leekwars_constants().len(), consts.len());
    }

    #[test]
    fn leekwars_constant_values_resolve() {
        let vals = leekwars_constant_values();
        let get = |n: &str| vals.iter().find(|(name, _)| *name == n).map(|(_, v)| *v);
        assert_eq!(get("WEAPON_PISTOL"), Some("37"));
        assert_eq!(get("CELL_EMPTY"), Some("0"));
        assert_eq!(get("MAX_TURNS"), Some("64")); // chained via State.MAX_TURNS
        assert_eq!(get("EROSION_DAMAGE"), Some("0.05")); // real literal
        // Most constants resolve; only a couple (enum .ordinal()) don't.
        assert!(vals.len() >= 360, "got {}", vals.len());
    }
}
