//! MIR-walking interpreter.
//!
//! Replaces the earlier HIR tree-walker. The interpreter owns a
//! [`leek_mir::MirProgram`] and executes one [`MirFunction`] at a
//! time, walking basic blocks until each one's [`Terminator`]
//! dispatches to the next block (or returns).
//!
//! The value model, builtin dispatch table, and op-budget bookkeeping
//! are unchanged — those live in [`crate::value`] and
//! the stdlib builtins in `leek_runtime`. What changed is the IR being walked.
//!
//! ## Regressed in this slice
//!
//! MIR does not yet lower:
//! - Classes — `new` / `this` / `super` raise `Outcome::Error`.
//! - Lambdas with captures — fall back to `Outcome::Error` at
//!   construction time.
//! - The full foreach iterator protocol — the body still executes
//!   but with no per-element binding update, so most foreach-based
//!   corpus tests will fail until MIR coverage lands.
//!
//! The HIR interpreter that handled these used to live here; it is
//! gone. Re-adding parity is tracked as the next MIR-coverage slice.

#![allow(
    clippy::collapsible_if,
    clippy::map_clone,
    clippy::useless_asref,
    clippy::type_complexity,
    clippy::manual_range_contains,
    clippy::identity_op,
    clippy::redundant_clone,
    clippy::single_match,
    clippy::needless_borrow
)]

use std::collections::HashMap;

use leek_hir::DefId;
use leek_mir::{
    FunctionKind, MirProgram,
};
use leek_types::Type;

use crate::value::Value;

/// A tiny FNV-1a [`Hasher`](std::hash::Hasher) for the file-global store. Global
/// names are short ASCII identifiers hashed on every read/write in hot loops;
/// the stdlib's default SipHash is DoS-resistant but ~3-4× slower than FNV for
/// such keys, and globals are an internal, untrusted-input-free map, so the
/// trade is pure win here. Used only for [`Interpreter::globals`].
#[derive(Clone, Copy)]
pub(crate) struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }
}

/// File-global store keyed by name, using the fast [`FnvHasher`].
pub(crate) type GlobalMap =
    HashMap<String, Value, std::hash::BuildHasherDefault<FnvHasher>>;

// ---- Outcomes and result ----

/// Bubbled control-flow / error outcome.
#[derive(Debug)]
pub enum Outcome {
    /// Statement ran to completion; continue with the next.
    Next,
    /// `return [value]` — bubble up to the enclosing function.
    Return(Value),
    /// `break` — bubble up to the enclosing loop / switch. MIR
    /// already lowers break to a goto inside the function, so this
    /// variant is only used by the [`call_value`] entry point for
    /// stray top-level breaks.
    Break,
    /// `continue` — same caveat as `Break`.
    Continue,
    /// Runtime error (uncaught).
    Error(String),
}

#[derive(Debug)]
pub struct RunResult {
    pub value: Value,
    pub error: Option<String>,
}

// ---- Interpreter ----

pub struct Interpreter<'a> {
    pub(crate) program: &'a MirProgram,
    /// File-level globals keyed by source name. We key by name (not
    /// DefId) because HIR doesn't always allocate a `Def::Global`
    /// for top-level `var`/`global` declarations — both forms read
    /// and write through this map.
    pub(crate) globals: GlobalMap,
    pub(crate) version: u8,
    /// Strict mode — when on, inferred types coerce on plain
    /// `=` too (`var a = 5.5; a = 2` stores `2.0`). Off by
    /// default to match the non-strict corpus runs.
    pub(crate) strict: bool,
    pub(crate) op_limit: Option<u64>,
    pub(crate) op_count: u64,
    /// Internal block-transition counter — caps runaway loops
    /// without inflating the user-facing op count.
    pub(crate) block_count: u64,
    /// Current call-stack depth. Used to abort infinite recursion
    /// before the host's native stack overflows (the corpus has
    /// tests that deliberately recurse forever to assert upstream's
    /// stack-overflow error code).
    pub(crate) call_depth: u32,
    /// Stack of currently-executing function indices. The top is
    /// the active frame; helpers like `current_frame_local_type`
    /// consult it to look up declared types on locals.
    pub(crate) function_stack: Vec<usize>,
    /// User-defined function index keyed by DefId for O(1) calls.
    /// Includes method DefIds (so `Value::Function(FnValue::User(m))`
    /// dispatches correctly for both top-level functions and
    /// methods).
    pub(crate) fn_by_def: HashMap<DefId, usize>,
    /// Class index keyed by name for O(1) `new ClassName(...)` and
    /// static-member lookup.
    pub(crate) class_by_name: HashMap<String, usize>,
    /// Lazily-evaluated static fields keyed by `(class_def, name)`.
    /// Each field's initializer runs the first time it's read.
    pub(crate) static_fields: HashMap<(DefId, String), Value>,
    /// Index of the synthetic main function in `program.functions`.
    pub(crate) main_idx: Option<usize>,
    /// Optional profiler — when present, the interpreter records
    /// per-call-stack ops samples for [`crate::Profiler`] to
    /// later emit as folded-flame data. Enabled via
    /// [`Self::set_profiler`].
    pub(crate) profiler: Option<crate::profiler::Profiler>,
    /// xorshift64 PRNG state for `rand` / `randInt` / `randFloat`.
    /// Seeded with a fixed constant so corpus runs stay reproducible
    /// while still producing a uniform stream (the statistical corpus
    /// tests — e.g. the DNA `contains` distribution — need real spread,
    /// not a constant low-bound).
    pub(crate) rng: leek_runtime::Rng,
    /// Signature-file builtins: `DefId` → builtin name, for bodiless
    /// functions carrying backend directives. A call to one dispatches
    /// the named runtime builtin (the interpreter has no body to run).
    /// Populated by [`Self::set_bodiless_builtins`] from the HIR.
    pub(crate) bodiless_builtins: HashMap<DefId, String>,
    /// Global name → declared type, precomputed once so a global write can
    /// look up its coercion type in O(1) instead of linear-scanning
    /// `program.globals` (and cloning the `Type`) on every assignment. Only
    /// non-`Any` entries are kept — an absent name means "no coercion".
    pub(crate) global_types: HashMap<String, Type>,
}

mod call;
mod class;
mod exec;
mod value;

pub(crate) use value::StepResult;

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a MirProgram) -> Self {
        let mut me = Self {
            program,
            globals: GlobalMap::default(),
            version: 4,
            strict: false,
            op_limit: None,
            op_count: 0,
            block_count: 0,
            call_depth: 0,
            function_stack: Vec::new(),
            fn_by_def: HashMap::new(),
            class_by_name: HashMap::new(),
            static_fields: HashMap::new(),
            main_idx: None,
            profiler: None,
            rng: leek_runtime::Rng::new(),
            bodiless_builtins: HashMap::new(),
            global_types: HashMap::new(),
        };
        me.index_functions();
        me.index_classes();
        me.index_global_types();
        me
    }

    /// Precompute the global-name → declared-type map consulted on the hot
    /// global-write path. Skips `Any`-typed globals (the common untyped case)
    /// since they need no coercion. Run once; `program` is immutable after.
    fn index_global_types(&mut self) {
        for g in &self.program.globals {
            if !matches!(g.ty, Type::Any) {
                self.global_types.insert(g.name.clone(), g.ty.clone());
            }
        }
    }

    /// Register signature-file builtins (`DefId` → builtin name) so calls
    /// to bodiless directive functions dispatch the runtime builtin.
    pub fn set_bodiless_builtins(&mut self, map: HashMap<DefId, String>) {
        self.bodiless_builtins = map;
    }

    pub fn with_op_limit(program: &'a MirProgram, limit: u64) -> Self {
        let mut me = Self::new(program);
        me.op_limit = Some(limit);
        me
    }

    pub fn set_version(&mut self, v: u8) {
        self.version = v;
        // Display formatting depends on language version (v1 uses
        // French-locale number formatting, v3 / v4 differ on set-vs-
        // array `[]` / `[:]` rendering, etc.). The `Value::Display`
        // impl reads this thread-local — without this sync, calling
        // `value.to_string()` after a v1 run renders with v4 rules.
        crate::value::DISPLAY_VERSION.with(|c| c.set(v));
    }

    /// Total ops charged since construction. Updated by `charge_ops`
    /// at every operation site. Used by the corpus runner to verify
    /// `.ops(N)` expectations.
    pub fn ops_used(&self) -> u64 {
        self.op_count
    }

    pub fn set_strict(&mut self, s: bool) {
        self.strict = s;
    }


    /// Enable stack-aware ops profiling. Hooks fire at every user-
    /// function entry/exit (including the synthetic `<main>`
    /// frame). Use [`Self::take_profiler`] after `run()` to read
    /// the collected samples.
    pub fn set_profiler(&mut self, p: crate::profiler::Profiler) {
        self.profiler = Some(p);
    }

    /// Retrieve the active profiler, leaving the interpreter
    /// without one. Returns `None` if profiling wasn't enabled.
    pub fn take_profiler(&mut self) -> Option<crate::profiler::Profiler> {
        self.profiler.take()
    }

    pub(crate) fn index_functions(&mut self) {
        for (i, f) in self.program.functions.iter().enumerate() {
            if let Some(def) = f.def_id {
                self.fn_by_def.insert(def, i);
            }
            if f.kind == FunctionKind::Main {
                self.main_idx = Some(i);
            }
        }
    }

    pub(crate) fn index_classes(&mut self) {
        for (i, c) in self.program.classes.iter().enumerate() {
            self.class_by_name.insert(c.name.clone(), i);
        }
    }

    /// Look up a method on `class_name` by name, walking the parent
    /// chain until found or the chain runs out. Returns the MIR
    /// function index, suitable for [`run_function`].
    pub(crate) fn find_method(&self, class_name: &str, method: &str) -> Option<usize> {
        self.find_method_arity(class_name, method, None)
    }

    pub(crate) fn find_static_method(&self, class_name: &str, method: &str) -> Option<usize> {
        self.find_static_method_arity(class_name, method, None)
    }

    /// Arity-aware method lookup: when `argc` is `Some(n)`, prefer
    /// an overload whose declared `user_arity == n` (searched
    /// across the WHOLE chain first), only falling back to the
    /// first matching name once no exact-arity overload exists.
    /// Used by the dispatcher so `class B extends A { x(a, b) {} }`
    /// invoked as `x(a+b)` (1 arg) reaches `A.x(a)` rather than
    /// looping on `B.x(a, b)`.
    pub(crate) fn find_method_arity(
        &self,
        class_name: &str,
        method: &str,
        argc: Option<usize>,
    ) -> Option<usize> {
        if let Some(n) = argc {
            if let Some(idx) = self.walk_class_chain(class_name, |c| {
                c.methods
                    .iter()
                    .find(|m| m.name == method && !m.is_static && m.user_arity == n)
                    .map(|m| m.function_idx)
            }) {
                return Some(idx);
            }
        }
        self.walk_class_chain(class_name, |c| {
            c.methods
                .iter()
                .find(|m| m.name == method && !m.is_static)
                .map(|m| m.function_idx)
        })
    }

    pub(crate) fn find_static_method_arity(
        &self,
        class_name: &str,
        method: &str,
        argc: Option<usize>,
    ) -> Option<usize> {
        if let Some(n) = argc {
            if let Some(idx) = self.walk_class_chain(class_name, |c| {
                c.methods
                    .iter()
                    .find(|m| m.name == method && m.is_static && m.user_arity == n)
                    .map(|m| m.function_idx)
            }) {
                return Some(idx);
            }
        }
        self.walk_class_chain(class_name, |c| {
            c.methods
                .iter()
                .find(|m| m.name == method && m.is_static)
                .map(|m| m.function_idx)
        })
    }

    /// Find a constructor matching the given argument count. Falls
    /// back to a parent's constructor only if the child class has
    /// no constructors at all (matching upstream's behavior).
    pub(crate) fn find_constructor(&self, class_name: &str, argc: usize) -> Option<usize> {
        self.walk_class_chain(class_name, |class| {
            if class.constructors.is_empty() {
                return None;
            }
            // Prefer an exact-arity match; otherwise take the
            // first constructor and let default-init params sort
            // out the rest.
            if let Some(c) = class.constructors.iter().find(|c| c.user_arity == argc) {
                return Some(c.function_idx);
            }
            Some(class.constructors[0].function_idx)
        })
    }

    /// Walk `class_name` and its parents in order, calling `f` on
    /// each class until it returns `Some`. Cycle-safe — a class
    /// already visited terminates the walk.
    pub(crate) fn walk_class_chain<R>(
        &self,
        class_name: &str,
        mut f: impl FnMut(&leek_mir::MirClass) -> Option<R>,
    ) -> Option<R> {
        let mut cursor = class_name.to_string();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            if !visited.insert(cursor.clone()) {
                return None;
            }
            let idx = *self.class_by_name.get(&cursor)?;
            let class = &self.program.classes[idx];
            if let Some(r) = f(class) {
                return Some(r);
            }
            cursor = class.parent.clone()?;
        }
    }

    pub fn run(&mut self) -> RunResult {
        let Some(idx) = self.main_idx else {
            return RunResult {
                value: Value::Null,
                error: None,
            };
        };
        let outcome = self.run_function(idx, Vec::new());
        let v = match outcome {
            Ok(v) => v,
            Err(Outcome::Return(v)) => v,
            Err(Outcome::Error(e)) => {
                return RunResult {
                    value: Value::Null,
                    error: Some(e),
                };
            }
            Err(_) => {
                return RunResult {
                    value: Value::Null,
                    error: None,
                };
            }
        };
        // User-defined `string()` method on a class instance: when
        // present, the displayed value goes through it (so
        // `class A { string() { return 'test' } } return new A()`
        // shows `test`). Mirrors upstream's
        // `Object.prototype.toString` override.
        let v = self.invoke_instance_string_method(v);
        RunResult {
            value: v,
            error: None,
        }
    }

    /// If `v` is a class instance whose class declares a
    /// `string()` method, invoke it and use its return value.
    /// Otherwise pass `v` through unchanged. Sets the
    /// `DISPLAY_TOP_LEVEL_BARE` flag so the returned string
    /// renders without quotes (matching upstream).
    pub(crate) fn invoke_instance_string_method(&mut self, v: Value) -> Value {
        let Value::Instance(inst) = &v else {
            return v;
        };
        let class_name = inst.borrow().class_name.clone();
        let Some(fn_idx) = self.find_method_arity(&class_name, "string", Some(0)) else {
            return v;
        };
        match self.run_function(fn_idx, vec![v.clone()]) {
            Ok(s) | Err(Outcome::Return(s)) => {
                crate::value::DISPLAY_TOP_LEVEL_BARE.with(|c| c.set(true));
                s
            }
            _ => v,
        }
    }
}
