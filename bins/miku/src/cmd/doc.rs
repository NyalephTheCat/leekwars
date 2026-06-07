//! `miku doc` — generate HTML API documentation.
//!
//! Walks every `.leek` source under the project root, parses each
//! one, then emits a single-file-per-source HTML page plus an
//! index. Each declaration gets:
//!
//! - A signature line (`function f(integer a, integer b) -> string`,
//!   `class Cat extends Animal`, etc.) reused from the LSP's
//!   `signature_for`.
//! - The leading `//` / `/** … */` doc comment band (same helper
//!   as the LSP hover).
//! - A complexity row computed by `leek-complexity` (for user
//!   functions) — the same `O(...)` and ops formula that
//!   `miku analyze` prints.
//! - Source location.
//!
//! Output: `target/doc/index.html` plus `target/doc/<file>.html`
//! for each source. A small inline stylesheet keeps the bundle
//! standalone.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_complexity::{Complexity, analyze_file};
use leek_hir::pipeline::HirArtifact;
use leek_ide::doc::{directives_enabled, doc_and_directives_before, doc_comment_before};
use leek_ide::signature::signature_for;
use leek_parser::pipeline::GreenTreeArtifact;
use leek_pipeline::Input;
use leek_span::SourceId;
use leek_syntax::{SyntaxKind, SyntaxNode};

use crate::cli::Doc;
use leek_project::Project;

pub fn run(args: Doc, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let out_root = args
        .out_dir
        .clone()
        .unwrap_or_else(|| project.root.join("target").join("doc"));
    std::fs::create_dir_all(&out_root)
        .with_context(|| format!("creating {}", out_root.display()))?;

    let mut sources = project.walk_sources();
    sources.extend(project.walk_tests());
    if sources.is_empty() {
        sources.push(project.entry_path());
    }

    // Build the per-source page set.
    let mut pages: Vec<Page> = Vec::new();
    for (i, path) in sources.iter().enumerate() {
        let source_id = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let (src, text) = project.pipeline_input(source_id, path)?;
        let input = Input::from(src);
        let pipeline =
            leek_recipes::pipeline(leek_recipes::Target::Hir, &leek_recipes::driver_params())
                .expect("recipe");
        let result = pipeline.run(input);
        let Some(hir_artifact) = result.get::<HirArtifact>() else {
            if !quiet {
                eprintln!(
                    "miku doc: skipping {} (no HIR)",
                    rel(&project.root, path).display()
                );
            }
            continue;
        };
        let Some(parse) = result.get::<GreenTreeArtifact>() else {
            continue;
        };
        let root = SyntaxNode::new_root(parse.0.clone());

        let complexities = analyze_file(&hir_artifact.0);

        let items = collect_items(&root, &text, &complexities);
        let out_name = file_html_name(&rel(&project.root, path));
        pages.push(Page {
            rel_source: rel(&project.root, path),
            html_name: out_name,
            items,
        });
    }

    // Write per-file pages.
    for page in &pages {
        let html = render_page(page, &project.manifest.project.name);
        let path = out_root.join(&page.html_name);
        std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    }

    // Write the index.
    let index_html = render_index(&pages, &project.manifest.project.name);
    let index_path = out_root.join("index.html");
    std::fs::write(&index_path, index_html)
        .with_context(|| format!("writing {}", index_path.display()))?;

    if !quiet {
        eprintln!(
            "miku doc: wrote {} page{} to {}",
            pages.len() + 1,
            if pages.is_empty() { "" } else { "s" },
            out_root.display(),
        );
    }

    if args.open
        && let Err(e) = open_in_browser(&index_path) {
            eprintln!("miku doc: failed to open browser: {e}");
        }

    Ok(ExitCode::SUCCESS)
}

// ─── data ──────────────────────────────────────────────────────────

struct Page {
    rel_source: PathBuf,
    html_name: String,
    items: Vec<Item>,
}

struct Item {
    name: String,
    kind: ItemKind,
    /// One-line signature (rendered via `signature_for`).
    signature: String,
    /// Leading `//` or `/** … */` doc band, already trimmed of
    /// leading marker chars.
    doc: Option<String>,
    /// `O(...)` + ops formula for user functions; `None` for
    /// classes, fields, globals.
    complexity: Option<Complexity>,
    /// Line number in the source (1-based).
    line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Function,
    Class,
    Global,
}

fn collect_items(root: &SyntaxNode, source: &str, complexities: &[Complexity]) -> Vec<Item> {
    let mut out = Vec::new();
    // Walk only direct children of the source file so we pick up
    // top-level declarations and skip nested classes/methods.
    for node in root.children() {
        let kind = match node.kind() {
            SyntaxKind::FnDecl => ItemKind::Function,
            SyntaxKind::ClassDecl => ItemKind::Class,
            SyntaxKind::VarDeclStmt => {
                // Only document `global` decls — locals at file
                // top-level are typically initialization scratch.
                if !is_global_decl(&node) {
                    continue;
                }
                ItemKind::Global
            }
            _ => continue,
        };
        let name = decl_name(&node).unwrap_or_else(|| "<anonymous>".into());
        let signature = signature_for(&node).unwrap_or_else(|| name.clone());
        let start = u32::from(node.text_range().start());
        // In a signature file, strip `@<backend>-backend:` directives
        // from the prose; in normal code they're inert and kept as-is.
        let doc = if directives_enabled(source, leek_span::FeatureFlags::from_env().function_signatures) {
            doc_and_directives_before(source, start)
                .map(|(visible, _)| visible)
                .filter(|v| !v.trim().is_empty())
        } else {
            doc_comment_before(source, start)
        };
        let complexity = if kind == ItemKind::Function {
            complexities.iter().find(|c| c.name == name).cloned()
        } else {
            None
        };
        let line = source[..start as usize].matches('\n').count() as u32 + 1;
        out.push(Item {
            name,
            kind,
            signature,
            doc,
            complexity,
            line,
        });
    }
    out
}

fn is_global_decl(node: &SyntaxNode) -> bool {
    node.children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .any(|t| t.kind() == SyntaxKind::KwGlobal)
}

fn decl_name(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

// ─── rendering ─────────────────────────────────────────────────────

const CSS: &str = r#"
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; max-width: 880px; margin: 2em auto; padding: 0 1em; color: #222; line-height: 1.5; }
h1, h2 { border-bottom: 1px solid #ddd; padding-bottom: 0.2em; }
.crumbs { color: #888; font-size: 0.9em; margin-bottom: 1em; }
.crumbs a { color: #4a6db5; }
.item { margin: 1.5em 0 2em 0; padding-left: 1em; border-left: 3px solid #e1e4e8; }
.item h3 { margin-bottom: 0.2em; font-size: 1.1em; }
.kind { font-size: 0.75em; text-transform: uppercase; letter-spacing: 0.05em; color: #888; margin-right: 0.5em; }
.sig { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; background: #f6f8fa; padding: 0.4em 0.6em; border-radius: 4px; display: block; overflow-x: auto; font-size: 0.95em; }
.doc { color: #444; margin-top: 0.5em; white-space: pre-wrap; }
.complexity { color: #555; font-size: 0.9em; margin-top: 0.4em; }
.complexity code { background: #f6f8fa; padding: 0.05em 0.3em; border-radius: 3px; font-size: 0.9em; }
.line { color: #aaa; font-size: 0.85em; }
.empty { color: #999; font-style: italic; }
.file-list { list-style: none; padding-left: 0; }
.file-list li { margin: 0.3em 0; }
.file-list a { color: #4a6db5; text-decoration: none; }
.file-list a:hover { text-decoration: underline; }
.file-list .count { color: #888; margin-left: 0.5em; font-size: 0.85em; }
"#;

fn render_index(pages: &[Page], project_name: &str) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str(&format!(
        "<title>{} – miku doc</title>",
        html_escape(project_name)
    ));
    out.push_str("<style>");
    out.push_str(CSS);
    out.push_str("</style></head><body>");
    out.push_str(&format!("<h1>{}</h1>", html_escape(project_name)));
    out.push_str("<p class=\"crumbs\">miku doc – API reference</p>");

    out.push_str("<h2>Files</h2>");
    out.push_str("<ul class=\"file-list\">");
    for page in pages {
        let total = page.items.len();
        out.push_str(&format!(
            "<li><a href=\"{href}\">{name}</a><span class=\"count\">{total} item{plural}</span></li>",
            href = html_escape(&page.html_name),
            name = html_escape(&page.rel_source.display().to_string()),
            total = total,
            plural = if total == 1 { "" } else { "s" },
        ));
    }
    out.push_str("</ul>");
    out.push_str("</body></html>");
    out
}

fn render_page(page: &Page, project_name: &str) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    out.push_str(&format!(
        "<title>{} – {}</title>",
        html_escape(&page.rel_source.display().to_string()),
        html_escape(project_name),
    ));
    out.push_str("<style>");
    out.push_str(CSS);
    out.push_str("</style></head><body>");
    out.push_str(&format!(
        "<p class=\"crumbs\"><a href=\"index.html\">{}</a> &raquo; {}</p>",
        html_escape(project_name),
        html_escape(&page.rel_source.display().to_string()),
    ));
    out.push_str(&format!(
        "<h1>{}</h1>",
        html_escape(&page.rel_source.display().to_string()),
    ));

    if page.items.is_empty() {
        out.push_str("<p class=\"empty\">No top-level declarations.</p>");
    } else {
        for item in &page.items {
            out.push_str(&render_item(item));
        }
    }
    out.push_str("</body></html>");
    out
}

fn render_item(item: &Item) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"item\">");
    out.push_str(&format!(
        "<h3><span class=\"kind\">{}</span>{}</h3>",
        match item.kind {
            ItemKind::Function => "function",
            ItemKind::Class => "class",
            ItemKind::Global => "global",
        },
        html_escape(&item.name),
    ));
    out.push_str(&format!(
        "<code class=\"sig\">{}</code>",
        html_escape(&item.signature),
    ));
    if let Some(doc) = &item.doc {
        out.push_str(&format!("<div class=\"doc\">{}</div>", html_escape(doc),));
    }
    if let Some(c) = &item.complexity {
        out.push_str(&format!(
            "<div class=\"complexity\"><strong>Complexity:</strong> {} &nbsp; <code>{}</code></div>",
            html_escape(&c.big_o.render()),
            html_escape(&c.formula.render()),
        ));
    }
    out.push_str(&format!("<div class=\"line\">line {}</div>", item.line,));
    out.push_str("</div>");
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn file_html_name(rel_path: &Path) -> String {
    // Replace path separators + `.leek` extension with `--` and
    // `.html` so the flat output directory has unique filenames.
    let s = rel_path.display().to_string();
    let mut sanitised = s.replace(['/', '\\'], "--");
    if let Some(stripped) = sanitised.strip_suffix(".leek") {
        sanitised = stripped.to_string();
    }
    format!("{sanitised}.html")
}

fn rel(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root).map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}

fn open_in_browser(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "start";
    let status = std::process::Command::new(cmd).arg(path).status()?;
    if !status.success() {
        anyhow::bail!("{cmd} exited with status {status}");
    }
    Ok(())
}
