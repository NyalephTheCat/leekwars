//! tower-lsp [`LanguageServer`] implementation.

use std::sync::Arc;

use tokio::sync::{Mutex, Notify};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types as lsp;
use tower_lsp::{Client, LanguageServer};

use crate::handlers::refusal::Refusal;
use crate::handlers::{
    call_hierarchy, code_action, code_lens, completion, definition, document_color,
    document_highlight, document_link, execute_command, file_operations, folding, formatting,
    hover, implementation, inlay_hints, inline_values, linked_editing, on_type_formatting,
    prepare_rename, pull_diagnostics, range_formatting, references, rename, selection_range,
    semantic_tokens, signature_help, symbols, type_definition, type_hierarchy, workspace_symbols,
};
use crate::util::guard::{guard, guard_with};
use crate::workspace::Workspace;

/// JSON-RPC error code for LSP `RequestFailed`: the request was valid
/// but the server cannot carry it out. Editors surface the message to
/// the user (a notification in VS Code, the echo area in Emacs/Neovim),
/// which is exactly what a refusal needs.
const REQUEST_FAILED: i64 = -32803;

/// Turn a handler's [`Refusal`] into the JSON-RPC error the client
/// shows the user.
fn refusal_to_rpc_error(refusal: Refusal) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(REQUEST_FAILED),
        message: std::borrow::Cow::Owned(refusal.message),
        data: None,
    }
}

pub struct LeekLanguageServer {
    pub client: Client,
    pub state: Arc<Mutex<Workspace>>,
    /// Notified by `shutdown` so the stdio driver can terminate the
    /// process. tower-lsp 0.20's serve loop only ends on stdin EOF — not
    /// on the `exit` notification — so without this an editor "restart
    /// server" can leave this process lingering (running the old binary).
    pub exit_signal: Arc<Notify>,
}

impl LeekLanguageServer {
    pub fn new(client: Client) -> Self {
        Self::new_with_exit(client, Arc::new(Notify::new()))
    }

    /// Construct with an externally-owned exit signal so the stdio driver
    /// can await the same notification it raises on `shutdown`.
    pub fn new_with_exit(client: Client, exit_signal: Arc<Notify>) -> Self {
        spawn_client_log_mirror(&client);
        Self {
            client,
            state: Arc::new(Mutex::new(Workspace::default())),
            exit_signal,
        }
    }
}

/// Start the task that forwards `WARN`-and-above [`tracing`] events to the
/// editor as `window/logMessage`.
///
/// The handlers this reports on are synchronous — `formatting::handle` can't
/// await `client.log_message` — so [`crate::log`] hands records to an
/// unbounded channel and this one task does the awaiting. Without a tokio
/// runtime (a unit test constructing a server directly) there is nothing to
/// spawn onto and the records simply stay on stderr.
fn spawn_client_log_mirror(client: &Client) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut rx = crate::log::attach_client_sink();
    let client = client.clone();
    handle.spawn(async move {
        // `showMessage` is a modal-ish popup in most editors. Raise it for
        // the first failure that asks for one and log the rest: the point is
        // that the user learns to look at the output channel, not that we
        // nag on every keystroke.
        let mut notified = false;
        while let Some(msg) = rx.recv().await {
            client.log_message(msg.kind, &msg.text).await;
            if msg.notify && !notified {
                notified = true;
                client.show_message(msg.kind, &msg.text).await;
            }
        }
    });
}

#[tower_lsp::async_trait]
impl LanguageServer for LeekLanguageServer {
    async fn initialize(&self, params: lsp::InitializeParams) -> Result<lsp::InitializeResult> {
        tracing::info!(
            client = params.client_info.as_ref().map_or_else(
                || "?".to_string(),
                |c| format!("{} {}", c.name, c.version.as_deref().unwrap_or("?"))
            ),
            pid = params.process_id,
            "initialize"
        );
        {
            let mut ws = self.state.lock().await;
            if let Some(folders) = &params.workspace_folders {
                for folder in folders {
                    if let Ok(path) = folder.uri.to_file_path() {
                        ws.queue_project_root(path);
                    }
                }
            } else if let Some(root_uri) = &params.root_uri
                && let Ok(path) = root_uri.to_file_path()
            {
                ws.queue_project_root(path);
            }
        }
        // Host-environment libraries. The LSP's primary use case is
        // leek-wars AIs, so we always load the built-in `leekwars` catalog
        // by default — even when the client doesn't pass
        // `initializationOptions.libraries` (some setups don't wire that
        // through). Any explicitly-configured libraries are merged on top
        // (deduped). Registering their functions and constants lets
        // diagnostics, completion, and hover recognize them workspace-wide
        // — the same mechanism `leekc`/`miku --library` use. We log a
        // per-library breakdown (functions + constants counts + a sample of
        // names) so users can confirm the library's *constants* actually
        // loaded. The lines are stashed and flushed via `window/logMessage`
        // in `initialized` (the VS Code "Leekscript" output channel) since
        // messages sent during `initialize` can be dropped before the
        // client finishes initializing.
        {
            // Default to leekwars; merge in any configured libraries.
            let mut specs: Vec<String> = vec!["leekwars".to_string()];
            if let Some(opts) = &params.initialization_options
                && let Some(libs) = opts.get("libraries").and_then(|v| v.as_array())
            {
                for s in libs.iter().filter_map(|v| v.as_str()) {
                    if !specs.iter().any(|x| x == s) {
                        specs.push(s.to_string());
                    }
                }
            }
            {
                let mut log: Vec<(bool, String)> = Vec::new();
                log.push((
                    false,
                    format!(
                        "leek-lsp: loading {} librar{} {specs:?}",
                        specs.len(),
                        if specs.len() == 1 { "y" } else { "ies" }
                    ),
                ));
                let mut total_fns = 0usize;
                let mut total_consts = 0usize;
                let mut any_err = false;
                for result in leek_session::load_register_and_report(&specs) {
                    match result {
                        Ok(s) => {
                            total_fns += s.functions;
                            total_consts += s.constants;
                            let imports = if s.imports.is_empty() {
                                String::new()
                            } else {
                                format!(", imports {}", s.imports.join(" "))
                            };
                            let fn_sample = if s.sample_functions.is_empty() {
                                String::new()
                            } else {
                                format!("; fns e.g. {}", s.sample_functions.join(", "))
                            };
                            let const_sample = if s.sample_constants.is_empty() {
                                String::new()
                            } else {
                                format!("; consts e.g. {}", s.sample_constants.join(", "))
                            };
                            log.push((
                                false,
                                format!(
                                    "leek-lsp:   ✓ {} — {} functions, {} constants{imports}{fn_sample}{const_sample}",
                                    s.spec, s.functions, s.constants
                                ),
                            ));
                            if s.constants == 0 {
                                log.push((
                                    true,
                                    format!(
                                        "leek-lsp:   ⚠ {} registered 0 constants — check the library defines `const NAME type` lines",
                                        s.spec
                                    ),
                                ));
                            }
                        }
                        Err(e) => {
                            any_err = true;
                            log.push((true, format!("leek-lsp:   ✗ failed to load {e}")));
                        }
                    }
                }
                log.push((
                    any_err,
                    format!(
                        "leek-lsp: libraries ready — {total_fns} functions, {total_consts} constants this load; resolver now knows {} functions, {} constants total",
                        leek_resolver::builtins::dynamic_builtin_functions().len(),
                        leek_resolver::builtins::dynamic_builtin_constants().len(),
                    ),
                ));
                // Mirror to the log now, for terminal / log-file launches.
                // The client half stays buffered until `initialized`: a
                // `window/logMessage` sent before then can be dropped.
                for (is_err, line) in &log {
                    if *is_err {
                        tracing::warn!("{line}");
                    } else {
                        tracing::info!("{line}");
                    }
                }
                self.state.lock().await.pending_library_log = log;
            }
        }
        Ok(lsp::InitializeResult {
            capabilities: lsp::ServerCapabilities {
                text_document_sync: Some(lsp::TextDocumentSyncCapability::Kind(
                    lsp::TextDocumentSyncKind::INCREMENTAL,
                )),
                hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
                definition_provider: Some(lsp::OneOf::Left(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                document_highlight_provider: Some(lsp::OneOf::Left(true)),
                document_symbol_provider: Some(lsp::OneOf::Left(true)),
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                rename_provider: Some(lsp::OneOf::Right(lsp::RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                })),
                folding_range_provider: Some(lsp::FoldingRangeProviderCapability::Simple(true)),
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                document_range_formatting_provider: Some(lsp::OneOf::Left(true)),
                code_action_provider: Some(lsp::CodeActionProviderCapability::Options(
                    lsp::CodeActionOptions {
                        // Advertise both the per-finding quick fixes and the
                        // whole-file `source.fixAll` so editors offer them in
                        // their "fix all" / on-save cleanup menus.
                        code_action_kinds: Some(vec![
                            lsp::CodeActionKind::QUICKFIX,
                            lsp::CodeActionKind::SOURCE_FIX_ALL,
                        ]),
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                        resolve_provider: None,
                    },
                )),
                // `Right(…Options)` so we can advertise `resolve_provider`:
                // the inlay's hover tooltip is computed lazily in
                // `inlay_hint_resolve` rather than on every hint.
                inlay_hint_provider: Some(lsp::OneOf::Right(
                    lsp::InlayHintServerCapabilities::Options(lsp::InlayHintOptions {
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                        resolve_provider: Some(true),
                    }),
                )),
                linked_editing_range_provider: Some(
                    lsp::LinkedEditingRangeServerCapabilities::Simple(true),
                ),
                // Live variable values during a debug session (rendered
                // by the editor from the debug adapter's data).
                inline_value_provider: Some(lsp::OneOf::Left(true)),
                type_definition_provider: Some(lsp::TypeDefinitionProviderCapability::Simple(true)),
                selection_range_provider: Some(lsp::SelectionRangeProviderCapability::Simple(true)),
                document_link_provider: Some(lsp::DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                }),
                call_hierarchy_provider: Some(lsp::CallHierarchyServerCapability::Simple(true)),
                // Note: lsp-types 0.94 doesn't expose
                // `type_hierarchy_provider` on `ServerCapabilities`.
                // The trait methods (`prepare_type_hierarchy`,
                // `supertypes`, `subtypes`) still work when a
                // client invokes them — we just can't advertise
                // the capability via the standard field. A newer
                // lsp-types version closes this gap.
                code_lens_provider: Some(lsp::CodeLensOptions {
                    // The "N references" lens arrives without a command
                    // and gets its count (and the `Location[]` the peek
                    // view needs) in `code_lens_resolve` — a
                    // program-wide occurrence search per function is far
                    // too much for a request the editor fires on scroll.
                    resolve_provider: Some(true),
                }),
                color_provider: Some(lsp::ColorProviderCapability::Simple(true)),
                declaration_provider: Some(lsp::DeclarationCapability::Simple(true)),
                implementation_provider: Some(lsp::ImplementationProviderCapability::Simple(true)),
                document_on_type_formatting_provider: Some(lsp::DocumentOnTypeFormattingOptions {
                    first_trigger_character: ";".into(),
                    more_trigger_character: Some(vec!["}".into(), "\n".into()]),
                }),
                execute_command_provider: Some(lsp::ExecuteCommandOptions {
                    commands: execute_command::COMMANDS
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                }),
                diagnostic_provider: Some(lsp::DiagnosticServerCapabilities::Options(
                    lsp::DiagnosticOptions {
                        identifier: Some("leek".into()),
                        inter_file_dependencies: true,
                        workspace_diagnostics: true,
                        work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                    },
                )),
                completion_provider: Some(lsp::CompletionOptions {
                    trigger_characters: Some(vec![".".into()]),
                    // We attach documentation lazily in
                    // `completion_resolve` rather than on every item.
                    resolve_provider: Some(true),
                    ..Default::default()
                }),
                signature_help_provider: Some(lsp::SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: Some(vec![",".into()]),
                    work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
                }),
                semantic_tokens_provider: Some(
                    lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(
                        lsp::SemanticTokensOptions {
                            legend: semantic_tokens::legend(),
                            // `Delta { delta: true }` advertises both
                            // `…/full` and `…/full/delta`; `range: true`
                            // adds `…/semanticTokens/range`.
                            full: Some(lsp::SemanticTokensFullOptions::Delta { delta: Some(true) }),
                            range: Some(true),
                            ..Default::default()
                        },
                    ),
                ),
                // Ask the client to send `willRenameFiles` for `.leek`
                // files so we can rewrite `include(...)` references as
                // part of the rename, and `didRenameFiles` afterwards so
                // the workspace can carry the moved file's state over.
                // Without the second registration the client never sends
                // it and `did_rename_files` is unreachable — a rename
                // reaches us only as the watcher's delete + create.
                workspace: Some(lsp::WorkspaceServerCapabilities {
                    workspace_folders: None,
                    file_operations: Some(lsp::WorkspaceFileOperationsServerCapabilities {
                        will_rename: Some(leek_file_filter()),
                        did_rename: Some(leek_file_filter()),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            },
            server_info: Some(lsp::ServerInfo {
                name: "leek-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: lsp::InitializedParams) {
        tracing::info!("initialized, ready");
        let library_log = {
            let mut ws = self.state.lock().await;
            ws.index_pending_projects();
            std::mem::take(&mut ws.pending_library_log)
        };
        // Flush the per-library load report to the client's log channel
        // (VS Code "Leekscript" output) now that initialization is complete.
        for (is_err, line) in library_log {
            let ty = if is_err {
                lsp::MessageType::WARNING
            } else {
                lsp::MessageType::INFO
            };
            self.client.log_message(ty, line).await;
        }
        self.client
            .log_message(lsp::MessageType::INFO, "leek-lsp ready")
            .await;

        // Ask the client to watch `.leek` files on disk and forward
        // changes via `workspace/didChangeWatchedFiles`, so edits made
        // outside the editor (git checkout, an external tool) refresh
        // the project index. Best-effort: clients without dynamic
        // registration simply won't send the events.
        let registration = lsp::Registration {
            id: "leek-watch-files".into(),
            method: "workspace/didChangeWatchedFiles".into(),
            register_options: serde_json::to_value(lsp::DidChangeWatchedFilesRegistrationOptions {
                watchers: vec![lsp::FileSystemWatcher {
                    glob_pattern: lsp::GlobPattern::String("**/*.leek".into()),
                    kind: None,
                }],
            })
            .ok(),
        };
        if let Err(e) = self.client.register_capability(vec![registration]).await {
            // Not fatal — the client simply won't report on-disk edits — but
            // it explains a stale index, so the user should be able to see it.
            tracing::warn!(error = %e, "file-watcher registration declined");
        }

        // Pull the initial `leek` settings so formatter / inlay-hint
        // options apply before the first `didChangeConfiguration` (some
        // clients only answer the pull). Best-effort.
        if let Ok(values) = self
            .client
            .configuration(vec![lsp::ConfigurationItem {
                scope_uri: None,
                section: Some("leek".into()),
            }])
            .await
            && let Some(value) = values.into_iter().next()
        {
            let settings = crate::settings::Settings::from_value(&value);
            tracing::debug!(?settings, "initial configuration");
            self.state.lock().await.settings = settings;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!("shutdown requested");
        // Tell the stdio driver to terminate the process once this
        // response is flushed. The client sends `exit` right after a
        // successful `shutdown`, but tower-lsp 0.20 doesn't end its serve
        // loop on `exit` (only on stdin EOF), so we must drive the exit
        // ourselves or the process lingers across an editor restart.
        self.exit_signal.notify_one();
        Ok(())
    }

    async fn did_open(&self, params: lsp::DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;
        tracing::trace!(%uri, bytes = text.len(), "didOpen");
        {
            let mut ws = self.state.lock().await;
            ws.open(uri.clone(), text);
            ws.set_doc_version(&uri, version);
        }
        self.publish_diagnostics(uri).await;
    }

    async fn did_change(&self, params: lsp::DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if params.content_changes.is_empty() {
            return;
        }
        tracing::trace!(%uri, changes = params.content_changes.len(), "didChange");
        let version = params.text_document.version;
        {
            let mut ws = self.state.lock().await;
            // Incremental sync: fold the ranged edits into the current
            // buffer, then re-seed the salsa input once with the result.
            let Some(doc) = ws.doc(&uri) else {
                return;
            };
            let new_text = crate::util::edits::apply_content_changes(
                doc.text.to_string(),
                &params.content_changes,
            );
            ws.update(&uri, new_text);
            ws.set_doc_version(&uri, version);
        }
        self.publish_diagnostics(uri).await;
    }

    async fn did_close(&self, params: lsp::DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "didClose");
        {
            let mut ws = self.state.lock().await;
            ws.close(&uri);
        }
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn hover(&self, params: lsp::HoverParams) -> Result<Option<lsp::Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        tracing::trace!(%uri, line = pos.line, col = pos.character, "hover");
        let ws = self.state.lock().await;
        Ok(guard("hover", || hover::handle(&ws, &uri, pos)))
    }

    async fn goto_definition(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> Result<Option<lsp::GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        tracing::trace!(%uri, line = pos.line, col = pos.character, "definition");
        let ws = self.state.lock().await;
        Ok(guard("definition", || definition::handle(&ws, &uri, pos)))
    }

    async fn references(&self, params: lsp::ReferenceParams) -> Result<Option<Vec<lsp::Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_decl = params.context.include_declaration;
        let ws = self.state.lock().await;
        Ok(guard("references", || {
            references::handle(&ws, &uri, pos, include_decl)
        }))
    }

    async fn document_highlight(
        &self,
        params: lsp::DocumentHighlightParams,
    ) -> Result<Option<Vec<lsp::DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("document_highlight", || {
            document_highlight::handle(&ws, &uri, pos)
        }))
    }

    async fn signature_help(
        &self,
        params: lsp::SignatureHelpParams,
    ) -> Result<Option<lsp::SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        tracing::trace!(%uri, line = pos.line, col = pos.character, "signatureHelp");
        let ws = self.state.lock().await;
        Ok(guard("signature_help", || {
            signature_help::handle(&ws, &uri, pos)
        }))
    }

    async fn document_symbol(
        &self,
        params: lsp::DocumentSymbolParams,
    ) -> Result<Option<lsp::DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "documentSymbol");
        let ws = self.state.lock().await;
        Ok(guard("document_symbol", || symbols::handle(&ws, &uri)))
    }

    async fn symbol(
        &self,
        params: lsp::WorkspaceSymbolParams,
    ) -> Result<Option<Vec<lsp::SymbolInformation>>> {
        let ws = self.state.lock().await;
        Ok(guard("workspace_symbol", || {
            workspace_symbols::handle(&ws, &params.query)
        }))
    }

    async fn goto_declaration(
        &self,
        params: lsp::request::GotoDeclarationParams,
    ) -> Result<Option<lsp::request::GotoDeclarationResponse>> {
        // Same shape as goto-definition for Leekscript.
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("declaration", || definition::handle(&ws, &uri, pos)))
    }

    async fn goto_implementation(
        &self,
        params: lsp::request::GotoImplementationParams,
    ) -> Result<Option<lsp::request::GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("implementation", || {
            implementation::handle(&ws, &uri, pos)
        }))
    }

    async fn on_type_formatting(
        &self,
        params: lsp::DocumentOnTypeFormattingParams,
    ) -> Result<Option<Vec<lsp::TextEdit>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let ws = self.state.lock().await;
        Ok(guard("on_type_formatting", || {
            on_type_formatting::handle(&ws, &uri, pos, &params.ch)
        }))
    }

    async fn execute_command(
        &self,
        params: lsp::ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        let result = {
            let ws = self.state.lock().await;
            guard("execute_command", || {
                execute_command::handle(&ws, &params.command, &params.arguments)
            })
        };
        // A command that answers with a plain string (`leek.showComplexity`,
        // reached from its code lens) has nowhere to render: the editor
        // discards an `executeCommand` result it did not ask for. Push it
        // as a notification so the click actually shows something.
        if let Some(serde_json::Value::String(message)) = &result {
            self.client
                .show_message(lsp::MessageType::INFO, message)
                .await;
        }
        Ok(result)
    }

    async fn diagnostic(
        &self,
        params: lsp::DocumentDiagnosticParams,
    ) -> Result<lsp::DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri;
        let ws = self.state.lock().await;
        Ok(pull_diagnostics::handle_textdoc(&ws, &uri))
    }

    async fn workspace_diagnostic(
        &self,
        _params: lsp::WorkspaceDiagnosticParams,
    ) -> Result<lsp::WorkspaceDiagnosticReportResult> {
        let ws = self.state.lock().await;
        Ok(pull_diagnostics::handle_workspace(&ws))
    }

    async fn did_change_configuration(&self, params: lsp::DidChangeConfigurationParams) {
        // The client pushed updated settings — mirror its `leek` section
        // into the workspace so the formatter and inlay-hint handlers
        // pick up the new options.
        let settings = crate::settings::Settings::from_value(&params.settings);
        tracing::debug!(?settings, "didChangeConfiguration");
        self.state.lock().await.settings = settings;
        // Inlay hints are computed on demand, so a toggle only takes
        // effect once the editor re-requests them. Nudge it to do so.
        let _ = self.client.inlay_hint_refresh().await;
    }

    async fn goto_type_definition(
        &self,
        params: lsp::request::GotoTypeDefinitionParams,
    ) -> Result<Option<lsp::request::GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("type_definition", || {
            type_definition::handle(&ws, &uri, pos)
        }))
    }

    async fn selection_range(
        &self,
        params: lsp::SelectionRangeParams,
    ) -> Result<Option<Vec<lsp::SelectionRange>>> {
        let uri = params.text_document.uri;
        let positions = params.positions;
        let ws = self.state.lock().await;
        Ok(guard("selection_range", || {
            selection_range::handle(&ws, &uri, positions)
        }))
    }

    async fn document_link(
        &self,
        params: lsp::DocumentLinkParams,
    ) -> Result<Option<Vec<lsp::DocumentLink>>> {
        let uri = params.text_document.uri;
        let ws = self.state.lock().await;
        Ok(guard("document_link", || document_link::handle(&ws, &uri)))
    }

    async fn prepare_rename(
        &self,
        params: lsp::TextDocumentPositionParams,
    ) -> Result<Option<lsp::PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let pos = params.position;
        let ws = self.state.lock().await;
        guard_with("prepare_rename", Ok(None), || {
            prepare_rename::handle(&ws, &uri, pos)
        })
        .map_err(refusal_to_rpc_error)
    }

    async fn prepare_call_hierarchy(
        &self,
        params: lsp::CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<lsp::CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("prepare_call_hierarchy", || {
            call_hierarchy::prepare(&ws, &uri, pos)
        }))
    }

    async fn incoming_calls(
        &self,
        params: lsp::CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<lsp::CallHierarchyIncomingCall>>> {
        let uri = params.item.uri.clone();
        let item = params.item;
        let ws = self.state.lock().await;
        Ok(guard("incoming_calls", || {
            call_hierarchy::incoming(&ws, &uri, &item)
        }))
    }

    async fn outgoing_calls(
        &self,
        params: lsp::CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<lsp::CallHierarchyOutgoingCall>>> {
        let uri = params.item.uri.clone();
        let item = params.item;
        let ws = self.state.lock().await;
        Ok(guard("outgoing_calls", || {
            call_hierarchy::outgoing(&ws, &uri, &item)
        }))
    }

    async fn prepare_type_hierarchy(
        &self,
        params: lsp::TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<lsp::TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("prepare_type_hierarchy", || {
            type_hierarchy::prepare(&ws, &uri, pos)
        }))
    }

    async fn supertypes(
        &self,
        params: lsp::TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<lsp::TypeHierarchyItem>>> {
        let uri = params.item.uri.clone();
        let item = params.item;
        let ws = self.state.lock().await;
        Ok(guard("supertypes", || {
            type_hierarchy::supertypes(&ws, &uri, &item)
        }))
    }

    async fn subtypes(
        &self,
        params: lsp::TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<lsp::TypeHierarchyItem>>> {
        let uri = params.item.uri.clone();
        let item = params.item;
        let ws = self.state.lock().await;
        Ok(guard("subtypes", || {
            type_hierarchy::subtypes(&ws, &uri, &item)
        }))
    }

    async fn code_lens(&self, params: lsp::CodeLensParams) -> Result<Option<Vec<lsp::CodeLens>>> {
        let uri = params.text_document.uri;
        let ws = self.state.lock().await;
        Ok(guard("code_lens", || code_lens::handle(&ws, &uri)))
    }

    async fn code_lens_resolve(&self, params: lsp::CodeLens) -> Result<lsp::CodeLens> {
        let ws = self.state.lock().await;
        // On a panic — or on a lens we can no longer resolve, e.g. the
        // document changed underneath it — hand the lens back unchanged
        // rather than failing the request.
        let unresolved = params.clone();
        Ok(guard("code_lens_resolve", || code_lens::resolve(&ws, params)).unwrap_or(unresolved))
    }

    async fn document_color(
        &self,
        params: lsp::DocumentColorParams,
    ) -> Result<Vec<lsp::ColorInformation>> {
        let uri = params.text_document.uri;
        let ws = self.state.lock().await;
        Ok(guard("document_color", || {
            document_color::handle(&ws, &uri).unwrap_or_default()
        }))
    }

    async fn color_presentation(
        &self,
        params: lsp::ColorPresentationParams,
    ) -> Result<Vec<lsp::ColorPresentation>> {
        let ws = self.state.lock().await;
        Ok(guard("color_presentation", || {
            document_color::presentations(&ws, params.color, params.range)
        }))
    }

    async fn rename(&self, params: lsp::RenameParams) -> Result<Option<lsp::WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let ws = self.state.lock().await;
        guard_with("rename", Ok(None), || {
            rename::handle(&ws, &uri, pos, &params.new_name)
        })
        .map_err(refusal_to_rpc_error)
    }

    async fn folding_range(
        &self,
        params: lsp::FoldingRangeParams,
    ) -> Result<Option<Vec<lsp::FoldingRange>>> {
        let uri = params.text_document.uri;
        let ws = self.state.lock().await;
        Ok(guard("folding_range", || folding::handle(&ws, &uri)))
    }

    async fn formatting(
        &self,
        params: lsp::DocumentFormattingParams,
    ) -> Result<Option<Vec<lsp::TextEdit>>> {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "formatting");
        let ws = self.state.lock().await;
        Ok(guard("formatting", || formatting::handle(&ws, &uri)))
    }

    async fn range_formatting(
        &self,
        params: lsp::DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<lsp::TextEdit>>> {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "rangeFormatting");
        let ws = self.state.lock().await;
        Ok(guard("range_formatting", || {
            range_formatting::handle(&ws, &uri, params.range)
        }))
    }

    async fn code_action(
        &self,
        params: lsp::CodeActionParams,
    ) -> Result<Option<lsp::CodeActionResponse>> {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "codeAction");
        let ws = self.state.lock().await;
        Ok(guard("code_action", || {
            code_action::handle(&ws, &uri, params.range, &params.context)
        }))
    }

    async fn inlay_hint(
        &self,
        params: lsp::InlayHintParams,
    ) -> Result<Option<Vec<lsp::InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let ws = self.state.lock().await;
        Ok(guard("inlay_hint", || {
            inlay_hints::handle(&ws, &uri, range)
        }))
    }

    async fn completion(
        &self,
        params: lsp::CompletionParams,
    ) -> Result<Option<lsp::CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let ws = self.state.lock().await;
        Ok(guard("completion", || completion::handle(&ws, &uri, pos)))
    }

    async fn semantic_tokens_full(
        &self,
        params: lsp::SemanticTokensParams,
    ) -> Result<Option<lsp::SemanticTokensResult>> {
        let uri = params.text_document.uri;
        tracing::trace!(%uri, "semanticTokens/full");
        let mut ws = self.state.lock().await;
        Ok(guard("semantic_tokens_full", || {
            semantic_tokens::handle(&mut ws, &uri)
        }))
    }

    async fn semantic_tokens_range(
        &self,
        params: lsp::SemanticTokensRangeParams,
    ) -> Result<Option<lsp::SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let ws = self.state.lock().await;
        Ok(guard("semantic_tokens_range", || {
            semantic_tokens::handle_range(&ws, &uri, range)
        }))
    }

    async fn semantic_tokens_full_delta(
        &self,
        params: lsp::SemanticTokensDeltaParams,
    ) -> Result<Option<lsp::SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri;
        let prev = params.previous_result_id;
        let mut ws = self.state.lock().await;
        Ok(guard("semantic_tokens_full_delta", || {
            semantic_tokens::handle_delta(&mut ws, &uri, &prev)
        }))
    }

    async fn completion_resolve(&self, params: lsp::CompletionItem) -> Result<lsp::CompletionItem> {
        let ws = self.state.lock().await;
        // `guard` needs a `Default`; `CompletionItem` isn't, so on a
        // panic fall back to returning the item unchanged.
        let item = params.clone();
        Ok(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            completion::resolve(&ws, params)
        }))
        .unwrap_or(item))
    }

    async fn inlay_hint_resolve(&self, params: lsp::InlayHint) -> Result<lsp::InlayHint> {
        let fallback = params.clone();
        Ok(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inlay_hints::resolve(params)
        }))
        .unwrap_or(fallback))
    }

    async fn inline_value(
        &self,
        params: lsp::InlineValueParams,
    ) -> Result<Option<Vec<lsp::InlineValue>>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let stopped = params.context.stopped_location;
        let ws = self.state.lock().await;
        Ok(guard("inline_value", || {
            inline_values::handle(&ws, &uri, range, stopped)
        }))
    }

    async fn linked_editing_range(
        &self,
        params: lsp::LinkedEditingRangeParams,
    ) -> Result<Option<lsp::LinkedEditingRanges>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let ws = self.state.lock().await;
        Ok(guard("linked_editing_range", || {
            linked_editing::handle(&ws, &uri, pos)
        }))
    }

    async fn will_rename_files(
        &self,
        params: lsp::RenameFilesParams,
    ) -> Result<Option<lsp::WorkspaceEdit>> {
        let renames: Vec<(String, String)> = params
            .files
            .into_iter()
            .map(|f| (f.old_uri, f.new_uri))
            .collect();
        let ws = self.state.lock().await;
        Ok(guard("will_rename_files", || {
            file_operations::will_rename(&ws, &renames)
        }))
    }

    async fn did_rename_files(&self, params: lsp::RenameFilesParams) {
        let open = {
            let mut ws = self.state.lock().await;
            for f in &params.files {
                if let (Ok(old), Ok(new)) =
                    (lsp::Url::parse(&f.old_uri), lsp::Url::parse(&f.new_uri))
                {
                    tracing::info!(%old, %new, "didRename");
                    ws.rename_file(&old, &new);
                }
            }
            open_documents(&ws)
        };
        self.republish(open).await;
    }

    async fn did_change_watched_files(&self, params: lsp::DidChangeWatchedFilesParams) {
        let open = {
            let mut ws = self.state.lock().await;
            for change in params.changes {
                match change.typ {
                    lsp::FileChangeType::DELETED => {
                        tracing::info!(uri = %change.uri, "watched delete");
                        // Drops the project's copy of the file and keeps
                        // any open buffer for it editable.
                        ws.remove_from_disk(&change.uri);
                    }
                    lsp::FileChangeType::CREATED => {
                        // A new file appeared on disk; fold it into the
                        // root that owns it, if any.
                        ws.register_new_file(&change.uri);
                    }
                    _ => {
                        // CHANGED: refresh from disk unless the editor owns
                        // the buffer (then `didChange` is authoritative).
                        ws.reload_from_disk(&change.uri);
                    }
                }
            }
            open_documents(&ws)
        };
        self.republish(open).await;
    }
}

/// The `.leek`-files filter both file-operation registrations use.
fn leek_file_filter() -> lsp::FileOperationRegistrationOptions {
    lsp::FileOperationRegistrationOptions {
        filters: vec![lsp::FileOperationFilter {
            scheme: Some("file".into()),
            pattern: lsp::FileOperationPattern {
                glob: "**/*.leek".into(),
                matches: Some(lsp::FileOperationPatternKind::File),
                options: None,
            },
        }],
    }
}

/// URIs of every open buffer — the documents a disk-side event has to
/// republish diagnostics for.
fn open_documents(ws: &Workspace) -> Vec<lsp::Url> {
    ws.docs.keys().cloned().collect()
}

impl LeekLanguageServer {
    /// Republish diagnostics for a batch of documents after a disk-side
    /// event (a watched change, a rename) moved the class union or an
    /// include closure under them.
    ///
    /// Open buffers only, and once per batch rather than once per
    /// change: an indexed-only file has nothing published to refresh,
    /// and a branch switch can rewrite hundreds of them. Takes the URIs
    /// by value so the caller can drop the workspace lock first —
    /// `publish_diagnostics` takes it again itself.
    async fn republish(&self, uris: Vec<lsp::Url>) {
        for uri in uris {
            self.publish_diagnostics(uri).await;
        }
    }

    /// Run the pipeline through the lint target and publish the resulting
    /// diagnostics. Each `publish` replaces whatever was published
    /// before for this URI.
    ///
    /// This runs on every keystroke (`didOpen` / `didChange`) over the whole
    /// include closure, so it is the heaviest analysis the server performs —
    /// it shares the pull handler's panic-guarded collector so a crash while
    /// analysing a half-typed buffer degrades to "no diagnostics" instead of
    /// unwinding out of the notification and killing the server.
    async fn publish_diagnostics(&self, uri: lsp::Url) {
        let (diags, version) = {
            let ws = self.state.lock().await;
            let Some(doc) = ws.docs.get(&uri) else {
                return;
            };
            // Tag the publish with the version we're analyzing so the
            // client can drop these if a newer revision's diagnostics
            // have already landed (publishes can complete out of order).
            let doc_version = doc.version;
            let diags =
                pull_diagnostics::collect_guarded(&ws, &uri, "textDocument/publishDiagnostics");
            drop(ws);
            (diags, Some(doc_version))
        };
        tracing::trace!(
            %uri,
            items = diags.len(),
            version = version.unwrap_or(-1),
            "publishDiagnostics"
        );
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}
