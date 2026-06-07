//! `leek-lsp` — Language Server Protocol implementation for
//! Leekscript.
//!
//! Wraps [`tower-lsp`] with a single workspace per server, backed by
//! a salsa [`LeekDb`](leek_pipeline::salsa::LeekDb) so per-keystroke
//! re-runs hit cache. The MVP supports:
//!
//! - `textDocument/publishDiagnostics` — on open / change
//! - `textDocument/hover` — types from `leek-types`'s `TypeTable`
//! - `textDocument/definition` — symbols from `leek-resolver`'s
//!   `ResolveTable`
//! - `textDocument/documentSymbol` — CST-only outline
//!
//! See `doc/lsp.md` for the broader v0.1 plan.

pub mod diagnostics;
pub mod documents;
pub mod handlers;
pub mod pipeline;
pub mod server;
pub mod settings;
pub mod util;
pub mod workspace;

pub use server::LeekLanguageServer;

/// Run the LSP server over stdio. Blocks until the client closes the
/// connection. The binary at `bins/leek-lsp` is a thin wrapper that
/// just calls this.
///
/// All server-side logs go to **stderr**, which `vscode-languageclient`
/// captures and forwards to the editor's "Leekscript" output channel.
/// Set `LEEK_LSP_LOG=trace` to enable per-handler entry logs.
pub fn run_stdio() {
    eprintln!(
        "leek-lsp v{} starting (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
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
                eprintln!("leek-lsp: stdin closed; shutting down");
            }
            () = exit_signal.notified() => {
                // Give the `shutdown` response a moment to flush to the
                // client before we tear the process down.
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                eprintln!("leek-lsp: shutdown/exit received; terminating");
            }
        }
    });
    eprintln!("leek-lsp: server loop exited");
    // Guarantee termination even if a tokio blocking stdin reader or a
    // background task would otherwise keep the process alive — so an
    // editor restart always reclaims this process.
    std::process::exit(0);
}

/// Returns true when `LEEK_LSP_LOG` is set to `trace` (or any
/// truthy value). Used to gate the noisier per-handler entry logs.
pub fn trace_enabled() -> bool {
    matches!(
        std::env::var("LEEK_LSP_LOG").as_deref(),
        Ok("trace" | "debug" | "1" | "true")
    )
}
