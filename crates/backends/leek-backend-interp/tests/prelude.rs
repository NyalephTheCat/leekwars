//! Implicit-prelude builtins under the interpreter: a bodiless
//! signature-file function dispatches the runtime builtin by name.

use leek_backend_interp::run;
use leek_hir::lower_file_with_prelude;
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse, parse_with_features};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

#[test]
fn prelude_builtin_runs_under_interp() {
    let prelude_src = "// @experimental: function_signatures\n\
/** @java-backend: Math.abs(%0)\n    @native-backend: abs */\n\
function abs(real x) -> real;\n";
    let user_src = "return abs(-5)\n";
    let source = SourceId::new(1).unwrap();
    let prelude_source = SourceId::new(0xF00D).unwrap();

    let p = parse_with_features(
        prelude_src,
        prelude_source,
        Version::V4,
        ParseFeatures {
            function_signatures: true,
            ..Default::default()
        },
    );
    let prelude_ast = SourceFile::cast(SyntaxNode::new_root(p.green)).expect("prelude");
    let u = parse(user_src, source, Version::V4);
    let user_ast = SourceFile::cast(SyntaxNode::new_root(u.green)).expect("user");

    let (hir, _diags) =
        lower_file_with_prelude(&user_ast, source, 4, &prelude_ast, prelude_source);
    let result = run(&hir);
    assert_eq!(result.error, None, "no runtime error");
    assert_eq!(
        result.value.to_string(),
        "5",
        "abs(-5) via prelude under interp"
    );
}
