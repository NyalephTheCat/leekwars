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
//! Output: `<build>/doc/index.html` plus `<build>/doc/<file>.html`
//! for each source, where `<build>` is `[paths].build` (default
//! `build/`) — the same root `miku clean` removes. A small inline
//! stylesheet keeps the bundle standalone.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_complexity::Complexity;
use leek_ide::doc::{directives_enabled, doc_and_directives_before, doc_comment_before};
use leek_ide::signature::signature_for;
use leek_parser::pipeline::GreenTreeArtifact;
use leek_session::{DriverConfig, Session, Target};
use leek_span::SourceId;
use leek_syntax::{SyntaxKind, SyntaxNode};

use crate::cli::Doc;
use leek_project::Project;

pub fn run(args: &Doc, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    if leek_session::report_manifest(
        &project,
        leek_diagnostics::ColorWhen::Auto,
        leek_diagnostics::MessageFormat::Human,
    ) {
        return Ok(ExitCode::from(1));
    }

    let out_root = args.out_dir.clone().unwrap_or_else(|| project.doc_dir());
    std::fs::create_dir_all(&out_root)
        .with_context(|| format!("creating {}", out_root.display()))?;

    let mut sources = project.walk_sources();
    sources.extend(project.walk_tests());
    if sources.is_empty() {
        sources.push(project.entry_path());
    }

    // The same driver entry point `check` uses, so a file's `include(...)`
    // calls resolve and the complexity rows reflect the real callees.
    let config = DriverConfig {
        target: Target::Complexity,
        ..DriverConfig::default()
    };

    // One session for every source: the reporter and the include-id space
    // are built once rather than per file.
    let session = Session::new(&project, config)?;

    // Build the per-source page set.
    let mut pages: Vec<Page> = Vec::new();
    for (i, path) in sources.iter().enumerate() {
        let source_id = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let compiled = session.compile_file(path, source_id)?;
        // See `analyze`: the id the spans carry is the session's.
        let source_id = compiled.input().source;
        let Some(report) = compiled.complexity() else {
            if !quiet {
                eprintln!(
                    "miku doc: skipping {} (no complexity report)",
                    project.relative(path).display()
                );
            }
            continue;
        };
        let Some(parse) = compiled.get::<GreenTreeArtifact>() else {
            continue;
        };
        let root = SyntaxNode::new_root(parse.0.clone());

        let items = collect_items(&root, source_id, compiled.text(), report);
        let relative = project.relative(path);
        pages.push(Page {
            html_name: file_html_name(&relative),
            rel_source: relative,
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
    let index_html = render_index(&pages, &project.manifest.project);
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
        && let Err(e) = open_in_browser(&index_path)
    {
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

fn collect_items(
    root: &SyntaxNode,
    source_id: SourceId,
    source: &str,
    complexities: &[Complexity],
) -> Vec<Item> {
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
        let doc = if directives_enabled(
            source,
            leek_span::FeatureFlags::from_env().function_signatures,
        ) {
            doc_and_directives_before(source, start)
                .map(|(visible, _)| visible)
                .filter(|v| !v.trim().is_empty())
        } else {
            doc_comment_before(source, start)
        };
        // The report also covers functions spliced in from included files;
        // match on the declaring file as well as the name so a collision
        // can't attach an included function's formula to this one.
        let complexity = if kind == ItemKind::Function {
            complexities
                .iter()
                .find(|c| c.name == name && c.span.is_some_and(|s| s.source == source_id))
                .cloned()
        } else {
            None
        };
        let line = u32::try_from(source[..start as usize].matches('\n').count()).unwrap() + 1;
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
.tagline { color: #444; font-size: 1.05em; margin: 0.2em 0 1em 0; }
.meta { color: #888; font-size: 0.85em; border-top: 1px solid #ddd; margin-top: 2.5em; padding-top: 0.8em; }
.meta a { color: #4a6db5; }
"#;

/// The index page. Takes the whole `[project]` table rather than just its
/// name: `description`, `authors`, `license` and `repository` are parsed and
/// this is where they land, which is what makes them something other than
/// decoration in `Miku.toml`.
fn render_index(pages: &[Page], project: &leek_manifest::ProjectTable) -> String {
    let project_name = project.name.as_str();
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    let _ = write!(
        out,
        "<title>{} – miku doc</title>",
        html_escape(project_name)
    );
    out.push_str("<style>");
    out.push_str(CSS);
    out.push_str("</style></head><body>");
    let _ = write!(out, "<h1>{}</h1>", html_escape(project_name));
    if let Some(description) = &project.description {
        let _ = write!(out, "<p class=\"tagline\">{}</p>", html_escape(description));
    }
    out.push_str("<p class=\"crumbs\">miku doc – API reference</p>");

    out.push_str("<h2>Files</h2>");
    out.push_str("<ul class=\"file-list\">");
    for page in pages {
        let total = page.items.len();
        let _ = write!(
            out,
            "<li><a href=\"{href}\">{name}</a><span class=\"count\">{total} item{plural}</span></li>",
            href = html_escape(&page.html_name),
            name = html_escape(&page.rel_source.display().to_string()),
            total = total,
            plural = if total == 1 { "" } else { "s" },
        );
    }
    out.push_str("</ul>");

    // Version, authors, license, repository — a footer rather than a header,
    // since the file list is what a reader came for.
    let mut meta: Vec<String> = vec![format!("v{}", html_escape(&project.version))];
    if !project.authors.is_empty() {
        let names: Vec<String> = project.authors.iter().map(|a| html_escape(a)).collect();
        meta.push(names.join(", "));
    }
    if let Some(license) = &project.license {
        meta.push(html_escape(license));
    }
    if let Some(repository) = &project.repository {
        let href = html_escape(repository);
        meta.push(format!("<a href=\"{href}\">{href}</a>"));
    }
    let _ = write!(out, "<p class=\"meta\">{}</p>", meta.join(" · "));

    out.push_str("</body></html>");
    out
}

fn render_page(page: &Page, project_name: &str) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    let _ = write!(
        out,
        "<title>{} – {}</title>",
        html_escape(&page.rel_source.display().to_string()),
        html_escape(project_name),
    );
    out.push_str("<style>");
    out.push_str(CSS);
    out.push_str("</style></head><body>");
    let _ = write!(
        out,
        "<p class=\"crumbs\"><a href=\"index.html\">{}</a> &raquo; {}</p>",
        html_escape(project_name),
        html_escape(&page.rel_source.display().to_string()),
    );
    let _ = write!(
        out,
        "<h1>{}</h1>",
        html_escape(&page.rel_source.display().to_string()),
    );

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
    let _ = write!(
        out,
        "<h3><span class=\"kind\">{}</span>{}</h3>",
        match item.kind {
            ItemKind::Function => "function",
            ItemKind::Class => "class",
            ItemKind::Global => "global",
        },
        html_escape(&item.name),
    );
    let _ = write!(
        out,
        "<code class=\"sig\">{}</code>",
        html_escape(&item.signature),
    );
    if let Some(doc) = &item.doc {
        let _ = write!(out, "<div class=\"doc\">{}</div>", html_escape(doc));
    }
    if let Some(c) = &item.complexity {
        let _ = write!(
            out,
            "<div class=\"complexity\"><strong>Complexity:</strong> {} &nbsp; <code>{}</code></div>",
            html_escape(&c.big_o.render()),
            html_escape(&c.formula.render()),
        );
    }
    let _ = write!(out, "<div class=\"line\">line {}</div>", item.line);
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
