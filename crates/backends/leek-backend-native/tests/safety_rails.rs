//! Native safety rails: a runaway AI must end in a runtime error, never hang,
//! crash or abort the host process, and must stop acting once it has errored.

use std::cell::RefCell;
use std::rc::Rc;

use leek_backend_native::{GameRuntime, NativeError, NativeOptions, run, set_game_runtime};
use leek_parser::{ast::AstNode, parse};
use leek_runtime::Value;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn hir(src: &str) -> leek_hir::HirFile {
    let s = SourceId::new(1).unwrap();
    let p = parse(src, s, Version::V4);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("parse");
    leek_hir::lower_file_versioned(&sf, s, 4).0
}

/// A game runtime whose every call panics.
struct Panicking;

impl GameRuntime for Panicking {
    fn call(&mut self, _name: &str, _args: &[Value]) -> Value {
        panic!("game runtime bug");
    }
}

#[test]
fn a_panicking_shim_becomes_a_runtime_error_instead_of_aborting() {
    let h = hir("var life = getLife() return life");
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_link_game(true);
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    set_game_runtime(Some(Box::new(Panicking)));
    let out = run(&h, &opts);
    set_game_runtime(None);
    std::panic::set_hook(hook);
    match out {
        Err(NativeError::Runtime(code)) => assert_eq!(code, "INTERNAL_PANIC"),
        other => panic!("expected INTERNAL_PANIC, got {other:?}"),
    }
}

/// Records every game function the AI calls.
struct Recorder(Rc<RefCell<Vec<String>>>);

impl GameRuntime for Recorder {
    fn call(&mut self, name: &str, _args: &[Value]) -> Value {
        self.0.borrow_mut().push(name.to_string());
        Value::Null
    }
}

/// Run `src` with the fight builtins linked to a [`Recorder`], returning the
/// run's outcome and the game calls it made.
fn run_recording(src: &str, opts: &NativeOptions) -> (Result<Value, NativeError>, Vec<String>) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    set_game_runtime(Some(Box::new(Recorder(Rc::clone(&calls)))));
    let out = run(&hir(src), &opts.clone().with_link_game(true));
    set_game_runtime(None);
    let calls = calls.borrow().clone();
    (out, calls)
}

#[test]
fn no_game_action_after_the_op_budget_runs_out_mid_block() {
    // Straight-line block: the concat charges 3 + 200 ops at runtime, blowing
    // the 100-op budget before `useWeapon` runs. Upstream throws on the
    // concat, so the action must never reach the fight.
    let long = "a".repeat(100);
    let src = format!("var a = \"{long}\" var b = a + a useWeapon(1) return b");
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_op_limit(100);
    let (out, calls) = run_recording(&src, &opts);
    assert!(
        matches!(&out, Err(NativeError::Runtime(c)) if c == "TOO_MUCH_OPERATIONS"),
        "expected TOO_MUCH_OPERATIONS, got {out:?}"
    );
    assert!(
        calls.is_empty(),
        "game actions ran after the error: {calls:?}"
    );
}

#[test]
fn no_game_action_after_a_strict_out_of_bounds_write() {
    let src = "var a = [1] a[5] = 2 useWeapon(1) return a";
    let opts = NativeOptions::release().with_lang(4, true);
    let (out, calls) = run_recording(src, &opts);
    assert!(
        matches!(&out, Err(NativeError::Runtime(c)) if c == "ARRAY_OUT_OF_BOUND"),
        "expected ARRAY_OUT_OF_BOUND, got {out:?}"
    );
    assert!(
        calls.is_empty(),
        "game actions ran after the error: {calls:?}"
    );
}

#[test]
fn game_actions_before_an_error_still_happen() {
    // Only what follows the fault is suppressed.
    let src = "useWeapon(1) var a = [1] a[5] = 2 moveToward(3) return a";
    let opts = NativeOptions::release().with_lang(4, true);
    let (out, calls) = run_recording(src, &opts);
    assert!(matches!(out, Err(NativeError::Runtime(_))), "got {out:?}");
    assert_eq!(calls, vec!["useWeapon".to_string()]);
}

fn outcome(src: &str, opts: &NativeOptions) -> String {
    match run(&hir(src), opts) {
        Ok(v) => v.to_string(),
        Err(NativeError::Runtime(code)) => format!("runtime error {code}"),
        Err(e) => format!("other error: {e}"),
    }
}

const STACK_OVERFLOW: &str = "runtime error STACKOVERFLOW";

#[test]
fn unbounded_recursion_raises_stack_overflow_instead_of_crashing() {
    // Runs on a default (2 MiB) test thread with the default depth limit: the
    // guard must trip before the native stack is exhausted, on every call path.
    let opts = NativeOptions::release().with_lang(4, false);
    for src in [
        // Direct user-function recursion (JIT-to-JIT calls).
        "function f(x) { return f(x) } return f(1)",
        // Mutual recursion.
        "function a(x) { return b(x) } function b(x) { return a(x) } return a(1)",
        // Recursion through a function value (crosses the Rust dispatch shims).
        "var g = function(x) { return g(x) } return g(1)",
        // Method recursion.
        "class A { m(x) { return this.m(x) } } return new A().m(1)",
    ] {
        assert_eq!(outcome(src, &opts), STACK_OVERFLOW, "{src}");
    }
}

#[test]
fn call_depth_limit_is_configurable() {
    let rec = |n: u32| {
        format!("function rec(n) {{ if (n == 0) return 0 return 1 + rec(n - 1) }} return rec({n})")
    };
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_max_call_depth(50);
    // `rec(n)` nests n + 1 frames.
    assert_eq!(outcome(&rec(49), &opts), "49");
    assert_eq!(outcome(&rec(50), &opts), STACK_OVERFLOW);
}

#[test]
fn stack_budget_stops_recursion_the_depth_limit_allows() {
    let src = "var g = function(x) { return g(x) } return g(1)";
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_max_call_depth(u32::MAX)
        .with_max_stack_bytes(64 * 1024);
    assert_eq!(outcome(src, &opts), STACK_OVERFLOW);
}

#[test]
fn default_limit_admits_the_corpus_recursion_depth() {
    // Upstream `rec(1000)` completes (`max_ops(50000).equals("1000")`).
    let src = "function rec(n) { if (n == 0) return 0 return 1 + rec(n - 1) } return rec(1000)";
    assert_eq!(
        outcome(src, &NativeOptions::release().with_lang(4, false)),
        "1000"
    );
}

#[test]
fn returns_pop_their_frames() {
    // Many sequential calls, each returning through a different path, never
    // accumulate depth: a limit of 3 frames is plenty.
    let src = "function f(x) { if (x > 0) return 1 } \
               function g() { f(1) f(0) } \
               var s = 0 for (var i = 0; i < 10000; i++) { g() s++ } return s";
    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_max_call_depth(3);
    assert_eq!(outcome(src, &opts), "10000");
}

/// Run `src` with no op limit on its own thread, failing if it doesn't finish
/// within a deadline — an unbounded run can only stop through the back-edge
/// abort poll.
fn unbounded_outcome_within_deadline(src: &'static str, strict: bool) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let opts = NativeOptions::release().with_lang(4, strict);
        let _ = tx.send(outcome(src, &opts));
    });
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("`{src}` never stopped after its runtime error"))
}

#[test]
fn unbounded_loop_stops_after_a_callee_overflows_the_stack() {
    // With no op budget, every call after the overflow returns at its entry
    // prologue; the caller's loop must still poll the abort flag and stop.
    for src in [
        "function f(x) { return f(x) } while (true) { f(1) } return 0",
        "function f(x) { return f(x) } for (;;) { f(1) } return 0",
    ] {
        assert_eq!(
            unbounded_outcome_within_deadline(src, false),
            STACK_OVERFLOW,
            "{src}"
        );
    }
}

#[test]
fn unbounded_loop_stops_after_a_strict_out_of_bounds_write() {
    for src in [
        "var a = [1] for (;;) { a[5] = 2 } return a",
        "var a = [1] a[5] = 2 while (true) {} return a",
    ] {
        assert_eq!(
            unbounded_outcome_within_deadline(src, true),
            "runtime error ARRAY_OUT_OF_BOUND",
            "{src}"
        );
    }
}
