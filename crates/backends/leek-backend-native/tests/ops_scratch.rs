//! TEMPORARY scratch decomposition for ops-parity debugging. DELETE ME.

use leek_backend_native::{NativeOptions, ops_used, run};
use leek_parser::{ast::AstNode, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn ops_v(src: &str, version: u8) -> u64 {
    let s = SourceId::new(1).unwrap();
    let v = match version {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        _ => Version::V4,
    };
    let p = parse(src, s, v);
    let sf = leek_parser::ast::SourceFile::cast(SyntaxNode::new_root(p.green)).expect("parse");
    let (h, _) = leek_hir::lower_file_versioned(&sf, s, version);
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    run(&h, &NativeOptions::release().with_lang(version, false)).expect("run");
    ops_used()
}

#[test]
fn scratch() {
    for (label, src, vers) in [
        ("uninit-decl", "real? a a >= 0", &[1u8, 2, 3, 4][..]),
        (
            "arraymap-lambda",
            "var x = arrayMap([1, 2, 3], function(x) { return x });",
            &[2, 3][..],
        ),
        (
            "arraymap-empty",
            "var x = arrayMap([], function(x) { return x });",
            &[2, 3][..],
        ),
        ("literal3", "var x = [1, 2, 3];", &[2, 3][..]),
        (
            "xor-assign",
            "var a = 87619 return a ^= 18431;",
            &[2, 4][..],
        ),
        ("concat", "return 1+','+2", &[2, 4][..]),
        (
            "foreach",
            "var r = [] for (var x in [1, 2, 3]) { push(r, x) }",
            &[1, 4][..],
        ),
        (
            "foreach-empty",
            "var r = [] for (var x in []) { push(r, x) }",
            &[1, 4][..],
        ),
        (
            "while-push",
            "var s = [] var i = 0 var j = 0 while (i < 2) { i++ j = 0 while (j < 3) { j++ push(s, j) }} return s;",
            &[4][..],
        ),
        (
            "while-nopush",
            "var i = 0 var j = 0 while (i < 2) { i++ j = 0 while (j < 3) { j++ }}",
            &[4][..],
        ),
        ("push-only", "var s = [] push(s, 1)", &[1, 4][..]),
        ("lit3", "var x = [1, 2, 3];", &[2, 4][..]),
        ("lit1", "var x = [1];", &[2, 4][..]),
        (
            "am1",
            "var x = arrayMap([1], function(x) { return x });",
            &[2][..],
        ),
        (
            "am2",
            "var x = arrayMap([1, 2], function(x) { return x });",
            &[2][..],
        ),
        (
            "lamcall",
            "var f = function(x) { return x } var y = f(1)",
            &[2][..],
        ),
        (
            "lamcall-v1",
            "var f = function(x) { return x } var y = f(1)",
            &[1][..],
        ),
        (
            "fncall-v1",
            "function g(a, b) { return a } g(1, 2)",
            &[1, 2, 4][..],
        ),
        (
            "curried",
            "function te(a) { return function(b) { return function(c) { return a * b * c } } } return te(2)(1)(2)",
            &[2][..],
        ),
        ("amcos1", "var x = arrayMap([1], cos);", &[2][..]),
        ("ret-soft", "return? 5", &[1, 4][..]),
        ("streq", "return \"a\\\"b\" == 'a\"b'", &[2, 3][..]),
        ("strslice", "var s = 'hello' return s[1:4:2]", &[4][..]),
        (
            "brk",
            "for (var x = 0; x < 2; ++x) { var a = 'a' for (var y = 0; y < 2; ++y) { var b = 'b' break } var d = 'd' } return 0;",
            &[1, 4][..],
        ),
        (
            "cont",
            "var a = 0 var x = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] for (var i in x) { if (i < 5) { continue } a++ } return a;",
            &[1, 2, 4][..],
        ),
        (
            "ret-soft-fn",
            "function f(x) { return? x return 12 } f(5)",
            &[2, 4][..],
        ),
        ("concat2", "return 1 + ', ' + 2", &[2, 4][..]),
        ("strlen", "return length('hello')", &[1, 4][..]),
        ("stridx", "var s = 'hello' return s[0]", &[4][..]),
        ("stridx-neg", "var s = 'hello' return s[-1]", &[4][..]),
        ("stridx-lit", "return 'hello'[1]", &[4][..]),
        (
            "replace",
            "return replace('bonjour','onj','pro')",
            &[1, 4][..],
        ),
        (
            "contains200",
            "var adn = '' for (var j = 0; j < 200; j++) { adn += 'A' } var c = contains(adn, 'GAGA');",
            &[1, 4][..],
        ),
    ] {
        for &v in vers {
            println!("{label} v{v}: {}", ops_v(src, v));
        }
    }
}
