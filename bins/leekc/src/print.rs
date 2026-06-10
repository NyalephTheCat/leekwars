//! HIR / MIR / CST pretty-printers.

use leek_syntax::{SyntaxKind, SyntaxNode};

pub(crate) fn print_tokens(text: &str, tokens: &[leek_syntax::Token]) {
    for tok in tokens {
        if tok.kind == SyntaxKind::Eof {
            println!("{:?}@{}..{}", tok.kind, tok.span.start, tok.span.end);
            break;
        }
        let slice = &text[tok.span.range()];
        let display = slice.replace('\n', "\\n").replace('\t', "\\t");
        println!(
            "{:?}@{}..{} {:?}",
            tok.kind, tok.span.start, tok.span.end, display
        );
    }
}

// ---- HIR / MIR pretty-printers ----

pub(crate) fn print_hir(file: &leek_hir::HirFile) {
    use leek_hir::Def;
    println!("# HIR ({} defs)", file.defs.len());
    for (i, def) in file.defs.iter().enumerate() {
        match def {
            Def::Function(f) => {
                println!("\n#[{i}] function {}", f.name);
                println!("  params: {:?}", param_names(&f.params));
                if let Some(rt) = &f.return_type {
                    println!("  return_type: {rt:?}");
                }
                if let Some(body) = &f.body {
                    println!("  body:");
                    for s in &body.stmts {
                        print_hir_stmt(s, 4);
                    }
                }
            }
            Def::Class(c) => {
                println!("\n#[{i}] class {}", c.name);
                if let Some(parent) = &c.parent {
                    println!("  extends: {parent}");
                }
                println!("  fields: {}", c.fields.len());
                for f in &c.fields {
                    println!(
                        "    {} {}{}",
                        if f.is_static { "static" } else { "      " },
                        f.name,
                        f.init.as_ref().map_or("", |_| " = <expr>"),
                    );
                }
                println!("  methods: {}", c.methods.len());
                for m in &c.methods {
                    println!(
                        "    {}{}({})",
                        if m.is_static { "static " } else { "" },
                        m.name,
                        m.params
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                println!("  constructors: {}", c.constructors.len());
            }
            Def::Global(g) => println!("\n#[{i}] global {}", g.name),
            Def::Local(_) => {}
        }
    }
    if !file.main.is_empty() {
        println!("\n# main");
        for s in &file.main {
            print_hir_stmt(s, 2);
        }
    }
}

pub(crate) fn param_names(params: &[leek_hir::Param]) -> Vec<&str> {
    params.iter().map(|p| p.name.as_str()).collect()
}

pub(crate) fn print_hir_stmt(s: &leek_hir::Stmt, indent: usize) {
    use leek_hir::Stmt;
    let pad = " ".repeat(indent);
    match s {
        Stmt::Expr(e) => println!("{pad}expr {}", format_hir_expr(e)),
        Stmt::VarDecl(v) => println!(
            "{pad}var {} {}= {}",
            v.name,
            v.ty.as_ref().map(|t| format!("{t:?} ")).unwrap_or_default(),
            v.init
                .as_ref()
                .map_or_else(|| "<none>".into(), format_hir_expr),
        ),
        Stmt::Return(Some(e)) => println!("{pad}return {}", format_hir_expr(e)),
        Stmt::Return(None) => println!("{pad}return"),
        Stmt::If(i) => {
            println!("{pad}if {}", format_hir_expr(&i.cond));
            print_hir_stmt(&i.then_branch, indent + 2);
            if let Some(e) = &i.else_branch {
                println!("{pad}else");
                print_hir_stmt(e, indent + 2);
            }
        }
        Stmt::While(w) => {
            println!("{pad}while {}", format_hir_expr(&w.cond));
            print_hir_stmt(&w.body, indent + 2);
        }
        Stmt::DoWhile(dw) => {
            println!("{pad}do");
            print_hir_stmt(&dw.body, indent + 2);
            println!("{pad}while {}", format_hir_expr(&dw.cond));
        }
        Stmt::For(f) => {
            println!("{pad}for");
            if let Some(init) = &f.init {
                print_hir_stmt(init, indent + 2);
            }
            if let Some(c) = &f.cond {
                println!("{pad}  cond {}", format_hir_expr(c));
            }
            if let Some(s) = &f.step {
                println!("{pad}  step {}", format_hir_expr(s));
            }
            print_hir_stmt(&f.body, indent + 2);
        }
        Stmt::Foreach(fe) => {
            println!(
                "{pad}foreach {}{} in {}",
                fe.key
                    .as_ref()
                    .map(|k| format!("{}: ", k.name))
                    .unwrap_or_default(),
                fe.value.name,
                format_hir_expr(&fe.iter),
            );
            print_hir_stmt(&fe.body, indent + 2);
        }
        Stmt::Break(_) => println!("{pad}break"),
        Stmt::Continue(_) => println!("{pad}continue"),
        Stmt::Block(b) => {
            println!("{pad}{{");
            for s in &b.stmts {
                print_hir_stmt(s, indent + 2);
            }
            println!("{pad}}}");
        }
        Stmt::Switch(sw) => {
            println!("{pad}switch {}", format_hir_expr(&sw.discriminant));
            for arm in &sw.arms {
                println!(
                    "{pad}  case {}",
                    arm.case
                        .as_ref()
                        .map_or_else(|| "default".into(), format_hir_expr),
                );
                for s in &arm.body {
                    print_hir_stmt(s, indent + 4);
                }
            }
        }
        Stmt::Include(i) => println!("{pad}include({})", i.path),
        Stmt::Import(i) => println!("{pad}import({})", i.path),
        Stmt::Charge(n) => println!("{pad}charge({n})"),
    }
}

pub(crate) fn format_hir_expr(e: &leek_hir::Expr) -> String {
    use leek_hir::ExprKind;
    match &e.kind {
        ExprKind::Literal(l) => format!("{l:?}"),
        ExprKind::Name(n) => format!("{n:?}"),
        ExprKind::Binary(op, l, r) => {
            format!("({} {op:?} {})", format_hir_expr(l), format_hir_expr(r))
        }
        ExprKind::Unary(op, x) => format!("({op:?} {})", format_hir_expr(x)),
        ExprKind::Postfix(op, x) => format!("({} {op:?})", format_hir_expr(x)),
        ExprKind::Call(c) => {
            let callee = match &c.callee {
                leek_hir::Callee::Function(n) => format!("{n:?}"),
                leek_hir::Callee::Method { receiver, method } => {
                    format!("{}.{}", format_hir_expr(receiver), method)
                }
                leek_hir::Callee::Expr(e) => format_hir_expr(e),
            };
            let args = c
                .args
                .iter()
                .map(format_hir_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{callee}({args})")
        }
        ExprKind::Field(b, name) => format!("{}.{name}", format_hir_expr(b)),
        ExprKind::Index(b, i) => format!("{}[{}]", format_hir_expr(b), format_hir_expr(i)),
        ExprKind::Array(items) => {
            let xs = items
                .iter()
                .map(format_hir_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{xs}]")
        }
        ExprKind::Lambda(_) => "<lambda>".into(),
        other => format!("<{other:?}>"),
    }
}

pub(crate) fn print_mir(p: &leek_mir::MirProgram) {
    use leek_mir::FunctionKind;
    println!(
        "# MIR ({} functions, {} globals, {} classes)",
        p.functions.len(),
        p.globals.len(),
        p.classes.len()
    );
    for g in &p.globals {
        println!("global {} : {:?}", g.name, g.ty);
    }
    for c in &p.classes {
        println!(
            "\nclass {}{} fields={} methods={} constructors={}",
            c.name,
            c.parent
                .as_ref()
                .map(|p| format!(" extends {p}"))
                .unwrap_or_default(),
            c.instance_fields.len() + c.static_fields.len(),
            c.methods.len(),
            c.constructors.len(),
        );
    }
    for (i, f) in p.functions.iter().enumerate() {
        let kind = match f.kind {
            FunctionKind::Main => "main",
            FunctionKind::User => "fn",
        };
        println!("\n#[{i}] {kind} {} -> {:?}", f.name, f.return_ty);
        println!("  params: {:?}", f.params);
        for (li, l) in f.locals.iter().enumerate() {
            let mut flags = String::new();
            if l.is_shared {
                flags.push_str(" shared");
            }
            if l.is_by_ref {
                flags.push_str(" byref");
            }
            println!(
                "  local[{li}] {:?} {}{}{}",
                l.kind,
                l.name.as_deref().unwrap_or("_"),
                l.default_init
                    .map(|b| format!(" default={b}"))
                    .unwrap_or_default(),
                flags,
            );
        }
        for b in &f.blocks {
            println!("  {}:", b.id);
            for s in &b.statements {
                println!("    {s:?}");
            }
            println!("    -> {:?}", b.terminator);
        }
    }
}

pub(crate) fn print_cst(node: &SyntaxNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let range = node.text_range();
    println!(
        "{indent}{:?}@{}..{}",
        node.kind(),
        u32::from(range.start()),
        u32::from(range.end()),
    );
    for child in node.children_with_tokens() {
        match child {
            leek_syntax::SyntaxElement::Node(n) => print_cst(&n, depth + 1),
            leek_syntax::SyntaxElement::Token(t) => {
                let r = t.text_range();
                let text = t.text().replace('\n', "\\n").replace('\t', "\\t");
                println!(
                    "{}  {:?}@{}..{} {:?}",
                    indent,
                    t.kind(),
                    u32::from(r.start()),
                    u32::from(r.end()),
                    text,
                );
            }
        }
    }
}
