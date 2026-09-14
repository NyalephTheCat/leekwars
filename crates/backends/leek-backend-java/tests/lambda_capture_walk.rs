//! Regression tests for the capture walkers in `emit::lambda`.
//!
//! `lambda_writes_to_outer` and `lambda_references_initializing_def`
//! used to enumerate `ExprKind` by hand and end in a `_ => false`
//! catch-all, so the arbitrary sub-expressions a slice or an interval
//! holds were invisible to them. A capture written only from a slice
//! bound was therefore never boxed, and the outlined factory took it
//! as a `final` parameter — javac's "cannot assign to final variable".
//! Both walkers now delegate to `leek_hir::walk_expr_children`, whose
//! `match` is variant-complete.

use leek_backend_java::{Options, emit};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn java_for(src: &str) -> String {
    let source = SourceId::new(1).unwrap();
    let opts = Options::exact(Version::V4, 7);
    let parsed = parse(src, source, opts.version);
    let root = SyntaxNode::new_root(parsed.green);
    let sf = leek_parser::ast::SourceFile::cast(root).expect("parse");
    let (hir, _diags) = leek_hir::lower_file(&sf, source);
    emit(&hir, &opts).java
}

/// A capture whose only write sits in a slice bound still has to be
/// boxed: `Object[] u_acc = …` plus `u_acc[0]` accesses, not a plain
/// `Object u_acc` handed to the factory as a `final` parameter.
#[test]
fn capture_written_only_in_a_slice_bound_is_boxed() {
    let java = java_for(
        "// @version:4\nvar acc = 0\nvar arr = [1, 2, 3, 4]\n\
         var f = function() { return arr[acc++ : 3] }\nreturn f()\n",
    );
    assert!(java.contains("Object[] u_acc = new Object[]{"), "{java}");
}

/// Same for an interval bound (`[a..b]`), the other `ExprKind` that
/// holds arbitrary children behind a struct.
#[test]
fn capture_written_only_in_an_interval_bound_is_boxed() {
    let java = java_for(
        "// @version:4\nvar acc = 0\n\
         var f = function() { return [acc++ .. 3] }\nreturn f()\n",
    );
    assert!(java.contains("Object[] u_acc = new Object[]{"), "{java}");
}
