//! TEMPORARY scratch: print emitted Java for drift snippets. DELETE ME.
use leek_parser::ast::AstNode;

#[test]
fn emit_one() {
    for (src, version) in [
        (
            "function te(a){ return function(b){ return function(c){return a*b*c;}; }; } return te(2)(1)(2);",
            2u8,
        ),
        ("var f = x => x * 2 return f(5)", 1u8),
        ("return Real.MIN_VALUE.class", 4u8),
        ("if (true) { return {a: 12} } else { return {b: 5} }", 2u8),
    ] {
        let s = leek_span::SourceId::new(1).unwrap();
        let v = match version {
            1 => leek_syntax::Version::V1,
            2 => leek_syntax::Version::V2,
            3 => leek_syntax::Version::V3,
            _ => leek_syntax::Version::V4,
        };
        let p = leek_parser::parse(src, s, v);
        let sf = leek_parser::ast::SourceFile::cast(leek_syntax::SyntaxNode::new_root(p.green))
            .expect("parse");
        let (h, _) = leek_hir::lower_file_versioned(&sf, s, version);
        let j = leek_backend_java::emit_exact(&h, v, 1);
        println!("==== v{version}: {src}\n{}", j.java);
    }
}
