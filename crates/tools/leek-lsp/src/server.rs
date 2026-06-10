//! tower-lsp [`LanguageServer`] implementation.

use std::sync::Arc;

use leek_recipes::Target;
use tokio::sync::{Mutex, Notify};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types as lsp;
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::to_lsp;
use crate::handlers::{
    call_hierarchy, code_action, code_lens, completion, definition, document_color,
    document_highlight, document_link, execute_command, file_operations, folding, formatting,
    hover, implementation, inlay_hints, inline_values, linked_editing, on_type_formatting,
    prepare_rename, pull_diagnostics, range_formatting, references, rename, selection_range,
    semantic_tokens, signature_help, symbols, type_definition, type_hierarchy, workspace_symbols,
};
use crate::workspace::Workspace;

/// Run a synchronous request-handler body, catching any panic so that one
/// malformed request (or a latent bug in analysis over an incomplete buffer)
/// can't take down the whole language server. The server is long-running and
/// processes untrusted buffers on every keystroke, so a panic must fail the
/// single request, not the process. On panic this returns the result type's
/// default (`None` / empty `Vec`), which clients treat as "no result".
///
/// `tokio::sync::Mutex` does not poison on panic, so a guard held across the
/// caught unwind is released cleanly when it drops.
fn guard<T: Default>(label: &str, f: impl FnOnce() -> T) -> T {
    let Ok(value) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) else {
        if crate::trace_enabled() {
            eprintln!("leek-lsp: handler `{label}` panicked; returning empty result");
        }
        return T::default();
    };
    value
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
        Self {
            client,
            state: Arc::new(Mutex::new(Workspace::default())),
            exit_signal,
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for LeekLanguageServer {
    async fn initialize(&self, params: lsp::InitializeParams) -> Result<lsp::InitializeResult> {
        eprintln!(
            "leek-lsp: initialize from client {:?} pid {:?}",
            params.client_info.as_ref().map(|c| format!(
                "{} {}",
                c.name,
                c.version.as_deref().unwrap_or("?")
            )),
            params.process_id
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
                for result in leek_recipes::load_register_and_report(&specs) {
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
                // Mirror to stderr immediately for terminal/log-file launches.
                for (_is_err, line) in &log {
                    eprintln!("{line}");
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
                    resolve_provider: Some(false),
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
                // part of the rename.
                workspace: Some(lsp::WorkspaceServerCapabilities {
                    workspace_folders: None,
                    file_operations: Some(lsp::WorkspaceFileOperationsServerCapabilities {
                        will_rename: Some(lsp::FileOperationRegistrationOptions {
                            filters: vec![lsp::FileOperationFilter {
                                scheme: Some("file".into()),
                                pattern: lsp::FileOperationPattern {
                                    glob: "**/*.leek".into(),
                                    matches: Some(lsp::FileOperationPatternKind::File),
                                    options: None,
                                },
                            }],
                        }),
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
        eprintln!("leek-lsp: initialized, ready");
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
            eprintln!("leek-lsp: file-watcher registration declined: {e}");
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
            if crate::trace_enabled() {
                eprintln!("leek-lsp: initial configuration -> {settings:?}");
            }
            self.state.lock().await.settings = settings;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        eprintln!("leek-lsp: shutdown requested");
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
        eprintln!("leek-lsp: didOpen {} ({} bytes)", uri, text.len());
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
        if crate::trace_enabled() {
            eprintln!(
                "leek-lsp: didChange {} ({} change(s))",
                uri,
                params.content_changes.len()
            );
        }
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
        eprintln!("leek-lsp: didClose {uri}");
        {
            let mut ws = self.state.lock().await;
            ws.close(&uri);
        }
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn hover(&self, params: lsp::HoverParams) -> Result<Option<lsp::Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        if crate::trace_enabled() {
            eprintln!("leek-lsp: hover {} {}:{}", uri, pos.line, pos.character);
        }
        let ws = self.state.lock().await;
        Ok(guard("hover", || hover::handle(&ws, &uri, pos)))
    }

    async fn goto_definition(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> Result<Option<lsp::GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        if crate::trace_enabled() {
            eprintln!(
                "leek-lsp: definition {} {}:{}",
                uri, pos.line, pos.character
            );
        }
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
        if crate::trace_enabled() {
            eprintln!(
                "leek-lsp: signatureHelp {} {}:{}",
                uri, pos.line, pos.character
            );
        }
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
        if crate::trace_enabled() {
            eprintln!("leek-lsp: documentSymbol {uri}");
        }
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
        let ws = self.state.lock().await;
        Ok(guard("execute_command", || {
            execute_command::handle(&ws, &params.command, &params.arguments)
        }))
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
        if crate::trace_enabled() {
            eprintln!("leek-lsp: didChangeConfiguration -> {settings:?}");
        }
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
        Ok(guard("prepare_rename", || {
            prepare_rename::handle(&ws, &uri, pos)
        }))
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
        Ok(guard("rename", || {
            rename::handle(&ws, &uri, pos, &params.new_name)
        }))
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
        if crate::trace_enabled() {
            eprintln!("leek-lsp: formatting {uri}");
        }
        let ws = self.state.lock().await;
        Ok(guard("formatting", || formatting::handle(&ws, &uri)))
    }

    async fn range_formatting(
        &self,
        params: lsp::DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<lsp::TextEdit>>> {
        let uri = params.text_document.uri;
        if crate::trace_enabled() {
            eprintln!("leek-lsp: rangeFormatting {uri}");
        }
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
        if crate::trace_enabled() {
            eprintln!("leek-lsp: codeAction {uri}");
        }
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
        if crate::trace_enabled() {
            eprintln!("leek-lsp: semanticTokens/full {uri}");
        }
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
        let mut ws = self.state.lock().await;
        for f in &params.files {
            if let (Ok(old), Ok(new)) = (lsp::Url::parse(&f.old_uri), lsp::Url::parse(&f.new_uri)) {
                eprintln!("leek-lsp: didRename {old} -> {new}");
                ws.rename_file(&old, &new);
            }
        }
    }

    async fn did_change_watched_files(&self, params: lsp::DidChangeWatchedFilesParams) {
        let mut ws = self.state.lock().await;
        for change in params.changes {
            match change.typ {
                lsp::FileChangeType::DELETED => {
                    eprintln!("leek-lsp: watched delete {}", change.uri);
                    ws.remove_file(&change.uri);
                }
                lsp::FileChangeType::CREATED => {
                    // A new file appeared on disk; fold it into the
                    // project index if we have one.
                    if let Ok(path) = change.uri.to_file_path()
                        && let Some(parent) = path.parent()
                    {
                        ws.index_project_at(parent);
                    }
                }
                _ => {
                    // CHANGED: refresh from disk unless the editor owns
                    // the buffer (then `didChange` is authoritative).
                    ws.reload_from_disk(&change.uri);
                }
            }
        }
    }
}

impl LeekLanguageServer {
    /// Run the pipeline through TypeCheck and publish the resulting
    /// diagnostics. Each `publish` replaces whatever was published
    /// before for this URI.
    async fn publish_diagnostics(&self, uri: lsp::Url) {
        let (diags, version) = {
            let ws = self.state.lock().await;
            let Some(doc) = ws.docs.get(&uri) else {
                return;
            };
            let line_table = doc.line_table.clone();
            let text = doc.text.clone();
            let source_file = doc.source_file;
            // Tag the publish with the version we're analyzing so the
            // client can drop these if a newer revision's diagnostics
            // have already landed (publishes can complete out of order).
            let doc_version = doc.version;
            // Recipe planning can fail (e.g. a malformed recipe params); degrade
            // to "no diagnostics" rather than crashing the server.
            // `Linted` is a superset of `TypeChecked` (it runs through type
            // checking + HIR + the lint pass), so the editor's problems panel
            // shows lint findings alongside parse/type errors.
            let lsp_diags: Vec<_> =
                if let Some(run) = crate::pipeline::run_on_file(&ws, source_file, Target::Linted) {
                    let pm = crate::util::position::PosMap::new(&line_table, &text);
                    run.diagnostics()
                        .iter()
                        .map(|d| to_lsp(d, pm, Some(&uri)))
                        .collect()
                } else {
                    if crate::trace_enabled() {
                        eprintln!("leek-lsp: recipe planning failed for {uri}; no diagnostics");
                    }
                    Vec::new()
                };
            drop(ws);
            (lsp_diags, Some(doc_version))
        };
        eprintln!(
            "leek-lsp: publishDiagnostics {} ({} items, v{})",
            uri,
            diags.len(),
            version.unwrap_or(-1),
        );
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}

#[cfg(test)]
mod tests {
    use super::guard;

    #[test]
    fn guard_returns_value_when_no_panic() {
        let r: Option<i32> = guard("ok", || Some(7));
        assert_eq!(r, Some(7));
    }

    #[test]
    fn guard_returns_default_on_panic() {
        // A panicking handler must degrade to the type default (`None` /
        // empty), not unwind out of the request and crash the server.
        // Silence the default panic hook so the expected panic isn't noisy.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let opt: Option<i32> = guard("boom", || panic!("kaboom"));
        let vec: Vec<i32> = guard("boom", || panic!("kaboom"));
        std::panic::set_hook(prev);
        assert_eq!(opt, None);
        assert!(vec.is_empty());
    }
}
