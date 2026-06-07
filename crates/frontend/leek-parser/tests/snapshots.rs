//! Insta snapshot tests for parser CST shapes.
//!
//! Run `cargo insta review` after intentional grammar changes.

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxElement, SyntaxNode, Version};

fn src() -> SourceId {
    SourceId::new(1).unwrap()
}

fn dump(node: &SyntaxNode) -> String {
    let mut out = String::new();
    fn walk(node: &SyntaxNode, depth: usize, out: &mut String) {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&format!("{:?}\n", node.kind()));
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Node(n) => walk(&n, depth + 1, out),
                SyntaxElement::Token(t) => {
                    if t.kind().is_trivia() {
                        continue;
                    }
                    out.push_str(&"  ".repeat(depth + 1));
                    out.push_str(&format!("{:?} {:?}\n", t.kind(), t.text()));
                }
            }
        }
    }
    walk(node, 0, &mut out);
    out
}

fn snapshot_tree(name: &str, text: &str) {
    let result = parse(text, src(), Version::V4);
    let node = SyntaxNode::new_root(result.green);
    insta::assert_snapshot!(name, dump(&node));
}

#[test]
fn snapshot_var_decl() {
    snapshot_tree("var_decl", "var x = 1;");
}

#[test]
fn snapshot_function() {
    snapshot_tree("function", "function add(a, b) { return a + b; }\n");
}

#[test]
fn snapshot_class() {
    snapshot_tree(
        "class",
        "class Point { var x = 0; function getX() { return x; } }\n",
    );
}

#[test]
fn snapshot_if_else() {
    snapshot_tree("if_else", "if (x > 0) { return 1; } else { return 0; }\n");
}
