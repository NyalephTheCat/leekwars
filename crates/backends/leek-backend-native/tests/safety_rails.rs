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
