//! Reentrancy: the same `HirFile` compiled and run repeatedly in one process,
//! and one run started from inside another.
//!
//! This is how the fight generator drives an AI — `run` per turn and `run_call`
//! per hook, all against one `HirFile` — so every run must start from the same
//! state as a fresh one. Each run reclaims the module's executable memory
//! (`free_memory`) and sweeps its value handles (`free_run_boxes`) while the
//! runtime's thread-locals (op counter, globals, dispatch tables, recorded
//! error) live on, so a missed reset shows up only on the *second* run — which
//! no other test in this crate performs.
//!
//! One fight goes further and *nests* runs: a plant waking runs its AI from
//! inside a builtin call made by the entity whose turn it interrupted. There
//! the same resets must not happen — the outer run is still using what they
//! would clear — so the nested run takes that state aside and hands it back.

use std::cell::RefCell;
use std::rc::Rc;

use leek_backend_native::ids::fn_id;
use leek_backend_native::set_game_runtime;
use leek_backend_native::{GameRuntime, NativeError, NativeOptions, ops_used, run, run_call};
use leek_hir::{Def, DefId, HirFile};
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_runtime::{Function, Value};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn hir_v4(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(src, source, Version::V4, ParseFeatures::default());
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    leek_hir::lower_file_versioned(&sf, source, 4).0
}

fn opts() -> NativeOptions {
    NativeOptions::release().with_lang(4, false)
}

/// Options that force-compile `name` as a root, exactly as
/// `leek_generator::official::run_hooks` does — without this the hook is
/// unreachable from `main` and gets dropped as dead code, so `run_call` would
/// find nothing to dispatch to.
fn hook_opts(name: &str) -> NativeOptions {
    opts().with_hook_roots(vec![name.to_string()])
}

/// The `Value::Function` the generator builds for a top-level zero-arg function
/// (the port of `EntityAI.findHookMethod`).
fn hook_value(hir: &HirFile, name: &str) -> Value {
    hir.defs
        .iter()
        .enumerate()
        .find_map(|(i, def)| match def {
            Def::Function(f) if f.name == name && f.params.is_empty() => u32::try_from(i)
                .ok()
                .map(|id| Value::Function(Function::User(fn_id(DefId(id))))),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no zero-arg top-level function `{name}`"))
}

#[test]
fn running_the_same_program_repeatedly_gives_the_same_value_and_op_count() {
    // Globals, the op counter and the recorded runtime error are all per-run
    // thread-local state reset at run setup. If any of them carried over, the
    // second run's value or charge would drift from the first's.
    let hir = hir_v4(
        "global acc = 0\nfor (var i = 0; i < 10; i++) { acc = acc + i }\nvar s = \"n=\" + acc\nreturn s\n",
    );
    let o = opts();
    let mut seen: Option<(String, u64)> = None;
    for n in 0..3 {
        let v = run(&hir, &o).expect("run").to_string();
        let ops = ops_used();
        match &seen {
            None => seen = Some((v, ops)),
            Some((want_v, want_ops)) => {
                assert_eq!(&v, want_v, "run {n} produced a different value");
                assert_eq!(ops, *want_ops, "run {n} charged a different op count");
            }
        }
    }
    // v4 displays a top-level string quoted.
    assert_eq!(seen.expect("ran").0, "\"n=45\"");
}

#[test]
fn a_hook_can_be_called_after_a_normal_run_of_the_same_program() {
    let src = "function hook() { return 40 + 2 }\nreturn 1\n";
    let hir = hir_v4(src);
    assert_eq!(run(&hir, &opts()).expect("run").to_string(), "1");
    let v = run_call(
        &hir,
        &hook_opts("hook"),
        &hook_value(&hir, "hook"),
        Vec::new(),
    )
    .expect("run_call");
    assert_eq!(v.to_string(), "42");
}

#[test]
fn run_call_does_not_initialize_globals() {
    // Documented contract (`run_call`'s doc comment): `main` never runs, so the
    // file's globals are unset. The generator depends on this — matching Java
    // would need the owner's live global state, which the per-turn re-run model
    // does not keep. Pin it so it can't drift silently.
    let hir = hir_v4("global g = 5\nfunction hook() { return g }\nreturn g\n");
    assert_eq!(run(&hir, &opts()).expect("run").to_string(), "5");
    let v = run_call(
        &hir,
        &hook_opts("hook"),
        &hook_value(&hir, "hook"),
        Vec::new(),
    )
    .expect("run_call");
    assert_eq!(
        v.to_string(),
        "null",
        "run_call must not initialize globals"
    );
}

#[test]
fn run_call_is_repeatable_on_the_same_program() {
    // Each `run_call` re-JITs the module and reinstalls the dispatch tables; the
    // previous call's tables held addresses into memory `free_memory` released,
    // so a stale entry would surface here (as a wrong value or a crash).
    let hir = hir_v4("function hook() { var a = [1, 2, 3]\nreturn count(a) }\nreturn 0\n");
    let o = hook_opts("hook");
    let callee = hook_value(&hir, "hook");
    for n in 0..3 {
        let v = run_call(&hir, &o, &callee, Vec::new()).expect("run_call");
        assert_eq!(v.to_string(), "3", "run_call {n}");
    }
}

#[test]
fn interleaving_run_and_run_call_leaves_run_unchanged() {
    // `run_call` arms the same per-run state `run` does; a `run` that follows one
    // must be indistinguishable from a fresh one.
    let hir = hir_v4("global g = 0\nfunction hook() { return 7 }\ng = g + 3\nreturn g\n");
    let o = opts();
    let hooked = hook_opts("hook");
    let callee = hook_value(&hir, "hook");

    let want = run(&hir, &o).expect("first run").to_string();
    let want_ops = ops_used();

    assert_eq!(
        run_call(&hir, &hooked, &callee, Vec::new())
            .expect("run_call")
            .to_string(),
        "7"
    );

    assert_eq!(run(&hir, &o).expect("second run").to_string(), want);
    assert_eq!(ops_used(), want_ops);
}

#[test]
fn a_run_that_trips_the_op_budget_does_not_poison_the_next_run() {
    // `take_runtime_error` clears the recorded fault as it reports it; if it
    // didn't, every later run of any program on this thread would fail with the
    // stale code.
    let runaway = hir_v4("var a = 0 for (var i = 0; i < 100000000; ++i) a = a + 1 return a");
    let clean = hir_v4("var x = 1 + 1\nreturn x\n");

    match run(&runaway, &opts().with_op_limit(10_000)) {
        Err(e) if e.runtime_code().is_some() => {
            assert_eq!(e.reason(), "TOO_MUCH_OPERATIONS");
        }
        other => panic!("expected an op-budget trip, got {other:?}"),
    }
    assert_eq!(
        run(&clean, &opts())
            .expect("a clean run after a faulted one")
            .to_string(),
        "2"
    );
}

/// A game runtime that runs a whole second program from inside one builtin
/// call — what a fight does when a plant wakes in the middle of another
/// entity's turn. The nested program runs on a *tight* op budget of its own,
/// as an awakening does.
struct Reentrant {
    inner: HirFile,
    opts: NativeOptions,
    ran: Rc<RefCell<Option<Result<Value, NativeError>>>>,
}

/// The innermost runtime: a plant's own builtins have to find something
/// installed, because `dispatch` moves the current runtime out for the
/// duration of the call it is serving.
struct Inert;

impl GameRuntime for Inert {
    fn call(&mut self, _name: &str, _args: &[Value]) -> Value {
        Value::Null
    }
}

impl GameRuntime for Reentrant {
    fn call(&mut self, _name: &str, _args: &[Value]) -> Value {
        set_game_runtime(Some(Box::new(Inert)));
        let out = run(&self.inner, &self.opts);
        set_game_runtime(None);
        *self.ran.borrow_mut() = Some(out);
        Value::Int(7)
    }
}

/// A run nested inside another run leaves the outer one standing: the outer
/// program keeps its own op budget, its arrays stay readable (the nested run
/// must not sweep the box arena they live in) and its value comes out whole.
///
/// The budgets are what make this observable. The nested run is given a tiny
/// one — enough for itself, nowhere near enough for the loop the outer run
/// still has to get through — so an unrestored `OP_LIMIT` ends the outer run
/// in `TOO_MUCH_OPERATIONS` instead of a value.
#[test]
fn a_nested_run_leaves_the_outer_run_standing() {
    let base = opts().with_link_game(true);
    let ran = Rc::new(RefCell::new(None));
    let inner = hir_v4("var s = 0 for (var i = 0; i < 3; i++) { s += i } return s");
    set_game_runtime(Some(Box::new(Reentrant {
        inner,
        opts: base.clone().with_op_limit(400),
        ran: Rc::clone(&ran),
    })));
    // `kept` is an array built before the builtin call and read after it; the
    // loop after the call costs far more than the nested run was allowed.
    let outer = run(
        &hir_v4(
            "var kept = [10, 20, 30] \
             var mid = getLife() \
             var acc = 0 \
             for (var i = 0; i < 200; i++) { acc += i } \
             return kept[0] + kept[1] + kept[2] + mid",
        ),
        &base.clone().with_op_limit(100_000),
    );
    set_game_runtime(None);

    assert!(
        matches!(&*ran.borrow(), Some(Ok(Value::Int(3)))),
        "the nested run produced {:?}",
        ran.borrow()
    );
    match outer {
        Ok(Value::Int(67)) => {} // 10 + 20 + 30 + 7
        other => panic!("outer run came back {other:?}"),
    }
}
