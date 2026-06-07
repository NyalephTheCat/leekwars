//! End-to-end profiler tests: run a fixture through the
//! interpreter with `set_profiler` enabled and confirm the
//! resulting folded-stack samples have the shape we expect.

use leek_backend_interp::{Interpreter, Profiler};
use leek_diagnostics::Severity;
use leek_hir::lower_file;
use leek_mir::lower_file as lower_mir_file;
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn run_with_profiler(src: &str) -> Profiler {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    assert!(
        !parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "parse errors: {:?}",
        parsed.diagnostics,
    );
    let root = SyntaxNode::new_root(parsed.green.clone());
    let file = SourceFile::cast(root).expect("source file");
    let (hir, _) = lower_file(&file, source);
    let (program, errs) = lower_mir_file(&hir);
    assert!(errs.is_empty(), "mir lowering: {errs:?}");
    let mut interp = Interpreter::with_op_limit(&program, 1_000_000);
    interp.set_version(4);
    interp.set_profiler(Profiler::new());
    let _ = interp.run();
    interp.take_profiler().expect("profiler present")
}

#[test]
fn straight_line_program_has_one_main_frame() {
    let p = run_with_profiler("return 1 + 2\n");
    let lines = p.folded_lines();
    // `<main>` should be the only frame.
    assert_eq!(lines.len(), 1, "lines: {lines:?}");
    assert!(lines[0].starts_with("<main>"));
}

#[test]
fn nested_calls_produce_one_frame_per_callee() {
    let p = run_with_profiler(
        "\
function inner() { return 1 }\n\
function outer() { return inner() + inner() }\n\
return outer()\n",
    );
    let samples = p.samples();
    // Expect frames for <main>, outer, inner under their stacks.
    let has_main = samples.keys().any(|k| k == &vec!["<main>".to_string()]);
    let has_outer = samples
        .keys()
        .any(|k| k == &vec!["<main>".to_string(), "outer".to_string()]);
    let has_inner = samples.keys().any(|k| {
        k == &vec![
            "<main>".to_string(),
            "outer".to_string(),
            "inner".to_string(),
        ]
    });
    assert!(has_main, "missing <main> frame: {samples:?}");
    assert!(has_outer, "missing outer frame: {samples:?}");
    assert!(has_inner, "missing inner frame: {samples:?}");
}

#[test]
fn linear_loop_attributes_most_ops_to_the_loop_function() {
    let p = run_with_profiler(
        "\
function work(arr) {\n\
    var t = 0\n\
    for (var x in arr) { t = t + x }\n\
    return t\n\
}\n\
return work([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])\n",
    );
    let samples = p.samples();
    // `work` frame should hold strictly more self-ops than the
    // bare main wrapper (which just builds the array literal).
    let work_path = vec!["<main>".to_string(), "work".to_string()];
    let work_ops = samples.get(&work_path).copied().unwrap_or(0);
    let main_ops = samples
        .get(&vec!["<main>".to_string()])
        .copied()
        .unwrap_or(0);
    assert!(
        work_ops > main_ops,
        "expected work > main; got work={work_ops} main={main_ops}\nsamples={samples:?}",
    );
}

#[test]
fn folded_lines_are_sorted_and_well_formed() {
    let p = run_with_profiler(
        "\
function a() { return 1 }\n\
function b() { return a() }\n\
return b()\n",
    );
    let lines = p.folded_lines();
    // Each line: `frame1;frame2;... N`.
    for l in &lines {
        let last_space = l.rfind(' ').expect("expected `N` suffix");
        let n: u64 = l[last_space + 1..].parse().expect("count is a u64");
        assert!(n > 0, "line had a zero count: {l}");
    }
    // Sorted.
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted, "folded lines should be sorted");
}
