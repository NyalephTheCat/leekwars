//! The `debug_hooks` build emits a `leek_dbg_safepoint(offset, desc, values)`
//! call before every statement; verify the installed hook fires with offsets
//! that map to the right source lines, and that the frame's locals render to
//! the expected values.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use leek_backend_native::{frame_name, render_frame_vars, run, DebugHook, NativeOptions};
use leek_hir::lower_file_versioned;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::{LineTable, SourceId};
use leek_syntax::{SyntaxNode, Version};

/// The debug hook is process-global (one debug session per process), so
/// these tests must not run concurrently. Serialize them on this lock.
static SERIAL: Mutex<()> = Mutex::new(());

struct Recorder {
    lt: LineTable,
    hits: Mutex<Vec<(u32, Vec<(String, String)>)>>,
}

impl DebugHook for Recorder {
    fn safepoint(&self, offset: u32, desc: usize, values: usize) {
        let line = self.lt.line_col(offset).line;
        // Rendered on the debuggee thread while the frame is live — exactly
        // how the adapter captures locals at a stop.
        let vars = render_frame_vars(desc, values);
        self.hits.lock().unwrap().push((line, vars));
    }
}

#[test]
fn safepoints_fire_and_render_locals() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let src = "var x = 40\nvar y = 2\nreturn x + y\n";
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, 4).0;

    let rec = Arc::new(Recorder {
        lt: LineTable::new(src),
        hits: Mutex::new(Vec::new()),
    });
    leek_backend_native::set_debug_hook(Some(rec.clone()));
    let opts = NativeOptions::debug().with_lang(4, false).with_debug_hooks(true);
    let result = run(&hir, &opts);
    leek_backend_native::set_debug_hook(None);

    assert!(result.is_ok(), "native run failed: {result:?}");
    assert_eq!(result.unwrap().to_string(), "42");

    let hits = rec.hits.lock().unwrap();
    let lines: Vec<u32> = hits.iter().map(|(l, _)| *l).collect();
    for expected in [1u32, 2, 3] {
        assert!(lines.contains(&expected), "line {expected} never hit; lines={lines:?}");
    }

    // By the line-3 safepoint (before `return x + y`), both locals are set.
    let at_line3 = hits
        .iter()
        .rev()
        .find(|(l, _)| *l == 3)
        .map(|(_, vars)| vars.clone())
        .expect("a safepoint on line 3");
    assert!(
        at_line3.contains(&("x".to_string(), "40".to_string())),
        "expected x=40 at line 3, got {at_line3:?}"
    );
    assert!(
        at_line3.contains(&("y".to_string(), "2".to_string())),
        "expected y=2 at line 3, got {at_line3:?}"
    );
}

#[test]
fn renders_reference_locals() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // A string local is a boxed `Value` handle (kind 3); it should render
    // via the runtime's `Display`, not crash.
    let src = "var s = \"hi\"\nvar n = 7\nreturn count(s) + n\n";
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, 4).0;

    let rec = Arc::new(Recorder {
        lt: LineTable::new(src),
        hits: Mutex::new(Vec::new()),
    });
    leek_backend_native::set_debug_hook(Some(rec.clone()));
    let opts = NativeOptions::debug().with_lang(4, false).with_debug_hooks(true);
    let result = run(&hir, &opts);
    leek_backend_native::set_debug_hook(None);
    assert!(result.is_ok(), "native run failed: {result:?}");

    let hits = rec.hits.lock().unwrap();
    let at_line3 = hits
        .iter()
        .rev()
        .find(|(l, _)| *l == 3)
        .map(|(_, vars)| vars.clone())
        .expect("a safepoint on line 3");
    let s = at_line3
        .iter()
        .find(|(n, _)| n == "s")
        .map(|(_, v)| v.clone())
        .expect("local `s` present");
    assert!(s.contains("hi"), "expected `s` to render containing 'hi', got {s:?}");
}

#[test]
fn safepoint_on_bare_return_line() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // `return a` is a bare terminator with no statement on its line; the
    // per-terminator safepoint must still let a debugger stop there.
    let src = "var a = 5\nreturn a\n";
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, 4).0;

    let rec = Arc::new(Recorder {
        lt: LineTable::new(src),
        hits: Mutex::new(Vec::new()),
    });
    leek_backend_native::set_debug_hook(Some(rec.clone()));
    let opts = NativeOptions::debug().with_lang(4, false).with_debug_hooks(true);
    let result = run(&hir, &opts);
    leek_backend_native::set_debug_hook(None);
    assert_eq!(result.unwrap().to_string(), "5");

    let lines: Vec<u32> = rec.hits.lock().unwrap().iter().map(|(l, _)| *l).collect();
    assert!(lines.contains(&2), "no safepoint on the bare `return` line 2; lines={lines:?}");
}

/// Records the live call depth (via enter/leave) at every safepoint.
struct StackRec {
    lt: LineTable,
    depth: AtomicI32,
    /// (line, depth) at each safepoint, and the names seen on enter.
    samples: Mutex<Vec<(u32, i32)>>,
    names: Mutex<Vec<String>>,
}

impl DebugHook for StackRec {
    fn safepoint(&self, offset: u32, _desc: usize, _values: usize) {
        let line = self.lt.line_col(offset).line;
        let depth = self.depth.load(Ordering::SeqCst);
        self.samples.lock().unwrap().push((line, depth));
    }
    fn enter_frame(&self, desc: usize) {
        self.depth.fetch_add(1, Ordering::SeqCst);
        if let Some(name) = frame_name(desc) {
            self.names.lock().unwrap().push(name);
        }
    }
    fn leave_frame(&self) {
        self.depth.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn shadow_stack_tracks_call_depth() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // `inc` is called from the top level: a safepoint inside `inc` runs at
    // depth 2, the top-level ones at depth 1.
    let src = "function inc(x) { return x + 1 }\nvar r = inc(41)\nreturn r\n";
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
    let hir = lower_file_versioned(&sf, source, 4).0;

    let rec = Arc::new(StackRec {
        lt: LineTable::new(src),
        depth: AtomicI32::new(0),
        samples: Mutex::new(Vec::new()),
        names: Mutex::new(Vec::new()),
    });
    leek_backend_native::set_debug_hook(Some(rec.clone()));
    let opts = NativeOptions::debug().with_lang(4, false).with_debug_hooks(true);
    let result = run(&hir, &opts);
    leek_backend_native::set_debug_hook(None);
    assert_eq!(result.unwrap().to_string(), "42");

    let samples = rec.samples.lock().unwrap();
    // Inside `inc` (line 1) we're two frames deep; at the top level (line 2/3)
    // we're one frame deep.
    assert!(
        samples.iter().any(|&(l, d)| l == 1 && d == 2),
        "expected a depth-2 safepoint on line 1; samples={samples:?}"
    );
    assert!(
        samples.iter().any(|&(l, d)| l >= 2 && d == 1),
        "expected a depth-1 safepoint at the top level; samples={samples:?}"
    );
    // Frame must have fully unwound by program end.
    assert_eq!(rec.depth.load(Ordering::SeqCst), 0, "frames left unbalanced");
}
