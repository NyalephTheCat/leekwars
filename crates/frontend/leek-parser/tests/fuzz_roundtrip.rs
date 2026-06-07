//! Fuzz target (P2 #25): the parser must never panic and must always be
//! lossless — the text reconstructed from the green tree equals the input
//! byte-for-byte — for *arbitrary* input, not just well-formed programs.
//!
//! Deterministic (fixed-seed LCG) so failures reproduce; no external fuzzing
//! harness needed. Covers three sources: hand-picked malformed edge cases,
//! random token soup, and mutations of valid snippets.

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn roundtrips(text: &str) {
    // Parsing must not panic (recursion guard, error recovery) …
    let result = parse(text, SourceId::new(1).unwrap(), Version::LATEST);
    let node = SyntaxNode::new_root(result.green);
    // … and the CST must losslessly reproduce the input.
    assert_eq!(
        node.text().to_string(),
        text,
        "round-trip mismatch for input {text:?}",
    );
}

#[test]
fn malformed_edge_cases_dont_panic_and_roundtrip() {
    let cases = [
        "",
        " ",
        "\n\n\t",
        "{",
        "}",
        "(((",
        ")))",
        "[",
        "var",
        "var x =",
        "= 5",
        "function",
        "function (",
        "class",
        "class {",
        "if",
        "if (",
        "return return return",
        "1 2 3 4 5",
        "+++---",
        "@#$%^&",
        "var x = \"unterminated",
        "/* unterminated block",
        "0x 0b 1.2.3 1e",
        "=> -> => ->",
        "[1:2:3:4]",
        "a.b.c.d.e.f.g",
        "  // trailing comment",
        "é à ü 漢字 🦀",
        "var \u{0}x = 1",
        "for(;;)",
        ";;;;;;",
        "..",
        "...",
        "?:??::",
    ];
    for c in cases {
        roundtrips(c);
    }
}

#[test]
fn random_token_soup_doesnt_panic_and_roundtrips() {
    // A small alphabet of lexically-meaningful fragments; random sequences of
    // them stress error recovery without needing a real grammar.
    const FRAGS: &[&str] = &[
        "var ", "x", "y", "=", "1", "2.5", "+", "-", "*", "(", ")", "{", "}", "[", "]",
        ";", ",", ".", "function", "return ", "if", "else", "=>", "->", "\"s\"", " ",
        "\n", "class", ":", "?", "for", "while", "//c\n", "true", "null", "@", "&&",
    ];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15; // fixed seed
    let mut next = || {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3000 {
        let len = (next() % 24) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push_str(FRAGS[(next() as usize) % FRAGS.len()]);
        }
        roundtrips(&s);
    }
}

#[test]
fn byte_mutations_of_valid_programs_roundtrip() {
    // Take valid programs and corrupt single bytes; the parser must recover
    // (no panic) and stay lossless.
    let seeds = [
        "var x = 1 + 2 * 3 return x",
        "function f(a, b) { return a + b } return f(1, 2)",
        "class A { private p = 1 public m() { return this.p } }",
        "var m = [1 : 2, 3 : 4] for (var k : var v in m) { debug(v) }",
        "var g = (x, y) => x * y return g(3, 4)",
    ];
    let inject = [b'{', b'}', b'(', b')', b'"', b'\\', 0, b';', b'@', b'='];
    for seed in seeds {
        let bytes = seed.as_bytes();
        for (i, _) in bytes.iter().enumerate() {
            for &b in &inject {
                let mut v = bytes.to_vec();
                v[i] = b;
                // Keep it valid UTF-8 (the parser takes &str); skip if a 0-byte
                // or replacement breaks a multibyte boundary.
                if let Ok(s) = std::str::from_utf8(&v) {
                    roundtrips(s);
                }
            }
        }
    }
}
