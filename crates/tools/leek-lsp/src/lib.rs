//! `leek-lsp` — Language Server Protocol implementation for
//! Leekscript.
//!
//! Wraps [`tower-lsp`] with a single workspace per server, backed by
//! a salsa [`LeekDb`](leek_query::salsa::LeekDb) so per-keystroke
//! re-runs hit cache.
//!
//! # What the server answers
//!
//! The authoritative list is the `ServerCapabilities` block in
//! [`server`]; this is the map of which module answers what.
//!
//! **Diagnostics** — [`diagnostics`] builds one set per file and both
//! transports serve it: push (`publishDiagnostics`, on open/change) and
//! pull ([`handlers::pull_diagnostics`]).
//!
//! **Navigation** — [`handlers::definition`] (definition + declaration),
//! [`handlers::type_definition`], [`handlers::implementation`],
//! [`handlers::references`], [`handlers::document_highlight`],
//! [`handlers::call_hierarchy`], [`handlers::type_hierarchy`].
//! Cross-file answers are scoped by [`handlers::program_scope`].
//!
//! **Information** — [`handlers::hover`], [`handlers::signature_help`],
//! [`handlers::inlay_hints`], [`handlers::inline_values`],
//! [`handlers::code_lens`] (references and complexity, resolved lazily
//! and clickable via `workspace/executeCommand`),
//! [`handlers::document_link`], [`handlers::document_color`].
//!
//! **Structure** — [`handlers::symbols`] (document outline),
//! [`handlers::workspace_symbols`], [`handlers::folding`],
//! [`handlers::selection_range`], [`handlers::semantic_tokens`].
//!
//! **Editing** — [`handlers::completion`] (identifiers, builtins, and
//! member completion after `.`), [`handlers::code_action`] (quick fixes
//! and `source.fixAll`), [`handlers::rename`] +
//! [`handlers::prepare_rename`], [`handlers::linked_editing`],
//! [`handlers::formatting`] / [`handlers::range_formatting`] /
//! [`handlers::on_type_formatting`], [`handlers::file_operations`].
//!
//! A handler whose only alternative would be a wrong edit answers with a
//! [`Refusal`](handlers::refusal::Refusal) the editor shows the user,
//! rather than a silent no-op.
//!
//! # Where the rest is written down
//!
//! [`analysis`] is the typed way to ask a file's green tree, syntax root,
//! resolve table, type table, HIR or complexity report — one memoized
//! `leek-db` query each, and the route handlers move onto as they come off
//! [`pipeline`]. [`log`] owns the single logging seam and the `LEEK_LSP_LOG` contract,
//! [`settings`] the editor-pushed configuration, [`workspace`] the salsa
//! DB and document registry, and [`handlers::program_scope`] the
//! per-program symbol scoping Leekscript's flat namespace forces.
//! `docs/lsp.md` in the repository root ties those together.

// A language server has no terminal. Everything it says must go through
// [`log`] so it reaches both stderr and — for warnings and errors — the
// editor's own output channel; a bare `println!`/`eprintln!` is a report the
// user's bug report can never contain.
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod analysis;
pub mod diagnostics;
pub mod documents;
pub mod folder;
pub mod handlers;
pub mod log;
pub mod server;
pub mod settings;
pub mod util;
pub mod workspace;

pub use server::LeekLanguageServer;

/// How long to wait for tokio's blocking stdin reader before detaching it.
/// Dropping a multi-thread runtime *joins* its blocking pool, and the stdin
/// reader only wakes on the next byte — which, on an editor "restart
/// server", never comes. Detaching keeps `run_stdio` a function that
/// returns.
const RUNTIME_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(100);

/// Run the LSP server over stdio. Blocks until the client closes the
/// connection or asks the server to exit, then **returns** — the binaries
/// at `bins/leek-lsp` and `miku lsp` are thin wrappers that decide the
/// process's fate themselves.
///
/// Logging is a caller's decision too: call [`log::init`] first (both
/// binaries do) to get the stderr subscriber and the `window/logMessage`
/// mirror. Without it the server runs silently, which is what an embedder
/// with its own subscriber wants.
///
/// # Errors
///
/// Returns the [`std::io::Error`] from building the tokio runtime; there is
/// no server to report it through, so the caller has to.
pub fn run_stdio() -> std::io::Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "leek-lsp starting"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Shared exit signal: the server raises it from `shutdown`, and the
    // driver below races it against the serve loop. tower-lsp 0.20 only
    // ends `serve` on stdin EOF, so on an editor "restart server" (which
    // sends `shutdown` + `exit` but may keep stdin open) we'd otherwise
    // linger. Driving the exit here makes a restart reliably reclaim us.
    let exit_signal = std::sync::Arc::new(tokio::sync::Notify::new());
    let exit_for_factory = exit_signal.clone();
    runtime.block_on(async move {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) = tower_lsp::LspService::new(move |client| {
            LeekLanguageServer::new_with_exit(client, exit_for_factory.clone())
        });
        let server = tower_lsp::Server::new(stdin, stdout, socket);
        tokio::select! {
            () = server.serve(service) => {
                tracing::info!("stdin closed; shutting down");
            }
            () = exit_signal.notified() => {
                // Give the `shutdown` response a moment to flush to the
                // client before we tear the server down.
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                tracing::info!("shutdown/exit received; terminating");
            }
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_GRACE);
    tracing::info!("server loop exited");
    Ok(())
}
