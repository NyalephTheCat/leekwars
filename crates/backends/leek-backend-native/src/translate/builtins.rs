//! Builtin-name classification (moved verbatim from translate/mod.rs).
//! Pure string matching — no Tx/state dependencies.

/// Builtins routed generically through the shared `leek_runtime::call_builtin`
/// (boxed args in, boxed result out). Restricted to *pure*, deterministic,
/// state-independent ops — no higher-order (lambda-taking) builtins, which
/// never reach native anyway.
/// Whether [`Tx::dispatch_builtin`](super::Tx::dispatch_builtin) recognizes
/// `name` as a builtin it can lower (so it won't hit the `unsupported` else).
/// Mirrors that method's recognition chain exactly. Used to decide, in the
/// method-call form, whether an unknown method (`null.toto()`) should fall
/// through to the generic runtime dispatch (→ null, matching the interpreter)
/// rather than refuse to compile. With `link_game`, the game runtime is the
/// catch-all, so everything is dispatchable.
pub(super) fn is_dispatchable_builtin(name: &str, link_game: bool) -> bool {
    link_game
        || matches!(name, "abs" | "signum" | "min" | "max" | "count" | "push")
        || leek_runtime::math_sig(name).is_some()
        || is_generic_builtin(name)
        || leek_runtime::builtin_class_name(name).is_some()
}

pub(super) fn is_generic_builtin(name: &str) -> bool {
    matches!(
        name,
        // Pure string operations.
        "charAt"
            | "charCodeAt"
            | "codePointAt"
            | "endsWith"
            | "ord"
            | "repeat"
            | "replace"
            | "split"
            | "startsWith"
            | "substring"
            | "toLower"
            | "toUpper"
            | "trim"
            | "chr"
            | "fromCharCode"
            | "join"
            | "length"
            // Pure collection operations (non-HOF, deterministic).
            | "contains"
            | "indexOf"
            | "search"
            | "reverse"
            | "sort"
            | "sum"
            | "isEmpty"
            | "first"
            | "last"
            | "subArray"
            | "arraySlice"
            | "concat"
            | "unique"
            | "keys"
            | "values"
            | "inArray"
            | "arrayFlatten"
            | "arrayChunk"
            // Higher-order array/map builtins: the callback is a lambda
            // (now compiled + registered in `LAMBDA_FNS`), dispatched via
            // `BuiltinHost::call_value`. Pure result, no RNG.
            | "arrayMap"
            | "arrayFilter"
            | "arrayFoldLeft"
            | "arrayFoldRight"
            | "arrayPartition"
            | "arrayEvery"
            | "arraySome"
            | "arrayFind"
            | "arrayIter"
            | "mapMap"
            | "mapFilter"
            | "mapFold"
            | "mapEvery"
            | "mapSome"
            | "mapIter"
            | "setFilter"
            | "arraySort"
            // Pure bit / number operations (scalar in, scalar out).
            | "bitCount"
            | "leadingZeros"
            | "trailingZeros"
            | "bitReverse"
            | "byteReverse"
            | "binString"
            | "hexString"
            | "realBits"
            | "rotateLeft"
            | "rotateRight"
            | "isPermutation"
            | "isNaN"
            | "isInfinite"
            | "isFinite"
            // More pure / in-place array & map operations.
            | "arrayUnique"
            | "arrayConcat"
            | "insert"
            | "unshift"
            | "arrayGet"
            | "arrayFrequencies"
            | "arrayToSet"
            | "arrayClear"
            | "arrayDistinct"
            | "arrayCount"
            | "arrayFirst"
            | "arrayLast"
            | "mapSearch"
            | "mapClear"
            | "mapPutAll"
            | "mapContains"
            | "mapContainsKey"
            | "mapContainsValue"
            | "mapKeys"
            | "mapValues"
            | "mapIsEmpty"
            | "mapSum"
            | "mapMin"
            | "mapMax"
            | "mapAverage"
            | "removeKey"
            | "mapRemoveAll"
            | "mapReplaceAll"
            | "mapGet"
            | "mapPut"
            | "getOperations"
            | "getInstructionsCount"
            // In-place array mutation (the backing `Rc<RefCell>` is shared
            // through the boxed handle, so the mutation is visible to the
            // caller's slot — no write-back needed).
            | "pop"
            | "shift"
            | "remove"
            | "fill"
            | "pushAll"
            // Pure interval operations.
            | "intervalToArray"
            | "intervalToSet"
            | "intervalContains"
            | "intervalCombine"
            | "intervalMin"
            | "intervalMax"
            | "intervalAverage"
            | "intervalIntersection"
            | "intervalIsEmpty"
            | "intervalIsBounded"
            | "intervalIsLeftBounded"
            | "intervalIsRightBounded"
            // Set operations: queries are pure; put/remove/clear mutate the
            // shared backing store in place.
            | "setPut"
            | "setRemove"
            | "setClear"
            | "setContains"
            | "setIsEmpty"
            | "setSize"
            | "setToArray"
            | "setIsSubsetOf"
            | "setIsSupersetOf"
            | "setUnion"
            | "setIntersection"
            | "setDifference"
            | "setDisjunction"
            // Pure map operations.
            | "mapMerge"
            | "mapSize"
            // Pure scalar / value conversions.
            | "typeOf"
            | "unknown"
            | "number"
            | "color"
            | "getColor"
            | "bitsToReal"
            // Pure aggregations + value ops.
            | "arrayMin"
            | "arrayMax"
            | "average"
            | "clone"
            | "string"
            // Pure JSON (de)serialization.
            | "jsonEncode"
            | "jsonDecode"
            // In-place collection edits — safe via the shared-`Rc` generic
            // path. `removeElement`/`assocSort`/`keySort`/`assocReverse` may
            // promote a dense array to a sparse LegacyArray in v1-v3 via a
            // stashed pending promotion; the post-call `Statement::ApplyPromotion`
            // writes the morphed map back to the caller's slot (now honoured
            // in v1-v3 — see the `ApplyPromotion` lowering).
            | "removeElement"
            | "arrayRemoveElement"
            | "assocSort"
            | "keySort"
            | "assocReverse"
            | "arrayRemoveAll"
            | "mapRemove"
            | "mapFill"
            | "mapReplace"
            // `debug`/`debugC`/`debugW`/`debugE` return null (no log sink
            // needed) — inert for value-equals.
            | "debug"
            | "debugC"
            | "debugW"
            | "debugE"
            // RNG: drawn from the per-run persistent `NATIVE_RNG`, the same
            // seeded xorshift sequence the interpreter uses — so native
            // reproduces the interpreter's RNG-dependent results (the corpus
            // expectations the interpreter already satisfies).
            | "rand"
            | "randInt"
            | "randFloat"
            | "randReal"
            | "arrayRandom"
            // `shuffle` is deterministic in the runtime (1-arg identity,
            // 2-arg seeded) — no divergence from the interpreter.
            | "shuffle"
            // Pure builtins implemented by `leek_runtime` but previously
            // skipped — siblings of ops already routed here.
            | "arraySize"        // array length, like `count`/`length`
            | "distinct"         // like `unique`/`arrayDistinct`
            | "fromCodePoint"    // like `fromCharCode`
            | "range"            // pure array generator
            // Colour-component extractors (pure, like `getColor`/`color`).
            | "getRed"
            | "getGreen"
            | "getBlue"
            // Pure hashing.
            | "hash"
            | "hashCode"
            | "stringHash"
            // Interval queries — siblings of the interval ops above.
            | "intervalSize"
            | "intervalIsClosed"
            | "intervalIsLeftClosed"
            | "intervalIsRightClosed"
            // Higher-order set ops — siblings of `setFilter` (lambda
            // dispatched via the compiled-lambda path).
            | "setMap"
            | "setIter"
    )
}
