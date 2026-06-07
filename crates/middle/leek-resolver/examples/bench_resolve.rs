use std::time::{Duration, Instant};

fn main() {
    let big = "var x = 1; var y = 2; var z = x + y;\n".repeat(200);
    let texts: &[(&str, &str)] = &[
        (
            "simple",
            "var n = 1000; var s = 0; for (var i = 0; i < n; i++) s += i; return s;",
        ),
        (
            "knapsack",
            "var items = [[37, 3], [47, 10], [28, 5]]; var all = []; var aux; aux = function(@current, i, tp, added, last) { if (count(current[1])) push(all, current); var item_count = count(items); for (var j = i; j < item_count; ++j) { var item = @items[j]; var cost = item[1]; if (cost > tp) continue; var copy = current; push(copy[1], @[item, cost, 1]); aux(copy, j, tp - cost, [], item[0]); } }; aux([0, []], 0, 25, [], -1); return count(all);",
        ),
        ("biggie", big.as_str()),
    ];
    let src = leek_span::SourceId::new(1).unwrap();
    let v = leek_syntax::Version::V4;

    for (name, text) in texts {
        // Time each phase in isolation. We always re-parse from
        // text so each measurement is independent of the previous.
        let mut sum_lex = Duration::ZERO;
        let mut sum_parse_relex = Duration::ZERO;
        let mut sum_parse_tokens = Duration::ZERO;
        let mut sum_resolve = Duration::ZERO;
        let mut sum_hir = Duration::ZERO;
        let n = 200;
        // warm up
        let _ = leek_parser::parse(text, src, v);
        for _ in 0..n {
            let t = Instant::now();
            let lex = leek_lexer::lex(text, src, v);
            sum_lex += t.elapsed();

            let t = Instant::now();
            let _ = leek_parser::parse(text, src, v);
            sum_parse_relex += t.elapsed();

            let t = Instant::now();
            let result = leek_parser::parse_tokens(text, src, &lex.tokens, v);
            sum_parse_tokens += t.elapsed();

            let root = leek_syntax::SyntaxNode::new_root(result.green.clone());
            let sf: leek_parser::ast::SourceFile =
                <leek_parser::ast::SourceFile as leek_parser::ast::AstNode>::cast(root).unwrap();

            let t = Instant::now();
            let _ = leek_resolver::resolve_with_version(&sf, src, v);
            sum_resolve += t.elapsed();

            let t = Instant::now();
            let _ = leek_hir::lower_file(&sf, src);
            sum_hir += t.elapsed();
        }
        println!(
            "{name:>12}  lex {:>9?}  parse(relex) {:>9?}  parse(tokens) {:>9?}  resolve {:>9?}  hir {:>9?}  ({} bytes)",
            sum_lex / n,
            sum_parse_relex / n,
            sum_parse_tokens / n,
            sum_resolve / n,
            sum_hir / n,
            text.len(),
        );
    }
}
