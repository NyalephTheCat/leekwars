//! Curated list of builtin functions and constants visible to all
//! Leekscript programs.
//!
//! Source: `runner/classes/*.java` static method names + game-side
//! runtime exports from the Leek Wars generator. This list is
//! deliberately generous — false positives (reporting a real builtin
//! as unknown) are worse than false negatives.
//!
//! Long-term this should be auto-generated from upstream Java sources
//! via a build script.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, RwLock};

/// Metadata for a builtin function: arity range plus the minimum
/// language version that exposes it. Functions missing from
/// [`BUILTIN_FNS`] are treated as variadic and version-agnostic.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinFn {
    pub name: &'static str,
    pub min_args: u8,
    pub max_args: u8,
    /// 1, 2, 3, or 4. Calls at lower versions emit `FUNCTION_NOT_AVAILABLE`.
    pub min_version: u8,
}

/// Metadata for an importable builtin library.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinLibrary {
    pub name: &'static str,
    pub symbols: &'static [&'static str],
}

const ANY: u8 = u8::MAX;

/// Builtins with known arity and/or version constraints. Conservative
/// — only entries we're confident about. Functions missing from this
/// table are not arity- or version-checked.
pub const BUILTIN_FNS: &[BuiltinFn] = &[
    // Math: well-defined single-arg functions. Arity checked at v3+.
    BuiltinFn {
        name: "sqrt",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "abs",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "ceil",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "floor",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "round",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "exp",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "cbrt",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "cos",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "sin",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "tan",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "acos",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "asin",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    BuiltinFn {
        name: "atan",
        min_args: 1,
        max_args: 1,
        min_version: 1,
    },
    // Map family — v4-only.
    BuiltinFn {
        name: "mapGet",
        min_args: 2,
        max_args: 3,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapPut",
        min_args: 3,
        max_args: 3,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapIsEmpty",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapClear",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapKeys",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapValues",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapSize",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapContainsKey",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapContainsValue",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapRemove",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapMerge",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "arrayGet",
        min_args: 2,
        max_args: 3,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapRemoveAll",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapReplace",
        min_args: 3,
        max_args: 3,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapReplaceAll",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapFill",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapSome",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapEvery",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapMin",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapMax",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapSum",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapAverage",
        min_args: 1,
        max_args: 1,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapSearch",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapIter",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapFold",
        min_args: 3,
        max_args: 3,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapFilter",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
    BuiltinFn {
        name: "mapMap",
        min_args: 2,
        max_args: 2,
        min_version: 4,
    },
];

/// Bare names that are immutable builtin constants. Assignment to
/// any of these emits `CANT_ASSIGN_VALUE` at every version.
///
/// The language-level set mirrors upstream `LeekConstants.java`
/// (`Infinity`/`NaN` are the upstream spellings; `INFINITY`/`NAN` are
/// kept as accepted aliases). Fight constants (`CHIP_*`, `WEAPON_*`,
/// …) are *not* here — they arrive via [`register_builtin_constant`]
/// when the `leekwars` library is loaded.
pub const BUILTIN_CONSTANTS: &[&str] = &[
    "PI",
    "E",
    "INFINITY",
    "Infinity",
    "NAN",
    "NaN",
    "INSTRUCTIONS_LIMIT",
    "OPERATIONS_LIMIT",
    "CELL_EMPTY",
    "CELL_PLAYER",
    "CELL_OBSTACLE",
    "COLOR_RED",
    "COLOR_GREEN",
    "COLOR_BLUE",
    "TYPE_NULL",
    "TYPE_BOOLEAN",
    "TYPE_NUMBER",
    "TYPE_STRING",
    "TYPE_ARRAY",
    "TYPE_OBJECT",
    "TYPE_FUNCTION",
    "TYPE_CLASS",
    "TYPE_MAP",
    "TYPE_SET",
    "TYPE_INTERVAL",
    "USE_SUCCESS",
    "USE_FAILED",
    "USE_CRITICAL",
    "USE_INVALID_TARGET",
    "USE_NOT_ENOUGH_TP",
    "USE_INVALID_POSITION",
    "USE_RESURRECT",
    "SORT_ASC",
    "SORT_DESC",
];

/// `Class.Field` paths that name an immutable builtin constant.
/// Assignment to any of these emits `CANNOT_ASSIGN_FINAL_FIELD`.
pub const FINAL_BUILTIN_FIELDS: &[&str] = &[
    "Integer.MIN_VALUE",
    "Integer.MAX_VALUE",
    "Integer.SIZE",
    "Integer.BYTES",
    "Real.MIN_VALUE",
    "Real.MAX_VALUE",
    "Real.PI",
    "Real.E",
    "Real.POSITIVE_INFINITY",
    "Real.NEGATIVE_INFINITY",
    "Real.NAN",
    "Real.EPSILON",
    "String.EMPTY",
];

// Variadic upper bound — kept as a constant for future use even
// though no current entries reach it.
#[allow(dead_code)]
const _ANY: u8 = ANY;

/// Names visible at every scope.
pub const BUILTINS: &[&str] = &[
    "getType",
    "typeOf",
    "instanceOf",
    "string",
    "number",
    "real",
    "integer",
    "boolean",
    "char",
    "clone",
    "isNull",
    "length",
    "count",
    "isEmpty",
    "debug",
    "debugC",
    "debugE",
    "debugW",
    "log",
    "max",
    "min",
    "abs",
    "ceil",
    "floor",
    "round",
    "sqrt",
    "pow",
    "cos",
    "sin",
    "tan",
    "acos",
    "asin",
    "atan",
    "atan2",
    "exp",
    "log10",
    "log2",
    "hypot",
    "cbrt",
    "rand",
    "randInt",
    "randFloat",
    "getSeed",
    "setSeed",
    "push",
    "pushAll",
    "pop",
    "shift",
    "unshift",
    "insert",
    "remove",
    "removeElement",
    "removeKey",
    "fill",
    "sort",
    "reverse",
    "shuffle",
    "join",
    "indexOf",
    "lastIndexOf",
    "search",
    "contains",
    "inArray",
    "slice",
    "splice",
    "chunk",
    "concat",
    "flatten",
    "reduce",
    "reduceRight",
    "forEach",
    "arrayForeach",
    "arrayMap",
    "arrayFilter",
    "arrayReduce",
    "arrayReduceRight",
    "arrayConcat",
    "arrayFlatten",
    "arrayCount",
    "arrayMax",
    "arrayMin",
    "arrayPartition",
    "arraySort",
    "arrayGet",
    "arrayIter",
    "arrayKeyExists",
    "arrayKeys",
    "arrayValues",
    "arrayLast",
    "arrayFirst",
    "arrayChunk",
    "arrayFoldLeft",
    "arrayFoldRight",
    "arrayCopy",
    "arrayConcatAll",
    "subArray",
    "arrayReplace",
    "arrayIntersect",
    "arrayDifference",
    "arrayUnion",
    "arrayDistinct",
    "arrayUnique",
    "arrayRandom",
    "arraySome",
    "arrayEvery",
    "arrayProduct",
    "arrayAvg",
    "arrayAdd",
    "arrayGroupBy",
    "arraySplit",
    "arraySplice",
    "entries",
    "mapKeys",
    "mapValues",
    "mapContainsKey",
    "mapContainsValue",
    "mapForEach",
    "mapSize",
    "mapIsEmpty",
    "mapPut",
    "mapGet",
    "mapRemove",
    "mapClear",
    "mapMerge",
    "mapFilter",
    "mapMap",
    "mapAverage",
    "mapEvery",
    "mapFill",
    "mapFold",
    "mapIter",
    "mapMax",
    "mapMin",
    "mapRemoveAll",
    "mapReplace",
    "mapReplaceAll",
    "mapSearch",
    "mapSome",
    "mapSum",
    "setRemove",
    "setContains",
    "setSize",
    "setClear",
    "setUnion",
    "setIntersection",
    "setDifference",
    "setForeach",
    "setForEach",
    "setToArray",
    "setIsEmpty",
    "charAt",
    "substring",
    "substr",
    "replace",
    "replaceAll",
    "split",
    "trim",
    "toString",
    "startsWith",
    "endsWith",
    "matches",
    "format",
    "stringContains",
    "stringFormat",
    "stringIndexOf",
    "stringJoin",
    "stringLength",
    "stringReverse",
    "stringSplit",
    "stringSubstring",
    "stringToLowerCase",
    "stringToUpperCase",
    "stringMatches",
    "numberAbs",
    "numberCeil",
    "numberFloor",
    "numberRound",
    "numberSqrt",
    "numberPow",
    "numberCos",
    "numberSin",
    "numberMax",
    "numberMin",
    "numberExp",
    "numberLog",
    "intervalSize",
    "intervalContains",
    "intervalIsEmpty",
    "intervalIter",
    "intervalMin",
    "intervalMax",
    "intervalAvg",
    "intervalForeach",
    "intervalForEach",
    "intervalReduce",
    "intervalReduceRight",
    "intervalFilter",
    "intervalMap",
    "jsonEncode",
    "jsonDecode",
    "json_encode",
    "json_decode",
    "color",
    "getColor",
    "getRed",
    "getGreen",
    "getBlue",
    "colorFromRGB",
    "colorToRGB",
    "getOperations",
    "getMaxOperations",
    "getUsedRAM",
    "getMaxRAM",
    "getRemainingOperations",
    "getRamUsage",
    "getAITimestamp",
    "getCurrentTime",
    "_",
    "getLife",
    "getTotalLife",
    "getMaxLife",
    "getStrength",
    "getAgility",
    "getWisdom",
    "getMP",
    "getTP",
    "getName",
    "getLevel",
    "getColors",
    "getTeamName",
    "getTeam",
    "getEnemiesCount",
    "getAlliesCount",
    "getLeek",
    "getLeeks",
    "getAlliedLeeks",
    "getEnemyLeeks",
    "getEntities",
    "getEnemy",
    "getEnemies",
    "getAllies",
    "getAbsoluteShield",
    "getRelativeShield",
    "getMagic",
    "getResistance",
    "getCellDistance",
    "getCellFromXY",
    "getCellX",
    "getCellY",
    "getCell",
    "getDistance",
    "getMapType",
    "getMap",
    "getNearestEnemy",
    "getNearestAlly",
    "getNearestAllyTo",
    "getNearestEnemyTo",
    "getPath",
    "getPathLength",
    "getOperationsCount",
    "getInstructionsCount",
    "getWeapons",
    "getWeapon",
    "getChips",
    "getCooldown",
    "moveToward",
    "moveTowardCell",
    "moveTowardLeek",
    "moveAwayFrom",
    "moveAwayFromCell",
    "moveAwayFromLeek",
    "useWeapon",
    "useWeaponOnCell",
    "useChip",
    "useChipOnCell",
    "setWeapon",
    "say",
    "show",
    "mark",
    "lineOfSight",
    "isAlive",
    "isDead",
    "isAlly",
    "isEnemy",
    "isOnSameLine",
    "isOnSameDiagonal",
    "isInlineAttack",
    "isDiagonal",
    "isStanding",
    "endTurn",
    "skipTurn",
    "summon",
    "getEntity",
    "getOperationsHistory",
    "getDamageReturn",
    "getPower",
    "getStartTP",
    "getStartMP",
    "PI",
    "E",
    "INFINITY",
    "Infinity",
    "NAN",
    "NaN",
    "INSTRUCTIONS_LIMIT",
    "OPERATIONS_LIMIT",
    "CELL_EMPTY",
    "CELL_PLAYER",
    "CELL_OBSTACLE",
    "COLOR_RED",
    "COLOR_GREEN",
    "COLOR_BLUE",
    "TYPE_NULL",
    "TYPE_BOOLEAN",
    "TYPE_NUMBER",
    "TYPE_STRING",
    "TYPE_ARRAY",
    "TYPE_OBJECT",
    "TYPE_FUNCTION",
    "TYPE_CLASS",
    "TYPE_MAP",
    "TYPE_SET",
    "TYPE_INTERVAL",
    "USE_SUCCESS",
    "USE_FAILED",
    "USE_CRITICAL",
    "USE_INVALID_TARGET",
    "USE_NOT_ENOUGH_TP",
    "USE_INVALID_POSITION",
    "USE_RESURRECT",
    "SORT_ASC",
    "SORT_DESC",
    "Number",
    "String",
    "Boolean",
    "Integer",
    "Real",
    "Array",
    "Map",
    "Set",
    "Object",
    "Function",
    "Class",
    "Interval",
    "System",
    "JSON",
    "Color",
    "Value",
    "Standard",
];

const FIGHT_GENERATOR_SYMBOLS: &[&str] = &["fightGenerate", "fightSeed", "fightPreview"];

/// Importable builtin libraries. These names are accepted by the
/// `import` statement and inject their symbols into resolver lookup.
pub const BUILTIN_LIBRARIES: &[BuiltinLibrary] = &[
    BuiltinLibrary {
        name: "fight.generator",
        symbols: FIGHT_GENERATOR_SYMBOLS,
    },
    // Alias accepted for users who prefer snake_case names.
    BuiltinLibrary {
        name: "fight_generator",
        symbols: FIGHT_GENERATOR_SYMBOLS,
    },
];

pub fn find_library(name: &str) -> Option<&'static BuiltinLibrary> {
    BUILTIN_LIBRARIES.iter().find(|lib| lib.name == name)
}

#[derive(Default)]
struct DynamicBuiltins {
    names: HashSet<String>,
    constants: HashSet<String>,
    functions: HashMap<String, (u8, u8, u8)>,
    libraries: HashMap<String, Vec<String>>,
}

static DYNAMIC_BUILTINS: LazyLock<RwLock<DynamicBuiltins>> =
    LazyLock::new(|| RwLock::new(DynamicBuiltins::default()));

/// Register an additional builtin function at runtime.
pub fn register_builtin_function(
    name: impl Into<String>,
    min_args: u8,
    max_args: u8,
    min_version: u8,
) {
    let name = name.into();
    let mut dyns = DYNAMIC_BUILTINS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    dyns.functions
        .insert(name.clone(), (min_args, max_args, min_version));
    dyns.names.insert(name);
}

/// Register an additional builtin constant at runtime.
pub fn register_builtin_constant(name: impl Into<String>) {
    let name = name.into();
    let mut dyns = DYNAMIC_BUILTINS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    dyns.constants.insert(name.clone());
    dyns.names.insert(name);
}

/// Register an additional builtin visible name at runtime.
pub fn register_builtin_name(name: impl Into<String>) {
    let name = name.into();
    let mut dyns = DYNAMIC_BUILTINS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    dyns.names.insert(name);
}

/// Register an additional importable library at runtime.
pub fn register_builtin_library(
    name: impl Into<String>,
    symbols: impl IntoIterator<Item = String>,
) {
    let mut dyns = DYNAMIC_BUILTINS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    dyns.libraries
        .insert(name.into(), symbols.into_iter().collect());
}

pub fn is_builtin_name(name: &str) -> bool {
    BUILTINS.contains(&name)
        || leek_builtins::is_java_builtin(name)
        || DYNAMIC_BUILTINS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .names
            .contains(name)
}

pub fn is_builtin_constant(name: &str) -> bool {
    BUILTIN_CONSTANTS.contains(&name)
        || DYNAMIC_BUILTINS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .constants
            .contains(name)
}

pub fn builtin_fn_meta(name: &str) -> Option<(u8, u8, u8)> {
    if let Some(b) = BUILTIN_FNS.iter().find(|b| b.name == name) {
        return Some((b.min_args, b.max_args, b.min_version));
    }
    DYNAMIC_BUILTINS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .functions
        .get(name)
        .copied()
}

/// Snapshot the dynamically-registered builtin functions as
/// `(name, min_args, max_args, min_version)` — for tools (the LSP) that
/// list builtins for completion/hover beyond the static [`BUILTIN_FNS`].
pub fn dynamic_builtin_functions() -> Vec<(String, u8, u8, u8)> {
    DYNAMIC_BUILTINS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .functions
        .iter()
        .map(|(n, &(lo, hi, v))| (n.clone(), lo, hi, v))
        .collect()
}

/// Snapshot the dynamically-registered builtin constant names.
pub fn dynamic_builtin_constants() -> Vec<String> {
    DYNAMIC_BUILTINS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .constants
        .iter()
        .cloned()
        .collect()
}

pub fn library_symbols(name: &str) -> Option<Vec<String>> {
    if let Some(lib) = find_library(name) {
        return Some(lib.symbols.iter().map(|s| (*s).to_string()).collect());
    }
    DYNAMIC_BUILTINS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .libraries
        .get(name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_builtin_function_and_constant_are_visible() {
        let fn_name = "__unit_builtin_fn".to_string();
        let const_name = "__UNIT_BUILTIN_CONST".to_string();
        register_builtin_function(fn_name.clone(), 1, 2, 3);
        register_builtin_constant(const_name.clone());

        assert!(is_builtin_name(&fn_name));
        assert!(is_builtin_constant(&const_name));
        assert_eq!(builtin_fn_meta(&fn_name), Some((1, 2, 3)));
    }

    #[test]
    fn dynamic_library_registration_is_resolvable() {
        let lib = "__unit_builtin_lib".to_string();
        register_builtin_library(
            lib.clone(),
            vec!["__unit_lib_fn".to_string(), "__unit_lib_const".to_string()],
        );

        let symbols = library_symbols(&lib).expect("library should resolve");
        assert!(symbols.contains(&"__unit_lib_fn".to_string()));
        assert!(symbols.contains(&"__unit_lib_const".to_string()));
    }

    // ---- Contract tests: keep the builtin metadata tables consistent. ----
    //
    // The three static tables are complementary facets, not copies:
    // `BUILTINS` is the always-visible name set, `BUILTIN_FNS` adds
    // arity/version constraints for a subset, `BUILTIN_CONSTANTS` lists
    // immutable constants. `is_builtin_name` unions all of them. These
    // tests pin the invariants that hold that union together, so a future
    // edit that, say, adds an arity entry for a name the resolver doesn't
    // otherwise recognize is caught at test time rather than surfacing as
    // a spurious "unknown function" diagnostic.

    // This contract test surfaced a real latent bug now fixed: `arrayGet`
    // has a `BUILTIN_FNS` arity entry and is a genuine dispatched stdlib
    // function (leek-runtime `array.rs`, `KNOWN_BUILTIN_NAMES`,
    // `catalog.yaml`, `stdlib.leek`) but was missing from the `BUILTINS`
    // visibility list, so `is_builtin_name("arrayGet")` returned false and
    // the resolver would have rejected `arrayGet(...)`. Adding it to
    // `BUILTINS` (a previously-unrecognized real builtin → recognized) is
    // strictly widening and corpus-safe.
    #[test]
    fn every_arity_constrained_fn_is_a_recognized_builtin() {
        // A name with arity/version metadata must resolve as a builtin —
        // otherwise the resolver would reject a call the metadata implies
        // is valid.
        for b in BUILTIN_FNS {
            assert!(
                is_builtin_name(b.name),
                "`{}` has BUILTIN_FNS metadata but is_builtin_name() rejects it",
                b.name,
            );
        }
    }

    #[test]
    fn arity_and_version_metadata_is_sane() {
        for b in BUILTIN_FNS {
            assert!(
                b.min_args <= b.max_args,
                "`{}`: min_args ({}) > max_args ({})",
                b.name,
                b.min_args,
                b.max_args,
            );
            assert!(
                (1..=4).contains(&b.min_version),
                "`{}`: min_version {} is not in 1..=4",
                b.name,
                b.min_version,
            );
        }
    }

    // This contract test surfaced (and we removed) duplicate `arrayMin`,
    // `arrayMax`, `arrayCount`, `concat`, and `getType` entries within the
    // `BUILTINS` list — harmless under the `.contains()` lookup but exactly
    // the hand-maintained-table drift this suite is meant to prevent
    // recurring.
    #[test]
    fn no_duplicate_names_within_each_table() {
        fn dup(names: impl IntoIterator<Item = &'static str>) -> Option<&'static str> {
            let mut seen = std::collections::HashSet::new();
            names.into_iter().find(|n| !seen.insert(*n))
        }
        assert_eq!(dup(BUILTINS.iter().copied()), None, "duplicate in BUILTINS");
        assert_eq!(
            dup(BUILTIN_CONSTANTS.iter().copied()),
            None,
            "duplicate in BUILTIN_CONSTANTS",
        );
        assert_eq!(
            dup(BUILTIN_FNS.iter().map(|b| b.name)),
            None,
            "duplicate name in BUILTIN_FNS",
        );
    }

    #[test]
    fn constants_and_functions_do_not_overlap() {
        // A name is either a value constant or a callable, never both —
        // overlap would make assignment-vs-call diagnostics ambiguous.
        let consts: std::collections::HashSet<&str> = BUILTIN_CONSTANTS.iter().copied().collect();
        for b in BUILTIN_FNS {
            assert!(
                !consts.contains(b.name),
                "`{}` is in both BUILTIN_FNS and BUILTIN_CONSTANTS",
                b.name,
            );
        }
    }
}
