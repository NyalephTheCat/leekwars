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
